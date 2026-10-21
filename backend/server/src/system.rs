//! The v2 System screen's day counts (`GET /api/v2/system`): what became of
//! today's Telegram calls, today's orders, and when the Upstox token runs out.
//! Pure: the route gathers the inputs.

use std::collections::BTreeMap;

use base64::Engine as _;
use chrono::NaiveDate;
use serde::Serialize;

use crate::desk::FeedStatus;
use crate::pulse::Sample;

/// What became of today's calls. The parts add up to `calls`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct SignalCounts {
    pub calls: usize,
    /// Bought, whether still open or closed since.
    pub traded: usize,
    pub waiting: usize,
    /// Not bought within the call's validity (`CALL_EXPIRED`).
    pub expired: usize,
    /// Dropped because the VWAP + PCR indicator disagreed (`INDICATOR_SKIP`).
    pub indicator: usize,
    /// Skipped under `index_options_only`.
    pub not_index: usize,
    /// Not taken for any other reason: the kill switch, a duplicate, an entry
    /// already past target 1, the 15:39 cutoff…
    pub other: usize,
}

/// From the feed statuses of today's messages (only calls count) and the
/// reasons of today's abandoned entries (`ENTRY_ABANDONED`). The reasons are
/// only counted up to the calls not taken, so the parts always add up.
pub fn signal_counts<'a>(statuses: impl IntoIterator<Item = &'a FeedStatus>, abandoned: &[String]) -> SignalCounts {
    let mut c = SignalCounts::default();
    let mut not_taken = 0;
    for status in statuses {
        match status {
            FeedStatus::Bought { .. } | FeedStatus::Closed { .. } => c.traded += 1,
            FeedStatus::Waiting { .. } => c.waiting += 1,
            FeedStatus::Skipped => c.not_index += 1,
            FeedStatus::NotTaken => not_taken += 1,
            FeedStatus::NotACall { .. } | FeedStatus::StopUpdate { .. } | FeedStatus::Exit { .. } => continue,
        }
        c.calls += 1;
    }
    let count = |prefix: &str| abandoned.iter().filter(|r| r.starts_with(prefix)).count();
    c.expired = count("CALL_EXPIRED").min(not_taken);
    c.indicator = count("INDICATOR_SKIP").min(not_taken - c.expired);
    c.other = not_taken - c.expired - c.indicator;
    c
}

/// Today's orders, in the current mode.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct OrderCounts {
    /// LIVE: orders sent to Kotak. Paper: its fills, as every paper order fills.
    pub sent: u64,
    /// Trade rows today in this mode.
    pub filled: u64,
    pub rejected: u64,
    pub cancelled: u64,
    /// Order-book polls that failed while settling LIVE fills.
    pub poll_failures: u64,
}

fn sum(m: &BTreeMap<String, u64>) -> u64 {
    m.values().sum()
}

fn get(m: &BTreeMap<String, u64>, key: &str) -> u64 {
    m.get(key).copied().unwrap_or(0)
}

/// `now`'s counters less the day's `base` (none: everything counted is today's).
pub fn order_counts(live: bool, fills: u64, now: &Sample, base: Option<&Sample>) -> OrderCounts {
    if !live {
        return OrderCounts { sent: fills, filled: fills, ..OrderCounts::default() };
    }
    let today = |f: &dyn Fn(&Sample) -> u64| f(now).saturating_sub(base.map_or(0, f));
    OrderCounts {
        sent: today(&|s| sum(&s.orders_by_result)),
        filled: fills,
        rejected: today(&|s| get(&s.orders_by_result, "rejected")),
        cancelled: today(&|s| get(&s.order_cancels, "ok")),
        poll_failures: today(&|s| s.order_book_poll_failures_total),
    }
}

/// The session watchdog's re-logins today, successful or not.
pub fn relogins_today(now: &Sample, base: Option<&Sample>) -> u64 {
    sum(&now.kotak_relogins).saturating_sub(base.map_or(0, |b| sum(&b.kotak_relogins)))
}

/// The IST day a JWT's `exp` falls on — read from its payload; the token
/// itself is never returned or logged.
pub fn token_expiry(token: &str) -> Option<NaiveDate> {
    let payload = token.trim().split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?.as_i64()?;
    let at = chrono::DateTime::from_timestamp(exp, 0)?;
    Some(at.with_timezone(&shared_domain::ist_offset()).date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_split_by_what_became_of_them() {
        let statuses = [
            FeedStatus::Bought { qty: 130, price: 150.0 },
            FeedStatus::Closed { outcome: None, pnl: Some(500.0) },
            FeedStatus::Waiting { valid_till: None },
            FeedStatus::Skipped,
            FeedStatus::NotTaken,
            FeedStatus::NotTaken,
            FeedStatus::NotTaken,
            FeedStatus::NotACall { no_instrument: true },
            FeedStatus::StopUpdate { price: Some(120.0) },
        ];
        let reasons = |rs: &[&str]| rs.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        let c = signal_counts(&statuses, &reasons(&["CALL_EXPIRED — not bought within 30 min", "INDICATOR_SKIP — NIFTY disagrees"]));
        assert_eq!(c, SignalCounts { calls: 7, traded: 2, waiting: 1, expired: 1, indicator: 1, not_index: 1, other: 1 });
        // More abandoned entries than calls not taken (a call from yesterday,
        // a scalp): the parts still add up.
        let c = signal_counts(&statuses, &reasons(&["CALL_EXPIRED"; 5]));
        assert_eq!((c.expired, c.indicator, c.other), (3, 0, 0));
        assert_eq!(c.traded + c.waiting + c.expired + c.indicator + c.not_index + c.other, c.calls);
    }

    #[test]
    fn todays_orders_are_counted_from_the_day_start() {
        let sample = |sent_ok: u64, rejected: u64, cancels: u64, polls: u64| Sample {
            orders_by_result: BTreeMap::from([("ok".to_string(), sent_ok), ("rejected".to_string(), rejected)]),
            order_cancels: BTreeMap::from([("ok".to_string(), cancels), ("error".to_string(), 1)]),
            order_book_poll_failures_total: polls,
            ..Sample::default()
        };
        let yesterday = sample(40, 2, 3, 1);
        let now = sample(84, 3, 4, 1);
        assert_eq!(
            order_counts(true, 44, &now, Some(&yesterday)),
            OrderCounts { sent: 45, filled: 44, rejected: 1, cancelled: 1, poll_failures: 0 }
        );
        // The day the server started: all of it is today's.
        assert_eq!(order_counts(true, 44, &now, None).sent, 87);
        // Paper fills are the orders; Kotak's counters don't apply.
        assert_eq!(order_counts(false, 12, &now, None), OrderCounts { sent: 12, filled: 12, ..OrderCounts::default() });
    }

    #[test]
    fn a_tokens_expiry_is_read_as_an_ist_day() {
        // exp = 2027-03-14 00:30 IST, still the 13th in UTC.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOiAxODA0OTY0NDAwfQ==.c2ln";
        assert_eq!(token_expiry(jwt), NaiveDate::from_ymd_opt(2027, 3, 14));
        assert_eq!(token_expiry("not-a-jwt"), None);
        assert_eq!(token_expiry("a.bm90IGpzb24.c"), None);
    }
}
