//! Hyperliquid LIVE's settings, the owner's alone (`hyperliquid::live`).
//! Behind the `hyperliquid` switch; none of these is named in the auth
//! policy, so all are the admin's.
//!
//! - `GET    /api/v2/hl/live` — the switches and the API wallets (never a key)
//! - `PUT    /api/v2/hl/live/settings` `{mode?, network?}`
//! - `PUT    /api/v2/hl/live/credentials` `{network, account, key}` — stored sealed, then checked
//! - `POST   /api/v2/hl/live/check` `{network}`
//! - `DELETE /api/v2/hl/live/credentials/:network`

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use hyperliquid_client::Network;
use serde::Deserialize;
use serde_json::json;

use crate::auth::Identity;
use crate::hyperliquid::live::HlMode;
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

pub async fn view_handler(State(state): State<AppState>) -> Response {
    if let Err(e) = switched_on(&state).await {
        return e;
    }
    Json(state.hl_live.view().await).into_response()
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
    if let Err(e) = switched_on(&state).await {
        return e;
    }
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
    let Some(owner) = identity.user_id() else {
        return refuse(StatusCode::UNAUTHORIZED, "sign in");
    };
    // Read once the switch is held: no paper order lands in between.
    match state.hl_live.set(mode, net, state.perps.flat(owner)).await {
        Ok(view) => {
            send_log(&state.db_tx, &state.log_tx, "WARN", json!({
                "event": "HL_MODE",
                "level": "WARN",
                "message": format!("Hyperliquid: {} on {}", if view.mode == HlMode::Live { "LIVE" } else { "Paper" }, view.network),
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

pub async fn credentials_handler(State(state): State<AppState>, Json(b): Json<CredentialBody>) -> Response {
    if let Err(e) = switched_on(&state).await {
        return e;
    }
    let net = match network(&b.network) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match state.hl_live.save_credential(net, &b.account, &b.key).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckBody {
    network: String,
}

pub async fn check_handler(State(state): State<AppState>, Json(b): Json<CheckBody>) -> Response {
    if let Err(e) = switched_on(&state).await {
        return e;
    }
    let net = match network(&b.network) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match state.hl_live.check(net).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}

pub async fn remove_handler(State(state): State<AppState>, Path(raw): Path<String>) -> Response {
    if let Err(e) = switched_on(&state).await {
        return e;
    }
    let net = match network(&raw) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match state.hl_live.remove_credential(net).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}
