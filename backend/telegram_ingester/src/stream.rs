use std::io::{self, BufRead, Write};
use std::sync::Arc;

use grammers_client::sender::{SenderPool, SenderPoolRunner};
use grammers_client::update::Update;
use grammers_client::{Client, SignInError};
use grammers_session::updates::UpdatesLike;
use shared_domain::telemetry::{self, ParseResult};
use shared_domain::{ist_offset, today_ist, DbWriteMessage, FeatureFlags, SignalChat, TradeSignal};
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::parser::{classify_with, Inbound};
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
///
/// `flags` are the live engine switches (`/api/v2/flags`), read once per
/// message; `db_tx` is where a call skipped under them is recorded.
pub(crate) async fn run_event_loop(
    client: Client,
    updates_rx: mpsc::UnboundedReceiver<UpdatesLike>,
    target_chat_ids: Vec<i64>,
    tx: broadcast::Sender<TradeSignal>,
    log_tx: Option<broadcast::Sender<String>>,
    flags: Arc<RwLock<FeatureFlags>>,
    db_tx: Option<mpsc::Sender<DbWriteMessage>>,
) {
    let mut stream = match client.stream_updates(updates_rx, Default::default()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("[telegram] stream_updates failed: {e}");
            return;
        }
    };

    tracing::info!(chats = target_chat_ids.len(), "[telegram] Event loop running");
    let _listening = telemetry::TelegramLoopGuard::enter();

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

        tracing::info!(chat_id, msg_id = msg.id(), "[telegram] Received message: {}", text);
        telemetry::telegram_message(is_edit);

        let msg_id_str = msg.id().to_string();
        let reply_to = msg.reply_to_message_id().map(|id| id.to_string());

        // Routing (and why its order matters) lives in `classify_with`, where
        // the tests can reach it.
        let flags_now = *flags.read().await;
        // Which chat it came from: kept on a call's position and trades, and
        // what a reply or an edit is matched within — message ids are only
        // unique per chat.
        let chat = SignalChat {
            id: chat_id,
            title: msg.peer().and_then(|p| p.name()).map(str::to_string),
        };
        let chat_title = chat.title.clone();
        // (how it was routed, the message a reply answers, a reply's price)
        let routed: (&'static str, Option<i64>, Option<f64>) = match classify_with(text, &msg_id_str, reply_to.as_deref(), today_ist(), flags_now) {
            Inbound::Signal(mut signal) => {
                signal.chat = Some(chat);
                tracing::info!(
                    instrument = %signal.instrument_name,
                    action = %signal.action,
                    "Signal parsed — broadcasting"
                );
                telemetry::telegram_parsed(ParseResult::Signal);
                if tx.send(signal).is_err() {
                    tracing::warn!("[telegram] No receivers — signal dropped");
                }
                ("signal", None, None)
            }
            Inbound::NotIndexOption(signal) => {
                tracing::info!(
                    instrument = %signal.instrument_name,
                    "[telegram] Call skipped — not an index option (index_options_only is on)"
                );
                let first_line = text.lines().next().unwrap_or_default();
                let payload = serde_json::json!({
                    "event": "SIGNAL_SKIPPED",
                    "level": "INFO",
                    "reason": "not_index_option",
                    "source": "telegram",
                    "message": format!("Skipped: not an index option — {first_line}"),
                    "instrument": signal.instrument_name,
                    "chat_id": chat_id,
                    "msg_id": msg.id(),
                    "edited": is_edit,
                    "text": text,
                });
                record_log(log_tx.as_ref(), db_tx.as_ref(), "INFO", payload).await;
                // Not traded, like an unreadable call — the System screen's
                // counters stay as they were.
                telemetry::telegram_parsed(ParseResult::Unparsed);
                ("not_index_option", None, None)
            }
            Inbound::IgnoredCall => {
                tracing::warn!("[telegram] BUY call not traded — no instrument or unsupported expiry/entry: {text}");
                if let Some(ref ltx) = log_tx {
                    let first_line = text.lines().next().unwrap_or_default();
                    let log_msg = serde_json::json!({
                        "event": "SIGNAL_IGNORED",
                        "level": "WARN",
                        "message": format!("BUY call not traded — no instrument, or an unsupported expiry/entry: {first_line}"),
                    });
                    let _ = ltx.send(log_msg.to_string());
                }
                telemetry::telegram_parsed(ParseResult::Unparsed);
                ("ignored_call", None, None)
            }
            Inbound::SlUpdate { parent, new_sl } => {
                let answers = parent.parse().ok();
                let signal = reply_update(text, parent, chat, "UPDATE_SL", 0.0, new_sl);
                tracing::info!(new_sl, "Reply parsed as SL update — broadcasting");
                telemetry::telegram_parsed(ParseResult::SlUpdate);
                if tx.send(signal).is_err() {
                    tracing::warn!("[telegram] No receivers — update dropped");
                }
                ("sl_update", answers, Some(new_sl))
            }
            Inbound::Exit { parent, price } => {
                let answers = parent.parse().ok();
                let signal = reply_update(text, parent, chat, "EXIT_AT", price, 0.0);
                tracing::info!(exit_price = price, "Reply parsed as EXIT_AT — broadcasting");
                telemetry::telegram_parsed(ParseResult::Exit);
                if tx.send(signal).is_err() {
                    tracing::warn!("[telegram] No receivers — update dropped");
                }
                ("exit", answers, Some(price))
            }
            Inbound::Unparsed => {
                telemetry::telegram_parsed(ParseResult::Unparsed);
                ("unparsed", None, None)
            }
        };

        // Kept for the v2 Desk's feed only after the engine has it, so
        // storing a message never holds up a signal.
        if let Some(db_tx) = db_tx.as_ref() {
            let (kind, reply_to, value) = routed;
            let _ = db_tx
                .send(DbWriteMessage::TelegramMessage {
                    chat_id,
                    chat_title,
                    msg_id: i64::from(msg.id()),
                    sent_at: msg.date().with_timezone(&ist_offset()).format("%Y-%m-%d %H:%M:%S").to_string(),
                    text: text.to_owned(),
                    kind,
                    reply_to,
                    value,
                    edited: is_edit,
                })
                .await;
        }
    }

    tracing::info!("[telegram] Event loop exited");
}

/// Send `payload` to the live log stream and, when there is one, to the
/// durable `system_logs` table, so it survives a restart and a page reload.
async fn record_log(
    log_tx: Option<&broadcast::Sender<String>>,
    db_tx: Option<&mpsc::Sender<DbWriteMessage>>,
    level: &str,
    payload: serde_json::Value,
) {
    let message = payload.to_string();
    if let Some(db_tx) = db_tx {
        let _ = db_tx
            .send(DbWriteMessage::Log { level: level.to_owned(), message: message.clone() })
            .await;
    }
    if let Some(log_tx) = log_tx {
        let _ = log_tx.send(message);
    }
}

/// The bare `UPDATE_SL` / `EXIT_AT` signal the monitor matches to the position
/// opened from message `parent` in `chat`.
fn reply_update(text: &str, parent: String, chat: SignalChat, action: &str, entry_price: f64, stop_loss: f64) -> TradeSignal {
    TradeSignal {
        instrument_name: "UPDATE".to_string(),
        strike: None,
        option_type: None,
        expiry: None,
        action: action.to_string(),
        entry_condition: "ABOVE".to_string(),
        entry_price,
        entry_price_max: None,
        targets: vec![],
        stop_loss,
        source: "telegram_reply".to_string(),
        signal_id: Some(parent),
        raw_message: Some(text.to_owned()),
        chat: Some(chat),
    }
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
    flags: Arc<RwLock<FeatureFlags>>,
    db_tx: Option<mpsc::Sender<DbWriteMessage>>,
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

    run_event_loop(client, updates_rx, target_chat_ids, tx, log_tx, flags, db_tx).await;
}
