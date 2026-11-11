//! The close snapshot: at 15:35 IST on a trading day one sweep of every
//! catalog equity and index and every key watched lately, through the same
//! scheduler as the live polling, into `quote_eod` (in the catalog's
//! `instruments.db`); served as the day's close ("as of 15:30") once the
//! market is shut, until the next day's quotes arrive. Necessary, not a
//! nicety: the Kotak base URL lives only in the session, which the 15:40
//! clear drops, so no REST quote is possible after it until the next login.
//!
//! The sweep's key set is public market data only — the catalog's cash keys
//! and what signed-in users asked about in the last 15 minutes. It never
//! takes the owner's open contracts or the paper desks' from their
//! subscriptions: a close answered for a contract nobody asked about would
//! tell a user it was held at 15:35.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};
use shared_domain::FeatureFlags;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::hub::{Quote, QuoteHub, Source};
use crate::instruments::Catalog;

/// When the sweep starts, IST.
pub const SWEEP_HOUR: u32 = 15;
pub const SWEEP_MINUTE: u32 = 35;
/// The close a snapshot stands for, in a served quote's `as_of`.
pub const CLOSE_TIME: &str = "15:30";
/// The sweep ends by this long after it starts: before the 15:40 clear.
pub const SWEEP_WINDOW: Duration = Duration::from_secs(270);
/// Days of closes kept.
const KEEP_DAYS: i64 = 14;
/// Rows per `INSERT`: 9 columns each, well under SQLite's variable limit.
const INSERT_BATCH: usize = 200;

/// One instrument's close.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub ltp: f64,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    /// The previous close.
    pub close: Option<f64>,
    pub volume: Option<f64>,
}

impl Row {
    pub fn of(q: &Quote) -> Self {
        Row { key: q.key.clone(), ltp: q.ltp, open: q.open, high: q.high, low: q.low, close: q.close, volume: q.volume }
    }
}

/// One day's closes.
pub struct Snapshot {
    pub day: NaiveDate,
    pub taken_at: DateTime<FixedOffset>,
    pub rows: HashMap<String, Row>,
    /// Keys the sweep asked for. More than `rows` means some got no close:
    /// the sweep was cut short by its deadline, or Kotak left keys out.
    pub asked: usize,
}

impl Snapshot {
    /// Whether some key asked for got no close.
    pub fn partial(&self) -> bool {
        self.rows.len() < self.asked
    }

    /// The close quote for `key`, if the sweep saw it.
    pub fn quote(&self, key: &str) -> Option<Quote> {
        let r = self.rows.get(key)?;
        let (change, per_change) = match r.close {
            Some(c) if c > 0.0 => (Some(r.ltp - c), Some((r.ltp - c) / c * 100.0)),
            _ => (None, None),
        };
        Some(Quote {
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            change,
            per_change,
            volume: r.volume,
            as_of: Some(format!("{} {CLOSE_TIME}", self.day)),
            ..Quote::bare(key, r.ltp, self.taken_at.timestamp_millis(), Source::Close)
        })
    }
}

/// Whether the market is open at `now`, in the shape of the paper loop's rule
/// (`paper::engine`): an admin-set special session (`shared_domain::
/// in_special_session_at`, Muhurat trading, even on a holiday or a Sunday),
/// or the regular weekday hours on a day that is not an exchange holiday,
/// from the calendar the paper loop and the desk read
/// (`telegram_ingester::parser::is_trading_holiday`). One difference: the
/// regular close here is `shared_domain`'s 15:40 (the hub's 15:35 sweep
/// depends on it), while the paper loop still closes its own window at 15:30.
pub fn open_at(now: DateTime<FixedOffset>) -> bool {
    shared_domain::in_special_session_at(now)
        || (shared_domain::is_regular_market_hours_at(now) && !telegram_ingester::parser::is_trading_holiday(now.date_naive()))
}

/// The next `hour:minute` IST at or after `now`: today's, or tomorrow's
/// once today's has passed.
pub fn next_at(now: DateTime<FixedOffset>, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    let today = today_at(now, hour, minute);
    if today > now {
        today
    } else {
        today + chrono::Duration::hours(24)
    }
}

/// `hour:minute` IST on `now`'s day.
fn today_at(now: DateTime<FixedOffset>, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    now.date_naive()
        .and_hms_opt(hour, minute, 0)
        .expect("valid time")
        .and_local_timezone(shared_domain::ist_offset())
        .single()
        .expect("IST time is unambiguous")
}

/// When the next sweep starts and by when it must be done: the next 15:35
/// and [`SWEEP_WINDOW`] after it — except on the sweep task's `first`
/// schedule (a start or restart; deploys run at any time of day) when
/// `now` is already inside today's window and no close of today's is on
/// file (`close_day`): then at once, with what is left of the window.
/// Otherwise the restart would wait for tomorrow's sweep and the evening
/// would show the day's last REST reads and, once those are evicted,
/// yesterday's close. Whether today is a trading day is the caller's
/// check, as for any sweep.
pub fn sweep_window(now: DateTime<FixedOffset>, first: bool, close_day: Option<NaiveDate>) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
    let window = chrono::Duration::from_std(SWEEP_WINDOW).expect("a few minutes");
    let today = today_at(now, SWEEP_HOUR, SWEEP_MINUTE);
    if first && now >= today && now < today + window && close_day.is_none_or(|day| day < now.date_naive()) {
        return (now, today + window);
    }
    let at = next_at(now, SWEEP_HOUR, SWEEP_MINUTE);
    (at, at + window)
}

fn day_text(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

/// The latest day's rows in `quote_eod`, if any.
pub async fn load_latest(pool: &SqlitePool) -> sqlx::Result<Option<Snapshot>> {
    let Some((day, taken_at)) =
        sqlx::query_as::<_, (String, String)>("SELECT day, taken_at FROM quote_eod ORDER BY day DESC, taken_at DESC LIMIT 1")
            .fetch_optional(pool)
            .await?
    else {
        return Ok(None);
    };
    let rows = sqlx::query_as::<_, (String, f64, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>)>(
        "SELECT key, ltp, open, high, low, close, volume FROM quote_eod WHERE day = ?",
    )
    .bind(&day)
    .fetch_all(pool)
    .await?;
    let asked = sqlx::query_scalar::<_, i64>("SELECT asked FROM quote_eod_days WHERE day = ?")
        .bind(&day)
        .fetch_optional(pool)
        .await?;
    let Some(day) = NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok() else {
        tracing::error!(day, "quote_eod: unreadable day");
        return Ok(None);
    };
    let taken_at = NaiveDateTime::parse_from_str(&taken_at, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|t| t.and_local_timezone(shared_domain::ist_offset()).single())
        .unwrap_or_else(|| {
            day.and_hms_opt(SWEEP_HOUR, SWEEP_MINUTE, 0)
                .expect("valid time")
                .and_local_timezone(shared_domain::ist_offset())
                .single()
                .expect("IST time is unambiguous")
        });
    let rows: HashMap<String, Row> = rows
        .into_iter()
        .map(|(key, ltp, open, high, low, close, volume)| (key.clone(), Row { key, ltp, open, high, low, close, volume }))
        .collect();
    // A day with no `quote_eod_days` row (written before the table existed)
    // is taken as complete.
    let asked = asked.map_or(rows.len(), |n| usize::try_from(n).unwrap_or(rows.len()));
    Ok(Some(Snapshot { day, taken_at, rows, asked }))
}

/// Writes a snapshot's rows, replacing the day's, in one transaction.
pub async fn save(pool: &SqlitePool, snapshot: &Snapshot) -> sqlx::Result<()> {
    let day = day_text(snapshot.day);
    let taken_at = snapshot.taken_at.format("%Y-%m-%d %H:%M:%S").to_string();
    let mut rows: Vec<&Row> = snapshot.rows.values().collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    let mut tx = pool.begin().await?;
    for batch in rows.chunks(INSERT_BATCH) {
        let placeholders = vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
        let sql = format!(
            "INSERT OR REPLACE INTO quote_eod (key, day, ltp, close, open, high, low, volume, taken_at) VALUES {placeholders}"
        );
        let mut query = sqlx::query(&sql);
        for r in batch {
            query = query
                .bind(&r.key)
                .bind(&day)
                .bind(r.ltp)
                .bind(r.close)
                .bind(r.open)
                .bind(r.high)
                .bind(r.low)
                .bind(r.volume)
                .bind(&taken_at);
        }
        query.execute(&mut *tx).await?;
    }
    sqlx::query("INSERT OR REPLACE INTO quote_eod_days (day, taken_at, asked, answered) VALUES (?, ?, ?, ?)")
        .bind(&day)
        .bind(&taken_at)
        .bind(snapshot.asked as i64)
        .bind(snapshot.rows.len() as i64)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// Drops the closes of days before `before`; returns how many rows went.
pub async fn prune(pool: &SqlitePool, before: NaiveDate) -> sqlx::Result<u64> {
    let before = day_text(before);
    sqlx::query("DELETE FROM quote_eod_days WHERE day < ?").bind(&before).execute(pool).await?;
    Ok(sqlx::query("DELETE FROM quote_eod WHERE day < ?").bind(&before).execute(pool).await?.rows_affected())
}

/// Runs forever: at 15:35 IST every trading day, while the `search` switch
/// is on, sweeps the catalog's equities and indices and every key watched
/// lately, then writes what came back and keeps it in memory as the day's
/// close. Started inside the sweep's window with no close of the day on
/// file (a restart mid-sweep), it sweeps at once with what is left of the
/// window (`sweep_window`). `pool` is the catalog database's; without one
/// the close is held in memory only.
pub async fn run_close_sweep(hub: Arc<QuoteHub>, pool: Option<SqlitePool>, catalog: Arc<Catalog>, flags: Arc<RwLock<FeatureFlags>>) {
    // The close on file was put back before this task was spawned
    // (`quotes::spawn`), so the first schedule knows whether today's is in.
    let mut first = true;
    loop {
        let now = shared_domain::now_ist();
        let (at, end) = sweep_window(now, std::mem::take(&mut first), hub.snapshot().map(|s| s.day));
        if at > now {
            let secs = (at - now).num_seconds().max(1) as u64;
            tracing::info!(secs, "Close snapshot sweep scheduled");
            tokio::time::sleep(Duration::from_secs(secs)).await;
        } else {
            tracing::info!(
                secs_left = (end - now).num_seconds(),
                "Close snapshot sweep starting at once — the server started inside today's sweep window with no close of today's on file"
            );
        }

        let now = shared_domain::now_ist();
        if telegram_ingester::parser::is_trading_holiday(now.date_naive()) {
            tracing::info!(day = %now.date_naive(), "Close snapshot sweep skipped — not a trading day");
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        if !flags.read().await.search {
            tracing::info!("Close snapshot sweep skipped — the `search` switch is off");
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        // The deadline is the window's end on the clock: a full window for
        // the 15:35 start, what is left of it for a catch-up.
        let Some(left) = (end - now).to_std().ok().filter(|left| !left.is_zero()) else {
            tracing::warn!("Close snapshot sweep skipped — its window had already passed when it woke");
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        };
        let mut keys: BTreeSet<String> = catalog.cash_keys().into_iter().collect();
        let catalog_keys = keys.len();
        keys.extend(hub.watched_keys());
        let total = hub.start_sweep(keys, now.date_naive(), now, Instant::now() + left);
        tracing::info!(keys = total, catalog_keys, secs = left.as_secs(), "Close snapshot sweep started");

        let sweep = loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Some(sweep) = hub.take_sweep_if_done(Instant::now()) {
                break sweep;
            }
        };
        let cut_short = sweep.cut_short();
        let snapshot = Snapshot { day: sweep.day, taken_at: sweep.started, rows: sweep.rows, asked: sweep.total };
        if snapshot.rows.is_empty() {
            tracing::warn!(
                day = %snapshot.day, keys = sweep.total,
                "Close snapshot sweep got no quotes — no Kotak session, or every call failed; the last close on file stays"
            );
        } else {
            if cut_short {
                tracing::warn!(
                    day = %snapshot.day, rows = snapshot.rows.len(), keys = snapshot.asked,
                    "Close snapshot sweep cut short by the 15:40 deadline — the day's close is partial (instrument status says so)"
                );
            } else if snapshot.partial() {
                tracing::warn!(
                    day = %snapshot.day, rows = snapshot.rows.len(), keys = snapshot.asked,
                    "Close snapshot sweep finished with keys unanswered — the day's close is partial (instrument status says so)"
                );
            }
            match &pool {
                Some(pool) => {
                    match save(pool, &snapshot).await {
                        Ok(()) => tracing::info!(day = %snapshot.day, rows = snapshot.rows.len(), keys = snapshot.asked, "Close snapshot saved"),
                        Err(e) => tracing::error!(day = %snapshot.day, error = %e, "Close snapshot could not be saved — served from memory until a restart"),
                    }
                    match prune(pool, snapshot.day - chrono::Duration::days(KEEP_DAYS)).await {
                        Ok(n) if n > 0 => tracing::info!(rows = n, "Old closes pruned"),
                        Ok(_) => {}
                        Err(e) => tracing::error!(error = %e, "quote_eod prune failed"),
                    }
                }
                None => tracing::warn!(day = %snapshot.day, "Close snapshot held in memory only — the catalog database is not open"),
            }
            hub.set_snapshot(snapshot);
        }
        // Step past the trigger instant so the next iteration schedules
        // tomorrow's occurrence, not today's again.
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::TempDb;

    fn ist(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .single()
            .unwrap()
    }

    #[test]
    fn the_market_is_open_on_trading_days_from_0915_until_1540() {
        // Special sessions are process-wide; the guard clears them and keeps
        // the other tests that set one waiting.
        let _guard = shared_domain::special_sessions_test_guard();
        // 2026-11-04 is a Wednesday.
        assert!(!open_at(ist(2026, 11, 4, 9, 14)));
        assert!(open_at(ist(2026, 11, 4, 9, 15)));
        assert!(open_at(ist(2026, 11, 4, 15, 39)));
        assert!(!open_at(ist(2026, 11, 4, 15, 40)));
        assert!(!open_at(ist(2026, 11, 4, 20, 0)));
        assert!(!open_at(ist(2026, 11, 7, 12, 0)), "Saturday");
        assert!(!open_at(ist(2026, 11, 8, 12, 0)), "Sunday");
        // An exchange holiday on a weekday, from the shared calendar.
        assert!(telegram_ingester::parser::is_trading_holiday(NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()));
        assert!(!open_at(ist(2026, 11, 10, 12, 0)), "Tuesday 2026-11-10 is a holiday");
        assert!(open_at(ist(2026, 11, 11, 12, 0)), "the day after is not");
        // A special session (Muhurat trading) opens the market on the holiday
        // for its hours alone, as the paper loop's rule has it.
        shared_domain::set_special_sessions(vec![shared_domain::SpecialSession {
            date: NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
            start: 18 * 60,
            end: 19 * 60 + 15,
        }]);
        assert!(!open_at(ist(2026, 11, 10, 12, 0)), "the holiday's regular hours stay shut");
        assert!(!open_at(ist(2026, 11, 10, 17, 59)));
        assert!(open_at(ist(2026, 11, 10, 18, 0)));
        assert!(open_at(ist(2026, 11, 10, 19, 14)));
        assert!(!open_at(ist(2026, 11, 10, 19, 15)));
        shared_domain::set_special_sessions(vec![]);
    }

    #[test]
    fn the_sweep_is_scheduled_for_today_or_tomorrow() {
        assert_eq!(next_at(ist(2026, 11, 4, 9, 0), 15, 35), ist(2026, 11, 4, 15, 35));
        assert_eq!(next_at(ist(2026, 11, 4, 15, 34), 15, 35), ist(2026, 11, 4, 15, 35));
        assert_eq!(next_at(ist(2026, 11, 4, 15, 35), 15, 35), ist(2026, 11, 5, 15, 35));
        assert_eq!(next_at(ist(2026, 11, 4, 23, 59), 15, 35), ist(2026, 11, 5, 15, 35));
    }

    #[test]
    fn a_restart_inside_the_sweep_window_sweeps_at_once_with_what_is_left() {
        let secs = chrono::Duration::seconds;
        let day = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let end = ist(2026, 11, 4, 15, 35) + secs(270);
        let tomorrow = (ist(2026, 11, 5, 15, 35), ist(2026, 11, 5, 15, 35) + secs(270));
        // Before the window: today's 15:35 and the full window, first or not.
        assert_eq!(sweep_window(ist(2026, 11, 4, 9, 0), true, Some(yesterday)), (ist(2026, 11, 4, 15, 35), end));
        assert_eq!(sweep_window(ist(2026, 11, 4, 15, 34), false, None), (ist(2026, 11, 4, 15, 35), end));
        // Started inside it with yesterday's close (or none) on file: now,
        // until the window's end.
        let restart = ist(2026, 11, 4, 15, 37);
        assert_eq!(sweep_window(restart, true, Some(yesterday)), (restart, end));
        assert_eq!(sweep_window(restart, true, None), (restart, end));
        assert_eq!(sweep_window(ist(2026, 11, 4, 15, 35), true, None), (ist(2026, 11, 4, 15, 35), end));
        assert_eq!(sweep_window(end - secs(1), true, None), (end - secs(1), end));
        // Today's close already on file (the sweep finished before the
        // restart), the loop's later rounds, or the window over: tomorrow.
        assert_eq!(sweep_window(restart, true, Some(day)), tomorrow);
        assert_eq!(sweep_window(restart, false, Some(yesterday)), tomorrow);
        assert_eq!(sweep_window(end, true, None), tomorrow);
        assert_eq!(sweep_window(ist(2026, 11, 4, 20, 0), true, None), tomorrow);
    }

    fn row(key: &str, ltp: f64) -> Row {
        Row { key: key.into(), ltp, open: Some(ltp - 1.0), high: Some(ltp + 2.0), low: None, close: Some(ltp - 0.5), volume: Some(10.0) }
    }

    #[tokio::test]
    async fn closes_round_trip_through_the_catalog_database_and_the_latest_day_is_loaded() {
        let db = TempDb::new("quote-eod");
        let (pool, _) = crate::db::init_catalog_db(db.path()).await.unwrap();
        assert!(load_latest(&pool).await.unwrap().is_none());

        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let rows: HashMap<String, Row> = (1..=450).map(|i| (format!("nse_cm|{i}"), row(&format!("nse_cm|{i}"), i as f64))).collect();
        save(&pool, &Snapshot { day, taken_at: ist(2026, 11, 3, 15, 35), rows, asked: 452 }).await.unwrap();
        let loaded = load_latest(&pool).await.unwrap().unwrap();
        assert_eq!(loaded.day, day);
        assert_eq!(loaded.taken_at, ist(2026, 11, 3, 15, 35));
        assert_eq!((loaded.rows.len(), loaded.asked, loaded.partial()), (450, 452, true), "two keys got no close");
        assert_eq!(loaded.rows["nse_cm|7"], row("nse_cm|7", 7.0));
        let q = loaded.quote("nse_cm|7").unwrap();
        assert_eq!((q.ltp, q.source, q.as_of.as_deref()), (7.0, Source::Close, Some("2026-11-03 15:30")));
        assert_eq!((q.change, q.low), (Some(0.5), None));

        // The next day's sweep replaces its own day's rows and is the one loaded.
        let next = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let mut rows = HashMap::new();
        rows.insert("nse_cm|7".to_string(), row("nse_cm|7", 8.0));
        save(&pool, &Snapshot { day: next, taken_at: ist(2026, 11, 4, 15, 35), rows: rows.clone(), asked: 1 }).await.unwrap();
        rows.insert("nse_cm|8".to_string(), row("nse_cm|8", 9.0));
        save(&pool, &Snapshot { day: next, taken_at: ist(2026, 11, 4, 15, 36), rows, asked: 2 }).await.unwrap();
        let loaded = load_latest(&pool).await.unwrap().unwrap();
        assert_eq!((loaded.day, loaded.rows.len(), loaded.partial()), (next, 2, false));
        assert_eq!(loaded.rows["nse_cm|7"].ltp, 8.0);
        // Pruning drops the older day only, its sweep row with it.
        assert_eq!(prune(&pool, next).await.unwrap(), 450);
        assert_eq!(prune(&pool, next).await.unwrap(), 0);
        let days: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quote_eod_days").fetch_one(&pool).await.unwrap();
        assert_eq!(days, 1);
        assert_eq!(load_latest(&pool).await.unwrap().unwrap().rows.len(), 2);
        // A day saved before `quote_eod_days` existed counts as complete.
        sqlx::query("DELETE FROM quote_eod_days").execute(&pool).await.unwrap();
        assert!(!load_latest(&pool).await.unwrap().unwrap().partial());
        pool.close().await;
    }
}
