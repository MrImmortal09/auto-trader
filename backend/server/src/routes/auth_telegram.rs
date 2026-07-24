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
    match mgr.start_monitoring(req.chat_ids, state.signal_tx.clone(), Some(state.log_tx.clone())).await {
        Ok(()) => {
            let _ = state.log_tx.send(r#"{"event":"TELEGRAM_STARTED"}"#.into());
            (StatusCode::OK, Json(serde_json::json!({"status": "running"}))).into_response()
        }
        Err(e) => bad_req!(e),
    }
}

/// `DELETE /api/auth/telegram/disconnect` — stop monitoring and clear the local session.
/// Resets TelegramManager to idle state and deletes the session JSON file.
/// Does NOT clear any frontend fields.
pub async fn disconnect_telegram(State(state): State<AppState>) -> impl IntoResponse {
    let mut mgr = state.telegram.lock().await;
    // Drop the client and all auth state by replacing with a fresh manager
    *mgr = telegram_ingester::TelegramManager::new();
    // Delete the persisted session file so it isn't re-used on next request-code call
    let _ = std::fs::remove_file("session.json");
    tracing::info!("Telegram disconnected via /api/auth/telegram/disconnect");
    (StatusCode::OK, Json(serde_json::json!({"status": "disconnected"})))
}
