//! The bearer token — one format for the owner's passkey login and the phone
//! sign-in: `base64url(header).base64url(payload).base64url(HMAC-SHA256)`,
//! signed with `AUTH_SECRET`. The passkey's token says `sub: "trader"`; a
//! phone token says `sub: "u:<user id>"` with `role: "admin"` or `"user"`.
//! Who the token *is* right now (an admin may have been disabled since) is the
//! middleware's business — see `auth::identify`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// `sub` of the owner's passkey token.
pub const OWNER_SUB: &str = "trader";
/// `role` of the owner's passkey token.
pub const OWNER_ROLE: &str = "write";
/// How long each kind of token lives.
pub const OWNER_TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub const ADMIN_TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub const USER_TTL_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Serialize)]
struct TokenHeader {
    alg: &'static str,
    typ: &'static str,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TokenPayload {
    pub sub: String,
    pub iat: u64,
    pub exp: u64,
    #[serde(default)]
    pub role: Option<String>,
}

impl TokenPayload {
    /// The owner's passkey token, as it has always been minted.
    pub fn owner(now: u64) -> Self {
        TokenPayload { sub: OWNER_SUB.to_string(), iat: now, exp: now + OWNER_TTL_SECS, role: Some(OWNER_ROLE.to_string()) }
    }

    /// A phone sign-in's token for `user_id` with `role` (`admin` or `user`).
    pub fn phone(user_id: i64, role: &str, now: u64, ttl_secs: u64) -> Self {
        TokenPayload { sub: format!("u:{user_id}"), iat: now, exp: now + ttl_secs, role: Some(role.to_string()) }
    }

    pub fn is_owner(&self) -> bool {
        self.sub == OWNER_SUB
    }

    /// The user id a phone token names (`u:<id>`), if it is one.
    pub fn user_id(&self) -> Option<i64> {
        self.sub.strip_prefix("u:")?.parse().ok()
    }
}

/// Sign `payload` with `secret`.
pub fn sign(payload: &TokenPayload, secret: &str) -> String {
    let header = TokenHeader { alg: "HS256", typ: "AT" };
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).expect("header serialises"));
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(payload).expect("payload serialises"));
    let msg = format!("{header_b64}.{payload_b64}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(msg.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{msg}.{sig_b64}")
}

/// Verifies HMAC signature, decoding, and expiration of a token string.
pub fn verify_token(token: &str, auth_secret: &str) -> Result<TokenPayload, &'static str> {
    verify_at(token, auth_secret, shared_domain::now_ist().timestamp() as u64)
}

/// `verify_token` with the clock supplied — for tests.
pub fn verify_at(token: &str, auth_secret: &str, now: u64) -> Result<TokenPayload, &'static str> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("Malformed token");
    }

    let msg = format!("{}.{}", parts[0], parts[1]);
    let mut mac = HmacSha256::new_from_slice(auth_secret.as_bytes()).map_err(|_| "HMAC init failed")?;
    mac.update(msg.as_bytes());
    let expected_sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    if parts[2].len() != expected_sig_b64.len()
        || !bool::from(subtle::ConstantTimeEq::ct_eq(parts[2].as_bytes(), expected_sig_b64.as_bytes()))
    {
        return Err("Invalid token signature");
    }

    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).map_err(|_| "Invalid token payload encoding")?;
    let payload: TokenPayload = serde_json::from_slice(&payload_bytes).map_err(|_| "Invalid token payload format")?;

    if now > payload.exp {
        return Err("Token expired");
    }

    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret";

    #[test]
    fn an_owner_token_round_trips_and_is_the_owner() {
        let token = sign(&TokenPayload::owner(1_000), SECRET);
        let payload = verify_at(&token, SECRET, 1_500).unwrap();
        assert!(payload.is_owner());
        assert_eq!(payload.user_id(), None);
        assert_eq!(payload.role.as_deref(), Some("write"));
        assert_eq!(payload.exp, 1_000 + OWNER_TTL_SECS);
    }

    #[test]
    fn a_phone_token_names_its_user_and_role() {
        let token = sign(&TokenPayload::phone(42, "user", 1_000, USER_TTL_SECS), SECRET);
        let payload = verify_at(&token, SECRET, 1_500).unwrap();
        assert!(!payload.is_owner());
        assert_eq!(payload.user_id(), Some(42));
        assert_eq!(payload.role.as_deref(), Some("user"));
        assert_eq!(payload.exp, 1_000 + USER_TTL_SECS);
    }

    #[test]
    fn a_tampered_or_foreign_token_is_refused() {
        let token = sign(&TokenPayload::phone(42, "user", 1_000, USER_TTL_SECS), SECRET);
        assert_eq!(verify_at(&token, "other-secret", 1_500), Err("Invalid token signature"));
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        // A payload claiming another user, with the old signature.
        parts[1] = URL_SAFE_NO_PAD.encode(r#"{"sub":"u:7","iat":1000,"exp":9999999,"role":"admin"}"#);
        assert_eq!(verify_at(&parts.join("."), SECRET, 1_500), Err("Invalid token signature"));
        assert_eq!(verify_at("not.a.token.at.all", SECRET, 1_500), Err("Malformed token"));
    }

    #[test]
    fn an_expired_token_is_refused() {
        let token = sign(&TokenPayload::phone(1, "admin", 1_000, ADMIN_TTL_SECS), SECRET);
        assert!(verify_at(&token, SECRET, 1_000 + ADMIN_TTL_SECS).is_ok());
        assert_eq!(verify_at(&token, SECRET, 1_001 + ADMIN_TTL_SECS), Err("Token expired"));
    }
}
