//! The close snapshot: at 15:35 IST one sweep of every catalog equity and
//! index, every held contract and every key watched lately, through the
//! same scheduler as the live polling, into `quote_eod`; served as the
//! day's close ("as of 15:30") once the market is shut, until the next
//! day's quotes arrive. Necessary, not a nicety: the Kotak base URL lives
//! only in the session, which the 15:40 clear drops, so no REST quote is
//! possible after it until the next login.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, Timelike};
use shared_domain::{MonitoredPosition, MARKET_CLOSE_HOUR, MARKET_CLOSE_MINUTE, MARKET_OPEN_HOUR, MARKET_OPEN_MINUTE};
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::hub::{Quote, QuoteHub, Source};
use crate::instruments::Catalog;
use crate::paper::PaperDesk;

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
}

impl Snapshot {
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

/// Whether the market is open at `now`: a weekday, 09:15 to 15:40 IST, as
/// `shared_domain::is_market_open` reads the clock.
pub fn open_at(now: DateTime<FixedOffset>) -> bool {
    if matches!(now.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        return false;
    }
    let minute = now.hour() * 60 + now.minute();
    (MARKET_OPEN_HOUR * 60 + MARKET_OPEN_MINUTE..MARKET_CLOSE_HOUR * 60 + MARKET_CLOSE_MINUTE).contains(&minute)
}

/// The next `hour:minute` IST at or after `now`: today's, or tomorrow's
/// once today's has passed.
pub fn next_at(now: DateTime<FixedOffset>, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    let today = now
        .date_naive()
        .and_hms_opt(hour, minute, 0)
        .expect("valid time")
        .and_local_timezone(shared_domain::ist_offset())
        .single()
        .expect("IST time is unambiguous");
    if today > now {
        today
    } else {
        today + chrono::Duration::hours(24)
    }
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
    let rows = rows
        .into_iter()
        .map(|(key, ltp, open, high, low, close, volume)| (key.clone(), Row { key, ltp, open, high, low, close, volume }))
        .collect();
    Ok(Some(Snapshot { day, taken_at, rows }))
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
    tx.commit().await
}

/// Drops the closes of days before `before`.
pub async fn prune(pool: &SqlitePool, before: NaiveDate) -> sqlx::Result<u64> {
    Ok(sqlx::query("DELETE FROM quote_eod WHERE day < ?").bind(day_text(before)).execute(pool).await?.rows_affected())
}

/// Runs forever: at 15:35 IST every weekday, sweeps the catalog's equities
/// and indices, every held contract (the owner's and the paper desks')
/// and every key watched lately, then writes what came back and keeps it
/// in memory as the day's close.
pub async fn run_close_sweep(
    hub: Arc<QuoteHub>,
    pool: SqlitePool,
    catalog: Arc<Catalog>,
    paper: Arc<PaperDesk>,
    positions: Arc<RwLock<Vec<MonitoredPosition>>>,
) {
    loop {
        let now = shared_domain::now_ist();
        let at = next_at(now, SWEEP_HOUR, SWEEP_MINUTE);
        let secs = (at - now).num_seconds().max(1) as u64;
        tracing::info!(secs, "Close snapshot sweep scheduled");
        tokio::time::sleep(Duration::from_secs(secs)).await;

        let now = shared_domain::now_ist();
        if matches!(now.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
            tracing::info!("Close snapshot sweep skipped — weekend");
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        let mut keys: BTreeSet<String> = catalog.cash_keys().into_iter().collect();
        let catalog_keys = keys.len();
        keys.extend(paper.feed_keys().await.0);
        keys.extend(positions.read().await.iter().filter_map(|p| p.ws_scrip_key.clone()));
        keys.extend(hub.watched_keys());
        let total = hub.start_sweep(keys, now.date_naive(), now, Instant::now() + SWEEP_WINDOW);
        tracing::info!(keys = total, catalog_keys, "Close snapshot sweep started");

        let sweep = loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Some(sweep) = hub.take_sweep_if_done(Instant::now()) {
                break sweep;
            }
        };
        let snapshot = Snapshot { day: sweep.day, taken_at: sweep.started, rows: sweep.rows };
        if snapshot.rows.is_empty() {
            tracing::warn!(
                day = %snapshot.day, keys = sweep.total,
                "Close snapshot sweep got no quotes — no Kotak session, or every call failed; the last close on file stays"
            );
        } else {
            match save(&pool, &snapshot).await {
                Ok(()) => tracing::info!(day = %snapshot.day, rows = snapshot.rows.len(), keys = sweep.total, "Close snapshot saved"),
                Err(e) => tracing::error!(day = %snapshot.day, error = %e, "Close snapshot could not be saved — served from memory until a restart"),
            }
            match prune(&pool, snapshot.day - chrono::Duration::days(KEEP_DAYS)).await {
                Ok(n) if n > 0 => tracing::info!(rows = n, "Old closes pruned"),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "quote_eod prune failed"),
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
    fn the_market_is_open_on_weekdays_from_0915_until_1540() {
        // 2026-11-04 is a Wednesday.
        assert!(!open_at(ist(2026, 11, 4, 9, 14)));
        assert!(open_at(ist(2026, 11, 4, 9, 15)));
        assert!(open_at(ist(2026, 11, 4, 15, 39)));
        assert!(!open_at(ist(2026, 11, 4, 15, 40)));
        assert!(!open_at(ist(2026, 11, 4, 20, 0)));
        assert!(!open_at(ist(2026, 11, 7, 12, 0)), "Saturday");
        assert!(!open_at(ist(2026, 11, 8, 12, 0)), "Sunday");
    }

    #[test]
    fn the_sweep_is_scheduled_for_today_or_tomorrow() {
        assert_eq!(next_at(ist(2026, 11, 4, 9, 0), 15, 35), ist(2026, 11, 4, 15, 35));
        assert_eq!(next_at(ist(2026, 11, 4, 15, 34), 15, 35), ist(2026, 11, 4, 15, 35));
        assert_eq!(next_at(ist(2026, 11, 4, 15, 35), 15, 35), ist(2026, 11, 5, 15, 35));
        assert_eq!(next_at(ist(2026, 11, 4, 23, 59), 15, 35), ist(2026, 11, 5, 15, 35));
    }

    fn row(key: &str, ltp: f64) -> Row {
        Row { key: key.into(), ltp, open: Some(ltp - 1.0), high: Some(ltp + 2.0), low: None, close: Some(ltp - 0.5), volume: Some(10.0) }
    }

    #[tokio::test]
    async fn closes_round_trip_through_the_table_and_the_latest_day_is_loaded() {
        let db = TempDb::new("quote-eod");
        let pool = crate::db::init_db(&db.url()).await;
        assert!(load_latest(&pool).await.unwrap().is_none());

        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let rows: HashMap<String, Row> = (1..=450).map(|i| (format!("nse_cm|{i}"), row(&format!("nse_cm|{i}"), i as f64))).collect();
        save(&pool, &Snapshot { day, taken_at: ist(2026, 11, 3, 15, 35), rows }).await.unwrap();
        let loaded = load_latest(&pool).await.unwrap().unwrap();
        assert_eq!(loaded.day, day);
        assert_eq!(loaded.taken_at, ist(2026, 11, 3, 15, 35));
        assert_eq!(loaded.rows.len(), 450);
        assert_eq!(loaded.rows["nse_cm|7"], row("nse_cm|7", 7.0));
        let q = loaded.quote("nse_cm|7").unwrap();
        assert_eq!((q.ltp, q.source, q.as_of.as_deref()), (7.0, Source::Close, Some("2026-11-03 15:30")));
        assert_eq!((q.change, q.low), (Some(0.5), None));

        // The next day's sweep replaces its own day's rows and is the one loaded.
        let next = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let mut rows = HashMap::new();
        rows.insert("nse_cm|7".to_string(), row("nse_cm|7", 8.0));
        save(&pool, &Snapshot { day: next, taken_at: ist(2026, 11, 4, 15, 35), rows: rows.clone() }).await.unwrap();
        rows.insert("nse_cm|8".to_string(), row("nse_cm|8", 9.0));
        save(&pool, &Snapshot { day: next, taken_at: ist(2026, 11, 4, 15, 36), rows }).await.unwrap();
        let loaded = load_latest(&pool).await.unwrap().unwrap();
        assert_eq!((loaded.day, loaded.rows.len()), (next, 2));
        assert_eq!(loaded.rows["nse_cm|7"].ltp, 8.0);
        // Pruning drops the older day only.
        assert_eq!(prune(&pool, next).await.unwrap(), 450);
        assert_eq!(prune(&pool, next).await.unwrap(), 0);
        assert_eq!(load_latest(&pool).await.unwrap().unwrap().rows.len(), 2);
        pool.close().await;
    }
}
