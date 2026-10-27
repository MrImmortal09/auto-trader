//! Phone sign-in: `POST /api/auth/otp/request`, `POST /api/auth/otp/verify`
//! and `GET /api/auth/me`. Only an allow-listed, enabled phone gets a code —
//! nothing is sent otherwise — and a right code is the whole "registration".
//!
//! The admin's code is also written to [`ADMIN_OTP_FILE`] on the server, so
//! the owner can still get in when WhatsApp doesn't deliver.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, Duration, FixedOffset};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{ip::client_ip, otp, token, Identity, Role, UserRecord};
use crate::AppState;

const NOT_ENABLED: &str = "This number isn't enabled. Ask the admin to add it.";

/// Where the admin's latest sign-in code is written, in the server's working
/// directory (beside `trades.db`). Only the admin's code, never a user's;
/// readable by the server's own user alone; removed once the admin signs in.
pub const ADMIN_OTP_FILE: &str = "admin-otp.log";

/// Write the admin's `code` to [`ADMIN_OTP_FILE`], replacing the last one.
fn write_admin_code(phone: &str, code: &str, now: DateTime<FixedOffset>, expires_at: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(ADMIN_OTP_FILE)?;
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    writeln!(file, "{} IST  admin sign-in code {code} for {}  valid once, until {expires_at} IST", ist(now), masked(phone))
}

#[derive(Deserialize)]
pub struct OtpRequest {
    pub phone: String,
}

#[derive(Deserialize)]
pub struct OtpVerify {
    pub phone: String,
    pub code: String,
}

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn too_many(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("Retry-After", retry_after_secs.to_string())],
        Json(json!({ "error": "Too many attempts. Try again later." })),
    )
        .into_response()
}

fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("phone sign-in: {what}: {e}");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("could not {what}"))
}

fn ist(t: DateTime<FixedOffset>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `7000000001` → `70••••0001`, for the log.
pub(crate) fn masked(phone: &str) -> String {
    if phone.len() < 6 {
        return "••••".to_string();
    }
    format!("{}••••{}", &phone[..2], &phone[phone.len() - 4..])
}

/// A user as the frontend sees it.
pub(crate) fn user_json(u: &UserRecord) -> serde_json::Value {
    json!({ "id": u.id, "phone": u.phone, "name": u.name, "role": u.role.as_str() })
}

/// `POST /api/auth/otp/request {phone}` — send a code to an allow-listed phone.
pub async fn otp_request_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OtpRequest>,
) -> Response {
    let ip = client_ip(peer, &headers);
    // The address limit comes first, so the allow-list can't be probed
    // number by number without bound.
    if let Err(retry) = state.limits.check(&format!("otp-send:{ip}"), otp::MAX_SENDS_PER_IP, otp::IP_WINDOW) {
        return too_many(retry);
    }
    let Some(phone) = otp::normalize_phone(&req.phone) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter a 10-digit Indian mobile number");
    };
    let Some(user) = state.auth.users.by_phone(&phone).filter(|u| u.enabled) else {
        tracing::info!(phone = %masked(&phone), ip, "OTP asked for a number that isn't enabled");
        return refuse(StatusCode::FORBIDDEN, NOT_ENABLED);
    };
    let Some(secret) = state.auth.secret.as_deref() else {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, "AUTH_SECRET not configured");
    };
    let admin = user.role == Role::Admin;
    // The admin can still get a code without WhatsApp: it's also on the server.
    if !state.whatsapp.configured() && !admin {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "Sign-in isn't configured on this server");
    }

    let now = shared_domain::now_ist();
    let since = ist(now - Duration::seconds(otp::SEND_WINDOW_SECS));
    let code = otp::generate_code();
    let hash = otp::hash_code(secret, &phone, &code);
    let expires_at = ist(now + Duration::seconds(otp::TTL_SECS));
    // The per-phone limit is enforced by the insert itself.
    let row = match otp::insert_if_under_limit(&state.db_pool, &phone, &hash, &ist(now), &expires_at, &ip, &since, otp::MAX_SENDS_PER_PHONE).await {
        Ok(Some(id)) => id,
        Ok(None) => return too_many(otp::SEND_WINDOW_SECS as u64),
        Err(e) => return server_error("store the code", e),
    };
    let on_server = admin
        && match write_admin_code(&phone, &code, now, &expires_at) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(error = %e, "could not write {ADMIN_OTP_FILE}");
                false
            }
        };
    let delivered = state.whatsapp.configured()
        && match state.whatsapp.send_otp(&phone, &code).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(phone = %masked(&phone), error = %e, "OTP not delivered");
                false
            }
        };
    if !delivered && !on_server {
        let _ = otp::delete(&state.db_pool, row).await;
        return refuse(StatusCode::BAD_GATEWAY, "Could not send the OTP — try again in a minute");
    }
    if on_server {
        tracing::info!(file = ADMIN_OTP_FILE, "Admin sign-in code also written on the server");
    }
    tracing::info!(phone = %masked(&phone), user_id = user.id, delivered, "OTP sent");
    let mut answer = json!({ "sent": true, "delivered": delivered, "expires_in": otp::TTL_SECS, "retry_after": otp::RESEND_AFTER_SECS });
    if !delivered {
        answer["note"] = json!(format!("WhatsApp didn't deliver it. The code is in {ADMIN_OTP_FILE} on the server."));
    }
    Json(answer).into_response()
}

/// `POST /api/auth/otp/verify {phone, code}` — the token, or why not.
pub async fn otp_verify_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OtpVerify>,
) -> Response {
    let ip = client_ip(peer, &headers);
    let verify_key = format!("otp-verify:{ip}");
    if let Err(retry) = state.limits.check(&verify_key, otp::MAX_VERIFIES_PER_IP, otp::IP_WINDOW) {
        return too_many(retry);
    }
    let Some(phone) = otp::normalize_phone(&req.phone) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter a 10-digit Indian mobile number");
    };
    let code = req.code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return refuse(StatusCode::BAD_REQUEST, "Enter the 6-digit code");
    }
    let Some(user) = state.auth.users.by_phone(&phone).filter(|u| u.enabled) else {
        return refuse(StatusCode::FORBIDDEN, NOT_ENABLED);
    };
    let Some(secret) = state.auth.secret.as_deref() else {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, "AUTH_SECRET not configured");
    };

    let now = shared_domain::now_ist();
    match otp::verify(&state.db_pool, secret, &phone, code, &ist(now)).await {
        Err(e) => server_error("check the code", e),
        Ok(otp::Verify::Wrong { attempts_left }) => refuse(
            StatusCode::UNAUTHORIZED,
            format!("Wrong code — {attempts_left} {} left", if attempts_left == 1 { "try" } else { "tries" }),
        ),
        Ok(otp::Verify::NoCode) => refuse(StatusCode::UNAUTHORIZED, "That code has expired — ask for a new one"),
        Ok(otp::Verify::TooManyAttempts) => refuse(StatusCode::UNAUTHORIZED, "Too many wrong codes — ask for a new one"),
        Ok(otp::Verify::Ok) => {
            state.limits.clear(&verify_key);
            if let Err(e) = sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
                .bind(ist(now))
                .bind(user.id)
                .execute(&state.db_pool)
                .await
            {
                tracing::warn!(user_id = user.id, error = %e, "could not record the login time");
            }
            if user.role == Role::Admin {
                // Used: it opens nothing now, so it needn't sit on the disk.
                let _ = std::fs::remove_file(ADMIN_OTP_FILE);
            }
            let payload = token::TokenPayload::phone(user.id, user.role.as_str(), now.timestamp() as u64, user.role.token_ttl_secs());
            let signed = token::sign(&payload, secret);
            tracing::info!(phone = %masked(&phone), user_id = user.id, role = user.role.as_str(), "Phone sign-in");
            Json(json!({ "token": signed, "user": user_json(&user) })).into_response()
        }
    }
}

/// `GET /api/auth/me` — who the caller is: `admin`, `user` or `anonymous`,
/// with the user record for a phone sign-in.
pub async fn me_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let user = identity.user_id().and_then(|id| state.auth.users.get(id)).map(|u| user_json(&u));
    Json(json!({ "identity": identity.kind(), "user": user })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_are_masked_in_logs() {
        assert_eq!(masked("7000000001"), "70••••0001");
        assert_eq!(masked("12"), "••••");
    }
}
