use std::sync::Arc;
use std::time::Duration;

use grammers_client::sender::SenderPool;
use grammers_client::{Client, SignInError};
use grammers_session::storages::MemorySession;
use grammers_session::updates::UpdatesLike;
use shared_domain::{DbWriteMessage, FeatureFlags, TradeSignal};
use tokio::sync::{broadcast, mpsc, RwLock};

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
///
/// After a restart, `resume(api_id, api_hash)` replaces steps 1–3 when
/// `session.json` still holds a valid login.
pub struct TelegramManager {
    client:         Option<Client>,
    session:        Option<Arc<MemorySession>>,
    updates_rx:     Option<mpsc::UnboundedReceiver<UpdatesLike>>,
    login_token:    Option<grammers_client::client::LoginToken>,
    pub password_token: Option<grammers_client::client::PasswordToken>,
    /// Human-readable state exposed by `/api/auth/telegram/status`.
    pub state: String,
    pub monitored_chats: Vec<i64>,
    /// API credentials of the last `request_code` / `resume`, so the server
    /// can save them for reconnecting after a restart.
    api_id:   Option<i32>,
    api_hash: Option<String>,
    /// The running signal loop. It holds its own clone of the client, so
    /// dropping the manager alone would leave it processing messages next to
    /// any new one — it is aborted on drop instead.
    event_loop: Option<tokio::task::JoinHandle<()>>,
}

impl Default for TelegramManager {
    fn default() -> Self { Self::new() }
}

impl Drop for TelegramManager {
    fn drop(&mut self) {
        if let Some(handle) = self.event_loop.take() {
            handle.abort();
        }
    }
}

impl TelegramManager {
    pub fn new() -> Self {
        Self {
            client: None, session: None, updates_rx: None,
            login_token: None, password_token: None,
            state: "idle".into(),
            monitored_chats: Vec::new(),
            api_id: None, api_hash: None,
            event_loop: None,
        }
    }

    /// True while the signal loop started by `start_monitoring` is running.
    pub fn is_listening(&self) -> bool {
        self.event_loop.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// `(api_id, api_hash)` of the last `request_code` / `resume`.
    pub fn api_credentials(&self) -> Option<(i32, String)> {
        Some((self.api_id?, self.api_hash.clone()?))
    }

    /// Drop any half-finished login and open a fresh connection from
    /// `session.json`. Refused while a signal loop is running: a second
    /// login would start a second loop and every message would be handled
    /// twice.
    fn connect(
        &mut self, api_id: i32, api_hash: &str,
    ) -> Result<(Client, Arc<MemorySession>, mpsc::UnboundedReceiver<UpdatesLike>), TelegramAuthError> {
        if self.is_listening() {
            return Err(TelegramAuthError::WrongState(self.state.clone()));
        }
        self.client = None; self.session = None; self.updates_rx = None;
        self.login_token = None; self.password_token = None;
        self.api_id = Some(api_id);
        self.api_hash = Some(api_hash.to_owned());

        let session = session_file::load_session();

        let pool   = SenderPool::new(Arc::clone(&session), api_id);
        let client = Client::new(pool.handle.clone());
        let updates_rx: mpsc::UnboundedReceiver<UpdatesLike> = pool.updates;
        let runner = pool.runner;

        // Exits by itself once every client handle is dropped.
        tokio::spawn(async move {
            runner.run().await;
            tracing::warn!("[telegram] SenderPool runner exited");
        });
        Ok((client, session, updates_rx))
    }

    // ── Resume ──────────────────────────────────────────────────────────── //

    /// Reconnect with the login saved in `session.json` — no code, no
    /// password — e.g. after a server restart. `Ok(true)`: logged in, ready
    /// for `start_monitoring`. `Ok(false)`: Telegram no longer accepts the
    /// saved login (logged out elsewhere, revoked), so a fresh login is
    /// needed. `Err`: couldn't tell — a timeout or network error, worth
    /// retrying.
    pub async fn resume(&mut self, api_id: i32, api_hash: &str) -> Result<bool, TelegramAuthError> {
        let (client, session, updates_rx) = self.connect(api_id, api_hash)?;
        let authorized = tokio::time::timeout(TG_TIMEOUT, client.is_authorized())
            .await
            .map_err(|_| TelegramAuthError::Timeout)?
            .map_err(TelegramAuthError::Grammers)?;
        if !authorized {
            self.state = "idle".into();
            return Ok(false);
        }
        tracing::info!("[telegram] Saved session still valid — resumed without a login code");
        self.client     = Some(client);
        self.session    = Some(session);
        self.updates_rx = Some(updates_rx);
        self.state      = "authenticated".into();
        Ok(true)
    }

    // ── Step 1 ──────────────────────────────────────────────────────────── //

    /// Load session from JSON file, connect, and request the SMS/app login
    /// code for `phone`.  If an existing valid session is found, skips to
    /// `"authenticated"`.
    pub async fn request_code(
        &mut self, api_id: i32, api_hash: &str, phone: &str,
    ) -> Result<(), TelegramAuthError> {
        let (client, session, updates_rx) = self.connect(api_id, api_hash)?;

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
    /// `flags` and `db_tx` are as for the loop itself (see `run_event_loop`).
    pub async fn start_monitoring(
        &mut self,
        chat_ids: Vec<i64>,
        signal_tx: broadcast::Sender<TradeSignal>,
        log_tx: Option<broadcast::Sender<String>>,
        flags: Arc<RwLock<FeatureFlags>>,
        db_tx: Option<mpsc::Sender<DbWriteMessage>>,
    ) -> Result<(), TelegramAuthError> {
        let client = self.client.as_ref()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?
            .clone();
        let updates_rx = self.updates_rx.take()
            .ok_or_else(|| TelegramAuthError::WrongState(self.state.clone()))?;

        self.monitored_chats = chat_ids.clone();
        self.event_loop = Some(tokio::spawn(run_event_loop(
            client, updates_rx, chat_ids, signal_tx, log_tx, flags, db_tx,
        )));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_the_manager_stops_its_signal_loop() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut mgr = TelegramManager::new();
        assert!(!mgr.is_listening());
        assert!(mgr.api_credentials().is_none());
        // Stands in for `run_event_loop`: runs until aborted.
        mgr.event_loop = Some(tokio::spawn(async move {
            let _alive = tx;
            std::future::pending::<()>().await;
        }));
        assert!(mgr.is_listening());
        // A second login while listening would start a second loop.
        assert!(matches!(mgr.connect(1, "hash"), Err(TelegramAuthError::WrongState(_))));

        drop(mgr); // what Disconnect does, by replacing the manager
        assert!(rx.await.is_err(), "the loop was aborted, dropping its sender");
    }
}
