use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::auth::{ip::client_ip, token, token::TokenPayload, Identity};
use crate::AppState;

#[derive(Deserialize)]
pub struct PasskeyReq {
    pub passkey: String,
}

#[derive(Serialize)]
pub struct SessionStatusResponse {
    pub authenticated: bool,
    /// `write` (the passkey), `admin` or `user` (a phone sign-in), or `read`.
    pub role: &'static str,
}

/// `GET /api/auth/session` — whether the caller is signed in, and as what.
pub async fn session_status_handler(Extension(identity): Extension<Identity>) -> impl IntoResponse {
    let (authenticated, role) = match identity {
        Identity::Owner => (true, "write"),
        Identity::Admin { .. } => (true, "admin"),
        Identity::User { .. } => (true, "user"),
        Identity::Anonymous => (false, "read"),
    };
    Json(SessionStatusResponse { authenticated, role })
}

pub async fn verify_passkey_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasskeyReq>,
) -> impl IntoResponse {
    // Behind nginx every peer is 127.0.0.1: the address is the last
    // X-Forwarded-For hop, the one nginx itself appended (`auth::ip`).
    let ip = client_ip(addr, &headers);

    // 1. Rate-limit check (5 attempts / 15 minutes per address)
    let limit_key = format!("passkey:{ip}");
    if let Err(retry_after) = state.limits.check(&limit_key, 5, Duration::from_secs(15 * 60)) {
        tracing::warn!(ip = %ip, "Rate limit exceeded for passkey login");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry_after.to_string())],
            Json(serde_json::json!({"error": "Too many attempts. Try again later."})),
        )
            .into_response();
    }

    // 2. Verify passkey (support both runtime and compile-time env vars)
    let env_passkey = std::env::var("PASSKEY")
        .or_else(|_| option_env!("PASSKEY").map(String::from).ok_or("not set"))
        .expect("PASSKEY must be set");

    // Constant-time comparison (check length first to prevent panic)
    let is_valid: bool = if req.passkey.len() == env_passkey.len() {
        subtle::ConstantTimeEq::ct_eq(req.passkey.as_bytes(), env_passkey.as_bytes()).into()
    } else {
        tracing::warn!(
            ip = %ip,
            req_len = req.passkey.len(),
            env_len = env_passkey.len(),
            "Passkey length mismatch"
        );
        false
    };

    if !is_valid {
        tracing::warn!(ip = %ip, "Invalid passkey attempt");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Invalid passkey"})),
        )
            .into_response();
    }

    // Reset rate limit on success
    state.limits.clear(&limit_key);

    // 3. Issue the owner's token — see `auth::token`.
    let auth_secret = crate::resolve_auth_secret().expect("AUTH_SECRET must be set");
    let now = shared_domain::now_ist().timestamp() as u64;
    let signed = token::sign(&TokenPayload::owner(now), &auth_secret);

    tracing::info!(ip = %ip, "Successful passkey login");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "token": signed })),
    ).into_response()
}
