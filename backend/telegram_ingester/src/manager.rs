use std::sync::Arc;
use std::time::Duration;

use grammers_client::sender::SenderPool;
use grammers_client::{Client, SignInError};
use grammers_session::storages::MemorySession;
use grammers_session::updates::UpdatesLike;
use shared_domain::TradeSignal;
use tokio::sync::{broadcast, mpsc};

use crate::session_file;
use crate::stream::run_event_loop;

/// Maximum time to wait for any single Telegram network call.
const TG_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A Telegram dialog entry returned by `TelegramManager::list_dialogs`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TelegramChat {
    pub id: i64,
    pub name: String,
    /// `"user"`, `"group"`, `"channel"`, or `"community"`.
    pub kind: String,
}

/// Errors from `TelegramManager` step operations.
#[derive(Debug, thiserror::Error)]
pub enum TelegramAuthError {
    #[error("wrong state for this operation (current: {0})")]
    WrongState(String),
    #[error("grammers error: {0}")]
    Grammers(#[from] grammers_client::InvocationError),
    #[error("sign-in error: {0}")]
    SignIn(String),
    #[error("session error: {0}")]
    Session(String),
    #[error("request timed out after 20 s — check API credentials and connectivity")]
    Timeout,
}

// ---------------------------------------------------------------------------
// TelegramManager
// ---------------------------------------------------------------------------

/// Step-by-step Telegram authentication + ingester manager.
///
/// Store as `Arc<tokio::sync::Mutex<TelegramManager>>` in Axum `AppState`.
///
/// # Auth flow
/// 1. `request_code(api_id, api_hash, phone)` — sends the Telegram code
/// 2. `submit_code(code)` → `true` if done, `false` if 2FA needed
/// 3. `submit_2fa(password)` — only when step 2 returned `false`
/// 4. `list_dialogs()` — returns all chats with names + IDs
/// 5. `start_monitoring(chat_ids, signal_tx)` — spawns the ingester loop
pub struct TelegramManager {
    client:         Option<Client>,
    session:        Option<Arc<MemorySession>>,
    updates_rx:     Option<mpsc::UnboundedReceiver<UpdatesLike>>,
    login_token:    Option<grammers_client::client::LoginToken>,
    pub password_token: Option<grammers_client::client::PasswordToken>,
    /// Human-readable state exposed by `/api/auth/telegram/status`.
    pub state: String,
    pub monitored_chats: Vec<i64>,
}

impl Default for TelegramManager {
    fn default() -> Self { Self::new() }
}

impl TelegramManager {
    pub fn new() -> Self {
        Self {
            client: None, session: None, updates_rx: None,
            login_token: None, password_token: None,
            state: "idle".into(),
            monitored_chats: Vec::new(),
        }
    }

    // ── Step 1 ──────────────────────────────────────────────────────────── //

    /// Load session from JSON file, connect, and request the SMS/app login
    /// code for `phone`.  If an existing valid session is found, skips to
    /// `"authenticated"`.
    pub async fn request_code(
        &mut self, api_id: i32, api_hash: &str, phone: &str,
    ) -> Result<(), TelegramAuthError> {
        self.client = None; self.session = None; self.updates_rx = None;
        self.login_token = None; self.password_token = None;

        let session = session_file::load_session();

        let pool   = SenderPool::new(Arc::clone(&session), api_id);
        let client = Client::new(pool.handle.clone());
        let updates_rx: mpsc::UnboundedReceiver<UpdatesLike> = pool.updates;
        let runner = pool.runner;

        tokio::spawn(async move {
            runner.run().await;
            tracing::warn!("[telegram] SenderPool runner exited");
        });

        if tokio::time::timeout(TG_TIMEOUT, client.is_authorized())
            .await
            .unwrap_or(Ok(false))
            .unwrap_or(false)
        {
            tracing::info!("[telegram] Existing session valid — skipping code request");
            self.client     = Some(client);
            self.session    = Some(session);
            self.updates_rx = Some(updates_rx);
            self.state      = "authenticated".into();
            return Ok(());
        }

        let login_token = tokio::time::timeout(
            TG_TIMEOUT,
            client.request_login_code(phone, api_hash),
        )
        .await
        .map_err(|_| TelegramAuthError::Timeout)?
        .map_err(TelegramAuthError::Grammers)?;

        self.client      = Some(client);
        self.session     = Some(session);
        self.updates_rx  = Some(updates_rx);
        self.login_token = Some(login_token);
        self.state       = "code_pending".into();
        Ok(())
    }

    // ── Step 2 ──────────────────────────────────────────────────────────── //

    /// Submit the code from the Telegram app.
    /// Returns `true` when fully authenticated, `false` when 2FA is required.
    pub async fn submit_code(&mut self, code: &str) -> Result<bool, TelegramAuthError> {
        let client = self.client.as_ref()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;
        let token  = self.login_token.as_ref()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;

        match tokio::time::timeout(TG_TIMEOUT, client.sign_in(token, code))
            .await
            .map_err(|_| TelegramAuthError::Timeout)?
        {
            Ok(_) => {
                self.login_token = None;
                self.state = "authenticated".into();
                self.persist_session().await;
                Ok(true)
            }
            Err(SignInError::PasswordRequired(pt)) => {
                self.login_token    = None;
                self.password_token = Some(pt);
                self.state          = "twofa_pending".into();
                Ok(false)
            }
            Err(e) => Err(TelegramAuthError::SignIn(e.to_string())),
        }
    }

    // ── Step 3 (optional) ───────────────────────────────────────────────── //

    /// Submit the 2FA password.
    pub async fn submit_2fa(&mut self, password: &str) -> Result<(), TelegramAuthError> {
        let client = self.client.as_ref()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;
        let pt = self.password_token.take()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;

        tokio::time::timeout(
            TG_TIMEOUT,
            client.check_password(pt, password.trim().as_bytes()),
        )
        .await
        .map_err(|_| TelegramAuthError::Timeout)?
        .map_err(|e| TelegramAuthError::SignIn(e.to_string()))?;

        self.state = "authenticated".into();
        self.persist_session().await;
        Ok(())
    }

    // ── Step 4 ──────────────────────────────────────────────────────────── //

    /// List all Telegram dialogs (groups, channels, DMs) with names + IDs.
    pub async fn list_dialogs(&self) -> Result<Vec<TelegramChat>, TelegramAuthError> {
        let client = self.client.as_ref()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;

        let mut iter  = client.iter_dialogs();
        let mut chats = Vec::new();

        while let Some(dialog) = tokio::time::timeout(
            TG_TIMEOUT,
            iter.next(),
        )
        .await
        .map_err(|_| TelegramAuthError::Timeout)?
        .map_err(TelegramAuthError::Grammers)? {
            use grammers_client::peer::Peer;
            let (id, name, kind) = match dialog.peer() {
                Peer::User(u) => {
                    let id = u.id().bot_api_dialog_id_unchecked();
                    let name = match (u.first_name(), u.last_name()) {
                        (Some(f), Some(l)) => format!("{f} {l}"),
                        (Some(f), None)    => f.to_owned(),
                        _ => u.username().map(str::to_owned)
                                .unwrap_or_else(|| format!("User {id}")),
                    };
                    (id, name, "user".to_owned())
                }
                Peer::Group(g) => (
                    g.id().bot_api_dialog_id_unchecked(),
                    g.title().unwrap_or("(group)").to_owned(),
                    "group".to_owned(),
                ),
                Peer::Channel(c) => (
                    c.id().bot_api_dialog_id_unchecked(),
                    c.title().to_owned(),
                    "channel".to_owned(),
                ),
                Peer::Community(c) => (
                    c.id().bot_api_dialog_id_unchecked(),
                    c.title().to_owned(),
                    "community".to_owned(),
                ),
            };
            chats.push(TelegramChat { id, name, kind });
        }

        Ok(chats)
    }

    // ── Step 5 ──────────────────────────────────────────────────────────── //

    /// Spawn the signal-monitoring loop for `chat_ids`.
    /// Clones the `Client` so `list_dialogs` keeps working; consumes `updates_rx`.
    pub async fn start_monitoring(
        &mut self,
        chat_ids: Vec<i64>,
        signal_tx: broadcast::Sender<TradeSignal>,
        log_tx: Option<broadcast::Sender<String>>,
    ) -> Result<(), TelegramAuthError> {
        let client = self.client.as_ref()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?
            .clone();
        let updates_rx = self.updates_rx.take()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;

        self.monitored_chats = chat_ids.clone();
        tokio::spawn(run_event_loop(client, updates_rx, chat_ids, signal_tx, log_tx));
        self.state = "running".into();
        Ok(())
    }

    // ── Helpers ─────────────────────────────────────────────────────────── //

    /// Persist the current in-memory session to the JSON file.
    async fn persist_session(&self) {
        if let Some(session) = &self.session {
            session_file::save_session(session).await;
        }
    }
}
