//! `GET /api/admin/users/:id/hl` — a user's Hyperliquid LIVE desk, for the
//! admin to see, read-only (the owner's pick, 2026-11-21): their switches,
//! their API wallets (never a key) and, while LIVE, their account as
//! Hyperliquid holds it and their day. Nothing here can trade for them.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;

use crate::hyperliquid::live::LiveView;
use crate::hyperliquid::paper::{AccountView, PerpToday};
use crate::AppState;

#[derive(Serialize)]
pub struct UserHl {
    pub live: LiveView,
    /// While LIVE: their account on Hyperliquid.
    pub account: Option<AccountView>,
    /// While LIVE: their day from IST midnight.
    pub today: Option<PerpToday>,
    /// Why the account or the day couldn't be read.
    pub error: Option<String>,
}

pub async fn user_hl_handler(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    if !state.flags.read().await.hyperliquid {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "Hyperliquid is switched off" }))).into_response();
    }
    if state.auth.users.get(id).is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    }
    let desk = match state.hl_live.desk(id).await {
        Ok(d) => d,
        Err(r) => return r.into_response(),
    };
    let live = desk.view().await;
    let (mut account, mut today, mut error) = (None, None, None);
    if let Some(net) = desk.live_network().await {
        match desk.account(net, None).await {
            Ok(a) => account = Some(a),
            Err(r) => error = Some(r.message),
        }
        match desk.today(net).await {
            Ok(t) => today = Some(t),
            Err(r) => error = error.or(Some(r.message)),
        }
    }
    Json(UserHl { live, account, today, error }).into_response()
}
