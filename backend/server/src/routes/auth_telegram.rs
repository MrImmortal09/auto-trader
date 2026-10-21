use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::AppState;

#[derive(Deserialize)] pub struct TelegramRequestCodeReq {
    pub api_id: i32, pub api_hash: String, pub phone: String,
}
#[derive(Deserialize)] pub struct TelegramCodeReq { pub code: String }
#[derive(Deserialize)] pub struct Telegram2faReq   { pub password: String }
#[derive(Deserialize)] pub struct TelegramStartReq  { pub chat_ids: Vec<i64> }

macro_rules! bad_req {
    ($e:expr) => {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": $e.to_string()})))
            .into_response()
    };
}

/// `POST /api/auth/telegram/request-code`
pub async fn telegram_request_code_handler(
    State(state): State<AppState>,
    Json(req): Json<TelegramRequestCodeReq>,
) -> impl IntoResponse {
    let mut mgr = state.telegram.lock().await;
    match mgr.request_code(req.api_id, &req.api_hash, &req.phone).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": mgr.state}))).into_response(),
        Err(e) => bad_req!(e),
    }
}

/// `POST /api/auth/telegram/submit-code`
pub async fn telegram_submit_code_handler(
    State(state): State<AppState>,
    Json(req): Json<TelegramCodeReq>,
) -> impl IntoResponse {
    let mut mgr = state.telegram.lock().await;
    match mgr.submit_code(&req.code).await {
        Ok(done) => (StatusCode::OK, Json(serde_json::json!({
            "status": mgr.state,
            "twofa_required": !done,
        }))).into_response(),
        Err(e) => bad_req!(e),
    }
}

/// `POST /api/auth/telegram/submit-2fa`
pub async fn telegram_submit_2fa_handler(
    State(state): State<AppState>,
    Json(req): Json<Telegram2faReq>,
) -> impl IntoResponse {
    let mut mgr = state.telegram.lock().await;
    match mgr.submit_2fa(&req.password).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": mgr.state}))).into_response(),
        Err(e) => bad_req!(e),
    }
}

/// `GET /api/auth/telegram/status`
pub async fn telegram_status_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mgr = state.telegram.lock().await;
    Json(serde_json::json!({
        "state": mgr.state,
        "chat_ids": mgr.monitored_chats
    }))
}

/// `GET /api/auth/telegram/chats`
pub async fn telegram_chats_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.telegram.lock().await.list_dialogs().await {
        Ok(chats) => (StatusCode::OK, Json(serde_json::json!(chats))).into_response(),
        Err(e)    => bad_req!(e),
    }
}

/// `POST /api/auth/telegram/start`
pub async fn telegram_start_handler(
    State(state): State<AppState>,
    Json(req): Json<TelegramStartReq>,
) -> impl IntoResponse {
    let mut mgr = state.telegram.lock().await;
    match mgr
        .start_monitoring(
            req.chat_ids,
            state.signal_tx.clone(),
            Some(state.log_tx.clone()),
            state.flags.clone(),
            Some(state.db_tx.clone()),
        )
        .await
    {
        Ok(()) => {
            // Saved so the next restart reconnects to the same chats by itself.
            if let Some((api_id, api_hash)) = mgr.api_credentials() {
                crate::db::save_telegram_resume(&state.db_pool, api_id, &api_hash, &mgr.monitored_chats).await;
            }
            let _ = state.log_tx.send(r#"{"event":"TELEGRAM_STARTED"}"#.into());
            (StatusCode::OK, Json(serde_json::json!({"status": "running"}))).into_response()
        }
        Err(e) => bad_req!(e),
    }
}

/// `DELETE /api/auth/telegram/disconnect` — stop monitoring and clear the local session.
/// Resets TelegramManager to idle state (which stops its signal loop),
/// deletes the session JSON file and the saved reconnect details, so the next
/// restart does not reconnect either. Does NOT clear any frontend fields.
pub async fn disconnect_telegram(State(state): State<AppState>) -> impl IntoResponse {
    let mut mgr = state.telegram.lock().await;
    // Drop the client and all auth state by replacing with a fresh manager
    *mgr = telegram_ingester::TelegramManager::new();
    // Delete the persisted session file so it isn't re-used on next request-code call
    let _ = std::fs::remove_file("session.json");
    crate::db::clear_telegram_resume(&state.db_pool).await;
    tracing::info!("Telegram disconnected via /api/auth/telegram/disconnect");
    (StatusCode::OK, Json(serde_json::json!({"status": "disconnected"})))
}

/// Reconnect attempts before giving up, and the wait between them.
const RESUME_ATTEMPTS: u32 = 6;
const RESUME_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

/// Reconnect Telegram after a restart — no code, no password, no clicks —
/// from the login in `session.json` and the credentials and chats the last
/// Start saved (`db::load_telegram_resume`). Spawned once at startup.
///
/// Nothing posted while the server was down is replayed: the signal loop
/// streams with grammers' default `catch_up: false`, so an old call can
/// never be traded late. A login Telegram rejects is not retried (log in
/// from the dashboard); a timeout or network error is, up to
/// `RESUME_ATTEMPTS` times. A login started by hand meanwhile wins: the
/// retries stop as soon as the state is no longer `"resuming"`.
pub async fn resume_saved_telegram(state: AppState) {
    // The env-configured ingester (`TELEGRAM_API_ID`) already listens on its
    // own; a second loop would handle every message twice.
    if std::env::var("TELEGRAM_API_ID").is_ok() {
        return;
    }
    let Some(saved) = crate::db::load_telegram_resume(&state.db_pool).await else { return };
    if saved.chat_ids.is_empty() {
        return;
    }
    let log = |level: &'static str, payload: serde_json::Value| {
        let state = state.clone();
        async move { super::auth_kotak::send_log(&state.db_tx, &state.log_tx, level, payload).await }
    };
    if !telegram_ingester::session_file::has_saved_session() {
        log("WARN", serde_json::json!({
            "event": "TELEGRAM_RESUME_SKIPPED",
            "level": "WARN",
            "message": "No saved Telegram login (session.json) — log in to Telegram from the dashboard",
        })).await;
        return;
    }

    state.telegram.lock().await.state = "resuming".into();
    for attempt in 1..=RESUME_ATTEMPTS {
        let outcome = {
            let mut mgr = state.telegram.lock().await;
            if mgr.state != "resuming" {
                tracing::info!(state = %mgr.state, "[telegram] Login started by hand — auto-resume stands down");
                return;
            }
            let outcome = match mgr.resume(saved.api_id, &saved.api_hash).await {
                Ok(true) => mgr
                    .start_monitoring(
                        saved.chat_ids.clone(),
                        state.signal_tx.clone(),
                        Some(state.log_tx.clone()),
                        state.flags.clone(),
                        Some(state.db_tx.clone()),
                    )
                    .await
                    .map(|()| true),
                other => other,
            };
            if outcome.is_err() {
                mgr.state = "resuming".into();
            }
            outcome
        };
        match outcome {
            Ok(true) => {
                log("INFO", serde_json::json!({
                    "event": "TELEGRAM_RESUMED",
                    "level": "INFO",
                    "message": format!("Telegram reconnected after restart — monitoring {} chat(s)", saved.chat_ids.len()),
                })).await;
                let _ = state.log_tx.send(r#"{"event":"TELEGRAM_STARTED"}"#.into());
                return;
            }
            Ok(false) => {
                log("ERROR", serde_json::json!({
                    "event": "TELEGRAM_RESUME_FAILED",
                    "level": "ERROR",
                    "message": "Telegram no longer accepts the saved login — log in to Telegram from the dashboard; no signals are received until then",
                })).await;
                return;
            }
            Err(e) => {
                tracing::warn!(attempt, error = %e, "[telegram] Auto-resume attempt failed");
                if attempt < RESUME_ATTEMPTS {
                    tokio::time::sleep(RESUME_RETRY).await;
                }
            }
        }
    }
    {
        let mut mgr = state.telegram.lock().await;
        if mgr.state == "resuming" {
            mgr.state = "idle".into();
        }
    }
    log("ERROR", serde_json::json!({
        "event": "TELEGRAM_RESUME_FAILED",
        "level": "ERROR",
        "message": format!("Telegram did not reconnect after {RESUME_ATTEMPTS} attempts — log in to Telegram from the dashboard; no signals are received until then"),
    })).await;
}
