use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{net::SocketAddr, time::Instant};

use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
pub struct PasskeyReq {
    pub passkey: String,
}

#[derive(Serialize)]
struct TokenHeader {
    alg: String,
    typ: String,
}

#[derive(Serialize)]
struct TokenPayload {
    sub: String,
    iat: u64,
    exp: u64,
}

pub async fn verify_passkey_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(req): Json<PasskeyReq>,
) -> impl IntoResponse {
    let ip = addr.ip().to_string();

    // 1. Rate-limit check (5 attempts / 15 minutes)
    let window_duration = std::time::Duration::from_secs(15 * 60);
    let mut current_attempts = 0;
    
    if let Some(mut entry) = state.rate_limit_map.get_mut(&ip) {
        if entry.window_start.elapsed() > window_duration {
            entry.attempts = 1;
            entry.window_start = Instant::now();
            current_attempts = 1;
        } else {
            if entry.attempts >= 5 {
                tracing::warn!(ip = %ip, "Rate limit exceeded for passkey login");
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("Retry-After", "900")],
                    Json(serde_json::json!({"error": "Too many attempts. Try again later."})),
                )
                    .into_response();
            }
            entry.attempts += 1;
            current_attempts = entry.attempts;
        }
    } else {
        state.rate_limit_map.insert(
            ip.clone(),
            crate::RateLimitEntry {
                attempts: 1,
                window_start: Instant::now(),
            },
        );
        current_attempts = 1;
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
        tracing::warn!(ip = %ip, attempts = current_attempts, "Invalid passkey attempt");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Invalid passkey"})),
        )
            .into_response();
    }

    // Reset rate limit on success
    state.rate_limit_map.remove(&ip);

    // 3. Issue Token
    let auth_secret = crate::resolve_auth_secret().expect("AUTH_SECRET must be set");
    let now = shared_domain::now_ist().timestamp() as u64;
    let exp = now + 7 * 24 * 60 * 60; // 7 days

    let header = TokenHeader {
        alg: "HS256".to_string(),
        typ: "AT".to_string(),
    };
    let payload = TokenPayload {
        sub: "trader".to_string(),
        iat: now,
        exp,
    };

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&payload).unwrap());
    let msg = format!("{}.{}", header_b64, payload_b64);

    let mut mac = HmacSha256::new_from_slice(auth_secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(msg.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig);

    let token = format!("{}.{}", msg, sig_b64);

    tracing::info!(ip = %ip, "Successful passkey login");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "token": token })),
    ).into_response()
}
