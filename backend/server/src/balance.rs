//! Kotak account balance for the dashboard, fetched in the background.
//!
//! `/api/portfolio` never calls Kotak: it serves the last balance read here.
//! The page used to call Kotak `limits` on every poll (every few seconds, from
//! each open tab) while holding the shared Kotak client mutex, so one hung
//! `limits` call held up every order behind it for the client's 30 s timeout —
//! on 2026-10-19 a SENSEX stop-loss sell waited ~10 s on one and filled 22
//! points under its stop.
//!
//! The balance is now read once an hour, and once shortly after every LIVE buy
//! or sell is recorded. The engine's own pre-entry funds check is separate and
//! still reads Kotak live — this cache is display-only and never consulted
//! before an order. A failed read is retried each minute during the trading
//! day only: Kotak's funds service doesn't answer at night (`300015` until
//! about 04:25 IST), and a minute's retries all night were hundreds of
//! refused calls.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, FixedOffset, Timelike};

use kotak_client::KotakClient;
use tokio::sync::{Mutex, Notify, RwLock};

/// Routine refresh period.
const REFRESH_EVERY: Duration = Duration::from_secs(3600);
/// After a fill, give Kotak a moment to reflect it in `limits`.
const AFTER_TRADE_DELAY: Duration = Duration::from_secs(2);
/// How often the task wakes to check whether the hourly refresh is due.
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// Spacing between attempts while the hourly read keeps failing, during the
/// trading day; outside it a failed read waits for the next hourly one.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

/// 09:00–15:45 IST on a weekday: from the morning login to the session clear.
fn in_trading_day(now: DateTime<FixedOffset>) -> bool {
    let minute = now.hour() * 60 + now.minute();
    now.weekday().num_days_from_monday() < 5 && (9 * 60..15 * 60 + 45).contains(&minute)
}

/// How long a failed read waits before the next try, at `now`.
fn retry_gap(now: DateTime<FixedOffset>) -> Duration {
    if in_trading_day(now) { RETRY_AFTER_FAILURE } else { REFRESH_EVERY }
}

#[derive(Clone, Debug)]
pub struct CachedBalance {
    /// Kotak's net available funds.
    pub net: f64,
    /// IST `YYYY-MM-DD HH:MM:SS` of the read.
    pub fetched_at: String,
}

#[derive(Default)]
pub struct BalanceCache {
    latest: RwLock<Option<CachedBalance>>,
    trade: Notify,
}

impl BalanceCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last successful read, if any.
    pub async fn latest(&self) -> Option<CachedBalance> {
        self.latest.read().await.clone()
    }

    /// A LIVE buy or sell was recorded — refresh shortly. Several in a row
    /// collapse into one refresh.
    pub fn trade_recorded(&self) {
        self.trade.notify_one();
    }
}

/// Runs forever. Refreshes once an hour (the first time as soon as a Kotak
/// session exists) and shortly after every recorded LIVE trade. The client is
/// cloned out of the mutex, so the read itself never holds it.
pub async fn run_balance_refresher(
    kotak: Arc<Mutex<Option<KotakClient>>>,
    cache: Arc<BalanceCache>,
) {
    let mut last_ok: Option<Instant> = None;
    let mut last_attempt: Option<Instant> = None;
    // A trade asked for a refresh that has not succeeded yet.
    let mut owed = false;
    // The first check runs at once, so a restart shows the balance right away.
    let mut wait = Duration::ZERO;

    loop {
        let traded = tokio::select! {
            _ = cache.trade.notified() => true,
            _ = tokio::time::sleep(wait) => false,
        };
        wait = CHECK_EVERY;
        if traded {
            owed = true;
            tokio::time::sleep(AFTER_TRADE_DELAY).await;
        } else {
            let due = owed || last_ok.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
            let gap = retry_gap(shared_domain::now_ist());
            let retry_ok = last_attempt.is_none_or(|t| t.elapsed() >= gap);
            if !due || !retry_ok {
                continue;
            }
        }

        let Some(client) = kotak.lock().await.clone() else {
            continue;
        };
        last_attempt = Some(Instant::now());
        match client.get_limits().await {
            Ok(limits) => {
                *cache.latest.write().await = Some(CachedBalance {
                    net: limits.net,
                    fetched_at: shared_domain::current_ist_timestamp_string(),
                });
                last_ok = Some(Instant::now());
                owed = false;
            }
            Err(e) => {
                tracing::warn!(error = %e, after_trade = traded, "Kotak balance refresh failed — showing the last read");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ist(d: (i32, u32, u32), t: (u32, u32)) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(5 * 3600 + 1800).unwrap().with_ymd_and_hms(d.0, d.1, d.2, t.0, t.1, 0).unwrap()
    }

    #[test]
    fn a_failed_read_retries_each_minute_only_during_the_trading_day() {
        // Wednesday 2026-10-28.
        assert_eq!(retry_gap(ist((2026, 10, 28), (9, 0))), RETRY_AFTER_FAILURE);
        assert_eq!(retry_gap(ist((2026, 10, 28), (12, 30))), RETRY_AFTER_FAILURE);
        assert_eq!(retry_gap(ist((2026, 10, 28), (15, 44))), RETRY_AFTER_FAILURE);
        // Night, before the morning login, after the clear, and the weekend.
        assert_eq!(retry_gap(ist((2026, 10, 28), (3, 0))), REFRESH_EVERY);
        assert_eq!(retry_gap(ist((2026, 10, 28), (8, 59))), REFRESH_EVERY);
        assert_eq!(retry_gap(ist((2026, 10, 28), (15, 45))), REFRESH_EVERY);
        assert_eq!(retry_gap(ist((2026, 10, 31), (11, 0))), REFRESH_EVERY);
    }
}
