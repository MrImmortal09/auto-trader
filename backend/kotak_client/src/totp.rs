//! RFC 6238 TOTP generation for the Kotak Neo authenticator secret.
//!
//! Kotak's `tradeApiLogin` endpoint expects an already-computed 6-digit code
//! (see [`crate::error::KotakCredentials::totp`]) — this module derives that
//! code from the Base32 secret shown once during TOTP registration, so the
//! app can log in without a human reading it off an authenticator app.

use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::error::KotakError;

type HmacSha1 = Hmac<Sha1>;

const TOTP_STEP_SECS: u64 = 30;
const TOTP_DIGITS: u32 = 6;

/// Generate the current 6-digit TOTP code for a Base32-encoded secret.
///
/// Accepts secrets copied verbatim from an authenticator app's setup screen —
/// whitespace and `-` separators are stripped and the secret is treated
/// case-insensitively, matching what Google Authenticator / Kotak's TOTP
/// registration QR normally shows.
pub fn generate_totp(secret: &str) -> Result<String, KotakError> {
    generate_totp_at(secret, std::time::SystemTime::now())
}

fn generate_totp_at(secret: &str, at: std::time::SystemTime) -> Result<String, KotakError> {
    let cleaned: String = secret
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    let cleaned = cleaned.trim_end_matches('=');

    if cleaned.is_empty() {
        return Err(KotakError::TotpSecretInvalid("secret is empty".into()));
    }

    let key = data_encoding::BASE32_NOPAD
        .decode(cleaned.as_bytes())
        .map_err(|e| KotakError::TotpSecretInvalid(format!("not valid Base32: {e}")))?;

    // Unix time is timezone-agnostic, so this must NOT go through the app's
    // `shared_domain::now_ist()` helper — the TOTP counter is defined in
    // terms of the raw epoch, not IST wall-clock time.
    let unix_secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| KotakError::TotpSecretInvalid("system clock before Unix epoch".into()))?
        .as_secs();
    let counter = unix_secs / TOTP_STEP_SECS;

    let mut mac = HmacSha1::new_from_slice(&key)
        .map_err(|_| KotakError::TotpSecretInvalid("HMAC key of invalid length".into()))?;
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();

    // Standard RFC 4226 dynamic truncation.
    let offset = (hash[hash.len() - 1] & 0x0f) as usize;
    let binary = ((u32::from(hash[offset]) & 0x7f) << 24)
        | (u32::from(hash[offset + 1]) << 16)
        | (u32::from(hash[offset + 2]) << 8)
        | u32::from(hash[offset + 3]);

    let modulus = 10u32.pow(TOTP_DIGITS);
    Ok(format!("{:0width$}", binary % modulus, width = TOTP_DIGITS as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    // RFC 6238 Appendix B test vectors use a 20-byte SHA1 secret whose ASCII
    // bytes are "12345678901234567890" — Base32 of that is the value below.
    const RFC6238_SECRET_BASE32: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn matches_rfc6238_sha1_vector_at_59s() {
        // RFC 6238 Appendix B gives the 8-digit SHA1 code "94287082" at T=59s;
        // our 6-digit truncation is that same value mod 1_000_000.
        let t = UNIX_EPOCH + Duration::from_secs(59);
        assert_eq!(generate_totp_at(RFC6238_SECRET_BASE32, t).unwrap(), "287082");
    }

    #[test]
    fn strips_whitespace_and_dashes_and_is_case_insensitive() {
        let t = UNIX_EPOCH + Duration::from_secs(59);
        let spaced = "gezd gnbv-gy3t qojq gezd-gnbv gy3t qojq";
        assert_eq!(
            generate_totp_at(spaced, t).unwrap(),
            generate_totp_at(RFC6238_SECRET_BASE32, t).unwrap()
        );
    }

    #[test]
    fn six_digits_always_zero_padded() {
        let code = generate_totp_at(RFC6238_SECRET_BASE32, UNIX_EPOCH).unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn rejects_invalid_base32() {
        assert!(generate_totp("not-valid-base32-!!!").is_err());
    }
}
