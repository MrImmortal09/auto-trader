//! One-time codes for the phone sign-in: generated here, hashed before they
//! are stored (`otp_codes`, one row per send), checked in constant time, and
//! limited per phone, per address and per code. The limits are enforced in
//! single SQL statements, so guesses or sends fired at once can't slip past
//! them together.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use sqlx::SqlitePool;

type HmacSha256 = Hmac<Sha256>;

/// A code is good for this long.
pub const TTL_SECS: i64 = 10 * 60;
/// Sends one phone may ask for inside `SEND_WINDOW_SECS`.
pub const MAX_SENDS_PER_PHONE: i64 = 3;
pub const SEND_WINDOW_SECS: i64 = 10 * 60;
/// Guesses a code takes before it is thrown away (a right one counts too).
pub const MAX_WRONG_ATTEMPTS: i64 = 5;
/// Per address, inside `IP_WINDOW`.
pub const MAX_SENDS_PER_IP: u32 = 10;
pub const MAX_VERIFIES_PER_IP: u32 = 20;
pub const IP_WINDOW: Duration = Duration::from_secs(10 * 60);
/// What the frontend waits before offering "Send again".
pub const RESEND_AFTER_SECS: u64 = 30;

/// A 10-digit Indian mobile number from however it was typed: spaces, dashes,
/// a `+91` / `91` country code or a leading `0` are dropped. `None` unless
/// ten digits starting 6–9 remain.
pub fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = match digits.len() {
        12 => digits.strip_prefix("91")?.to_string(),
        11 => digits.strip_prefix('0')?.to_string(),
        _ => digits,
    };
    let valid = digits.len() == 10 && matches!(digits.as_bytes()[0], b'6'..=b'9');
    valid.then_some(digits)
}

/// Six digits from the OS random source, zero-padded.
pub fn generate_code() -> String {
    format!("{:06}", rand::rngs::OsRng.gen_range(0..1_000_000u32))
}

/// What is stored for a code: HMAC-SHA256(secret, "phone:code"), so a leaked
/// table gives away nothing without `AUTH_SECRET`.
pub fn hash_code(secret: &str, phone: &str, code: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(format!("{phone}:{code}").as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn code_matches(secret: &str, phone: &str, code: &str, stored: &str) -> bool {
    let computed = hash_code(secret, phone, code);
    computed.len() == stored.len() && bool::from(subtle::ConstantTimeEq::ct_eq(computed.as_bytes(), stored.as_bytes()))
}

/// Record a send, unless `phone` already has `max_sends` recorded at or after
/// `since` (an IST `YYYY-MM-DD HH:MM:SS`) — one statement, so two sends
/// racing each other can't both get in. The row id comes back so a failed
/// delivery can take it away; `None` means the limit is reached.
pub async fn insert_if_under_limit(
    pool: &SqlitePool,
    phone: &str,
    code_hash: &str,
    sent_at: &str,
    expires_at: &str,
    ip: &str,
    since: &str,
    max_sends: i64,
) -> sqlx::Result<Option<i64>> {
    let done = sqlx::query(
        "INSERT INTO otp_codes (phone, code_hash, sent_at, expires_at, attempts, consumed, ip)
         SELECT ?, ?, ?, ?, 0, 0, ?
         WHERE (SELECT COUNT(*) FROM otp_codes WHERE phone = ? AND sent_at >= ?) < ?",
    )
    .bind(phone)
    .bind(code_hash)
    .bind(sent_at)
    .bind(expires_at)
    .bind(ip)
    .bind(phone)
    .bind(since)
    .bind(max_sends)
    .execute(pool)
    .await?;
    Ok((done.rows_affected() == 1).then(|| done.last_insert_rowid()))
}

pub async fn delete(pool: &SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM otp_codes WHERE id = ?").bind(id).execute(pool).await?;
    Ok(())
}

/// Drop codes that expired before `now`.
pub async fn prune_expired(pool: &SqlitePool, now: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM otp_codes WHERE expires_at < ?").bind(now).execute(pool).await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verify {
    /// The code was right; it and every other code for the phone are gone.
    Ok,
    Wrong { attempts_left: i64 },
    /// No live code for this phone: none asked for, or it expired.
    NoCode,
    /// The fifth wrong guess: the code is gone, ask for a new one.
    TooManyAttempts,
}

#[derive(sqlx::FromRow)]
struct Pending {
    id: i64,
    code_hash: String,
}

/// Check `code` against the newest live code for `phone`. The guess is
/// counted in one guarded statement *before* it is compared, so parallel
/// guesses each take one of the five tries; a right code claims the row
/// with the same guard, so it is accepted once.
pub async fn verify(pool: &SqlitePool, secret: &str, phone: &str, code: &str, now: &str) -> sqlx::Result<Verify> {
    let pending = sqlx::query_as::<_, Pending>(
        "SELECT id, code_hash FROM otp_codes
         WHERE phone = ? AND consumed = 0 AND expires_at > ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(phone)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    let Some(pending) = pending else { return Ok(Verify::NoCode) };

    let attempts: Option<i64> = sqlx::query_scalar(
        "UPDATE otp_codes SET attempts = attempts + 1
         WHERE id = ? AND consumed = 0 AND attempts < ? RETURNING attempts",
    )
    .bind(pending.id)
    .bind(MAX_WRONG_ATTEMPTS)
    .fetch_optional(pool)
    .await?;
    let Some(attempts) = attempts else { return Ok(Verify::TooManyAttempts) };

    if code_matches(secret, phone, code, &pending.code_hash) {
        let mut tx = pool.begin().await?;
        let claimed = sqlx::query("UPDATE otp_codes SET consumed = 1 WHERE id = ? AND consumed = 0")
            .bind(pending.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if claimed == 0 {
            return Ok(Verify::NoCode);
        }
        sqlx::query("DELETE FROM otp_codes WHERE phone = ? AND id != ?").bind(phone).bind(pending.id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(Verify::Ok);
    }

    if attempts >= MAX_WRONG_ATTEMPTS {
        sqlx::query("UPDATE otp_codes SET consumed = 1 WHERE id = ?").bind(pending.id).execute(pool).await?;
        return Ok(Verify::TooManyAttempts);
    }
    Ok(Verify::Wrong { attempts_left: MAX_WRONG_ATTEMPTS - attempts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, test_support::TempDb};

    const PHONE: &str = "7000000001";

    #[test]
    fn phones_are_read_however_they_were_typed() {
        for raw in ["7000000001", "+91 70000 00001", "91-7000000001", "07000000001", " 7000 000 001 "] {
            assert_eq!(normalize_phone(raw).as_deref(), Some(PHONE), "{raw:?}");
        }
        // A 10-digit number that happens to start with 91 is kept whole.
        assert_eq!(normalize_phone("9123456789").as_deref(), Some("9123456789"));
        for raw in ["700000000", "70000000011", "5000000001", "", "abc", "+1 415 555 0100"] {
            assert_eq!(normalize_phone(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn codes_are_six_digits_and_hashes_bind_the_phone() {
        let code = generate_code();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
        let h = hash_code("s", PHONE, "123456");
        assert!(code_matches("s", PHONE, "123456", &h));
        assert!(!code_matches("s", PHONE, "123457", &h));
        assert!(!code_matches("s", "9999999999", "123456", &h));
        assert!(!code_matches("other", PHONE, "123456", &h));
    }

    async fn send(pool: &SqlitePool, code: &str, at: &str, until: &str) -> Option<i64> {
        insert_if_under_limit(pool, PHONE, &hash_code("s", PHONE, code), at, until, "1.2.3.4", "2026-10-26 09:50:00", MAX_SENDS_PER_PHONE)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_code_is_accepted_once_and_wrong_guesses_run_out() {
        let tmp = TempDb::new("otp");
        let pool = db::init_db(&tmp.url()).await;
        let secret = "s";
        let id = send(&pool, "123456", "2026-10-26 10:00:00", "2026-10-26 10:10:00").await.unwrap();

        // Expired: no code.
        assert_eq!(verify(&pool, secret, PHONE, "123456", "2026-10-26 10:10:00").await.unwrap(), Verify::NoCode);
        // Wrong, four times: attempts left count down; the right code still works on the fifth try.
        for left in [4, 3, 2, 1] {
            assert_eq!(verify(&pool, secret, PHONE, "000000", "2026-10-26 10:05:00").await.unwrap(), Verify::Wrong { attempts_left: left });
        }
        assert_eq!(verify(&pool, secret, PHONE, "123456", "2026-10-26 10:05:00").await.unwrap(), Verify::Ok);
        // Consumed: a second use is no code.
        assert_eq!(verify(&pool, secret, PHONE, "123456", "2026-10-26 10:05:00").await.unwrap(), Verify::NoCode);

        // A fresh code dies on the fifth wrong guess, and the right code is useless after.
        send(&pool, "654321", "2026-10-26 10:06:00", "2026-10-26 10:16:00").await.unwrap();
        for _ in 0..4 {
            assert!(matches!(verify(&pool, secret, PHONE, "111111", "2026-10-26 10:07:00").await.unwrap(), Verify::Wrong { .. }));
        }
        assert_eq!(verify(&pool, secret, PHONE, "111111", "2026-10-26 10:07:00").await.unwrap(), Verify::TooManyAttempts);
        assert_eq!(verify(&pool, secret, PHONE, "654321", "2026-10-26 10:07:00").await.unwrap(), Verify::NoCode);

        // The send limit: three in the window, the fourth is refused; a removed row frees a slot.
        assert!(send(&pool, "222222", "2026-10-26 10:08:00", "2026-10-26 10:18:00").await.is_some());
        assert!(send(&pool, "333333", "2026-10-26 10:09:00", "2026-10-26 10:19:00").await.is_none());
        delete(&pool, id).await.unwrap();
        assert!(send(&pool, "333333", "2026-10-26 10:09:00", "2026-10-26 10:19:00").await.is_some());

        // The newest code is the one checked; success deletes the older ones.
        assert!(matches!(verify(&pool, secret, PHONE, "222222", "2026-10-26 10:10:00").await.unwrap(), Verify::Wrong { .. }));
        assert_eq!(verify(&pool, secret, PHONE, "333333", "2026-10-26 10:10:00").await.unwrap(), Verify::Ok);
        let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM otp_codes WHERE phone = ? AND consumed = 0").bind(PHONE).fetch_one(&pool).await.unwrap();
        assert_eq!(live, 0);

        prune_expired(&pool, "2026-10-27 00:00:00").await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM otp_codes").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn parallel_guesses_each_spend_a_try() {
        let tmp = TempDb::new("otp-race");
        let pool = db::init_db(&tmp.url()).await;
        send(&pool, "123456", "2026-10-26 10:00:00", "2026-10-26 10:10:00").await.unwrap();
        // Twenty wrong guesses at once: at most five are counted as tries, the rest are refused.
        let guesses = (0..20).map(|_| {
            let pool = pool.clone();
            async move { verify(&pool, "s", PHONE, "000000", "2026-10-26 10:05:00").await.unwrap() }
        });
        let answers = futures::future::join_all(guesses).await;
        let wrong = answers.iter().filter(|a| matches!(a, Verify::Wrong { .. })).count();
        assert!(wrong <= 4, "{answers:?}");
        assert!(answers.iter().all(|a| matches!(a, Verify::Wrong { .. } | Verify::TooManyAttempts)), "{answers:?}");
        // Afterwards even the right code is refused.
        assert_eq!(verify(&pool, "s", PHONE, "123456", "2026-10-26 10:05:00").await.unwrap(), Verify::NoCode);
    }
}
