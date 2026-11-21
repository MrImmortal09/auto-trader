//! Hyperliquid LIVE's settings, each signed-in person's own
//! (`hyperliquid::live`): a user may save an API wallet and go LIVE once the
//! admin has switched LIVE on for them. Behind the `hyperliquid` switch.
//!
//! - `GET    /api/v2/hl/live` — the switches and the API wallets (never a key)
//! - `PUT    /api/v2/hl/live/settings` `{mode?, network?}`
//! - `PUT    /api/v2/hl/live/credentials` `{network, account, key}` — stored sealed, then checked;
//!   `account` may be empty: Hyperliquid says whose API wallet the key is
//! - `POST   /api/v2/hl/live/check` `{network}`
//! - `DELETE /api/v2/hl/live/credentials/:network`
//! - `POST   /api/v2/hl/live/kill` — their own kill switch: every Hyperliquid
//!   order cancelled and position closed (while LIVE), new LIVE orders refused
//! - `DELETE /api/v2/hl/live/kill` — reset it

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use hyperliquid_client::Network;
use serde::Deserialize;
use serde_json::json;

use std::sync::Arc;

use crate::auth::Identity;
use crate::hyperliquid::live::{HlMode, LiveDesk};
use crate::routes::auth_kotak::send_log;
use crate::AppState;

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

async fn switched_on(state: &AppState) -> Result<(), Response> {
    if state.flags.read().await.hyperliquid {
        Ok(())
    } else {
        Err(refuse(StatusCode::NOT_FOUND, "Hyperliquid is switched off"))
    }
}

fn network(raw: &str) -> Result<Network, Response> {
    Network::parse(raw).ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "network must be testnet or mainnet"))
}

/// The caller's own LIVE desk, with the switch on.
async fn mine(state: &AppState, identity: &Identity) -> Result<Arc<LiveDesk>, Response> {
    switched_on(state).await?;
    let id = identity.user_id().ok_or_else(|| refuse(StatusCode::UNAUTHORIZED, "sign in"))?;
    state.hl_live.desk(id).await.map_err(|r| refuse(r.status, r.message))
}

macro_rules! mine_or_return {
    ($state:expr, $identity:expr) => {
        match mine(&$state, &$identity).await {
            Ok(d) => d,
            Err(e) => return e,
        }
    };
}

pub async fn view_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let desk = mine_or_return!(state, identity);
    Json(desk.view().await).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsBody {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    network: Option<String>,
}

pub async fn settings_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<SettingsBody>) -> Response {
    let desk = mine_or_return!(state, identity);
    let mode = match b.mode.as_deref() {
        None => None,
        Some("paper") => Some(HlMode::Paper),
        Some("live") => Some(HlMode::Live),
        Some(_) => return refuse(StatusCode::BAD_REQUEST, "mode must be paper or live"),
    };
    let net = match b.network.as_deref().map(network) {
        None => None,
        Some(Ok(n)) => Some(n),
        Some(Err(e)) => return e,
    };
    // Read once the switch is held: no paper order lands in between.
    match desk.set(mode, net, state.perps.flat(desk.user_id)).await {
        Ok(view) => {
            // The engine log is the admin's: a user's switch is told there
            // too, by id, as the admin's own is.
            let who = if desk.admin { String::new() } else { format!(" for user {}", desk.user_id) };
            send_log(&state.db_tx, &state.log_tx, "WARN", json!({
                "event": "HL_MODE",
                "level": "WARN",
                "message": format!("Hyperliquid: {} on {}{who}", if view.mode == HlMode::Live { "LIVE" } else { "Paper" }, view.network),
            }))
            .await;
            Json(view).into_response()
        }
        Err(r) => refuse(r.status, r.message),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialBody {
    network: String,
    account: String,
    key: String,
}

impl std::fmt::Debug for CredentialBody {
    // Never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialBody").field("network", &self.network).field("account", &self.account).finish_non_exhaustive()
    }
}

pub async fn credentials_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<CredentialBody>) -> Response {
    let desk = mine_or_return!(state, identity);
    let net = match network(&b.network) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match desk.save_credential(net, &b.account, &b.key).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckBody {
    network: String,
}

pub async fn check_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<CheckBody>) -> Response {
    let desk = mine_or_return!(state, identity);
    let net = match network(&b.network) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match desk.check(net).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}

pub async fn remove_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(raw): Path<String>) -> Response {
    let desk = mine_or_return!(state, identity);
    let net = match network(&raw) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match desk.remove_credential(net).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}

/// `POST /api/v2/hl/live/kill` — the caller's own Hyperliquid kill switch.
pub async fn kill_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let desk = mine_or_return!(state, identity);
    if let Err(r) = desk.engage_kill().await {
        return refuse(r.status, r.message);
    }
    send_log(&state.db_tx, &state.log_tx, "WARN", json!({
        "event": "HL_KILL_SWITCH",
        "level": "WARN",
        "message": format!("Hyperliquid kill switch on{}", if desk.admin { String::new() } else { format!(" for user {}", desk.user_id) }),
    }))
    .await;
    Json(desk.view().await).into_response()
}

/// `DELETE /api/v2/hl/live/kill` — reset it.
pub async fn reset_kill_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let desk = mine_or_return!(state, identity);
    match desk.reset_kill().await {
        Ok(()) => Json(desk.view().await).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}
