use std::io::{self, BufRead, Write};
use std::sync::Arc;

use grammers_client::sender::{SenderPool, SenderPoolRunner};
use grammers_client::update::Update;
use grammers_client::{Client, SignInError};
use grammers_session::updates::UpdatesLike;
use shared_domain::TradeSignal;
use tokio::sync::{broadcast, mpsc};

use crate::parser::parse_signal;
use crate::session_file;

// ---------------------------------------------------------------------------
// Stdin prompt (blocking — only called during first-time login)
// ---------------------------------------------------------------------------

pub(crate) fn prompt(label: &str) -> String {
    print!("{label}");
    io::stdout().flush().expect("stdout flush");
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).expect("stdin read");
    line.trim().to_owned()
}

// ---------------------------------------------------------------------------
// Shared event loop
// ---------------------------------------------------------------------------

/// Inner update loop — runs until the stream closes or the channel is dropped.
/// The caller is responsible for authentication; the client must already be
/// authorised before passing it here.
pub(crate) async fn run_event_loop(
    client: Client,
    updates_rx: mpsc::UnboundedReceiver<UpdatesLike>,
    target_chat_ids: Vec<i64>,
    tx: broadcast::Sender<TradeSignal>,
    log_tx: Option<broadcast::Sender<String>>,
) {
    let mut stream = match client.stream_updates(updates_rx, Default::default()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("[telegram] stream_updates failed: {e}");
            return;
        }
    };

    tracing::info!(chats = target_chat_ids.len(), "[telegram] Event loop running");

    loop {
        let update = match stream.next().await {
            Ok(u) => u,
            Err(e) => { tracing::warn!("[telegram] Stream closed: {e}"); break; }
        };

        let (msg, is_edit) = match update {
            Update::NewMessage(m) => (m, false),
            Update::MessageEdited(m) => (m, true),
            _ => continue,
        };

        let chat_id: i64 = msg.peer_id().bot_api_dialog_id_unchecked();
        if !target_chat_ids.contains(&chat_id) { continue; }

        let text = msg.text();
        if text.is_empty() { continue; }

        if let Some(ref ltx) = log_tx {
            let log_msg = serde_json::json!({
                "event": if is_edit { "TELEGRAM_MESSAGE_EDITED" } else { "TELEGRAM_MESSAGE" },
                "chat_id": chat_id,
                "msg_id": msg.id(),
                "text": text,
            });
            let _ = ltx.send(log_msg.to_string());
        }

        let msg_id_str = msg.id().to_string();
        if let Some(signal) = parse_signal(text, "telegram", Some(msg_id_str)) {
            tracing::info!(
                instrument = %signal.instrument_name,
                action = %signal.action,
                "Signal parsed — broadcasting"
            );
            if tx.send(signal).is_err() {
                tracing::warn!("[telegram] No receivers — signal dropped");
            }
        }
    }

    tracing::info!("[telegram] Event loop exited");
}

// ---------------------------------------------------------------------------
// Standalone ingester (stdin-based login)
// ---------------------------------------------------------------------------

/// Connect to Telegram as a user account and forward parsed signals to `tx`.
///
/// Uses `MemorySession` with JSON file persistence to avoid the `libsql` vs
/// `sqlx` threading conflict.  If `session.json` has no auth key, prompts
/// interactively via stdin for the phone number and login code (2FA handled
/// automatically).
pub async fn start_ingester_loop(
    api_id: i32,
    api_hash: String,
    target_chat_ids: Vec<i64>,
    tx: broadcast::Sender<TradeSignal>,
    log_tx: Option<broadcast::Sender<String>>,
) {
    let session = session_file::load_session();

    let pool = SenderPool::new(Arc::clone(&session), api_id);
    let client = Client::new(pool.handle.clone());
    let updates_rx: mpsc::UnboundedReceiver<UpdatesLike> = pool.updates;
    let runner: SenderPoolRunner = pool.runner;

    tokio::spawn(async move {
        runner.run().await;
        tracing::warn!("[telegram] SenderPool runner exited");
    });

    match client.is_authorized().await {
        Ok(true) => tracing::info!("[telegram] Existing session valid"),
        _ => {
            let phone = prompt("Telegram phone number (+91...): ");
            let token = client
                .request_login_code(&phone, &api_hash)
                .await
                .expect("request_login_code failed");
            let code = prompt("Login code: ");
            match client.sign_in(&token, &code).await {
                Ok(u) => tracing::info!(name = u.first_name().unwrap_or("?"), "Signed in"),
                Err(SignInError::PasswordRequired(pt)) => {
                    let pw = prompt("2FA password: ");
                    client.check_password(pt, pw.trim().as_bytes())
                        .await.expect("2FA failed");
                    tracing::info!("Signed in (2FA)");
                }
                Err(SignInError::SignUpRequired) => panic!("Phone not registered"),
                Err(e) => panic!("Sign-in error: {e}"),
            }
            // Persist session after successful login.
            session_file::save_session(&session).await;
        }
    }

    run_event_loop(client, updates_rx, target_chat_ids, tx, log_tx).await;
}
