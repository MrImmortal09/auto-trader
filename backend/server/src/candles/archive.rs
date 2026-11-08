//! The permanent 1-minute candle archive: the six indices and every listed
//! future on them, with Upstox's volume and open interest, kept forever.
//!
//! Upstox's free candles stop once a contract expires (expired contracts need
//! Upstox Plus), and an index's VWAP is built from its future's volume, so a
//! past day's index VWAP can only be rebuilt from what is saved here. Each
//! future is kept under its own contract, `NIFTY:FUT:2026-11-25`, so which
//! month was the nearest on a day is read from the names, never guessed.
//!
//! Analytics only: it reads Upstox and writes `candle_archive` through the
//! single DB writer. It never touches Kotak, orders or positions, and it stays
//! out of the trading day: it sweeps at 15:50 IST, after the close, and at
//! startup only outside 09:00–15:50 on a weekday.
//!
//! Each sweep fetches, per symbol, from its last archived day (fetched again,
//! in case a sweep was cut short) to the last finished day. A symbol never
//! archived goes back [`BACKFILL_DAYS`], or to its listing if that is later.

use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, FixedOffset, NaiveDate, Weekday};
use shared_domain::{ArchivedCandle, DbWriteMessage, INDEX_NAMES};
use sqlx::sqlite::SqlitePool;
use tokio::sync::mpsc;
use upstox_client::{Candle, UpstoxClient};

use super::symbols::upstox_index_key;
use super::UPSTOX_INDEX_FILES;

/// The daily sweep, IST: ten minutes after the 15:40 close, once Upstox's
/// last minute is final.
const SWEEP_AT: (u32, u32) = (15, 50);
/// From here to [`SWEEP_AT`] on a weekday, the startup sweep waits for the
/// daily one (09:00 is the first Kotak session trigger).
const DAY_STARTS_AT: (u32, u32) = (9, 0);
/// Lets the rest of the server finish starting first.
const STARTUP_DELAY: Duration = Duration::from_secs(120);
/// How far back a symbol never archived is filled.
const BACKFILL_DAYS: i64 = 92;
/// Upstox serves at most a month of 1-minute candles per request.
const REQUEST_SPAN_DAYS: i64 = 28;
/// Between Upstox requests: the Market poller and the charts share its limits.
const REQUEST_GAP: Duration = Duration::from_millis(300);
/// Rows per writer message, so the writer is never held for long.
const WRITE_BATCH: usize = 500;
/// The engine shares the writer's queue: a batch joins it only while it holds
/// no more than this many messages, so a backfill can never fill it.
const MAX_QUEUED: usize = 8;
const MINUTE_MS: i64 = 60_000;

/// Runs forever: a sweep at startup (outside the trading day), then one each
/// day at [`SWEEP_AT`], weekends included — a sweep with nothing new to fetch
/// costs a request per symbol.
pub async fn run(client: UpstoxClient, pool: SqlitePool, db_tx: mpsc::Sender<DbWriteMessage>) {
    tokio::time::sleep(STARTUP_DELAY).await;
    if trading_day_under_way(shared_domain::now_ist()) {
        tracing::info!("Candle archive: the startup sweep waits for 15:50 IST, the market day is under way");
    } else {
        sweep(&client, &pool, &db_tx).await;
    }
    loop {
        let now = shared_domain::now_ist();
        let wait = (next_sweep_at(now) - now).to_std().unwrap_or(Duration::from_secs(1));
        tokio::time::sleep(wait).await;
        sweep(&client, &pool, &db_tx).await;
    }
}

/// One Upstox request in a sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Request {
    /// Past days, inclusive.
    Past(NaiveDate, NaiveDate),
    /// Today's finished minutes.
    Today,
}

async fn sweep(client: &UpstoxClient, pool: &SqlitePool, db_tx: &mpsc::Sender<DbWriteMessage>) {
    let now = shared_domain::now_ist();
    let symbols = symbols(client).await;
    tracing::info!(symbols = symbols.len(), "Candle archive sweep started");
    let (mut saved, mut failed) = (0usize, 0usize);
    for (symbol, key) in &symbols {
        let last_archived = match crate::db::last_archived_minute(pool, symbol).await {
            Ok(t) => t.and_then(ist_day),
            Err(e) => {
                tracing::warn!(symbol, error = %e, "Candle archive: could not read the last archived minute");
                failed += 1;
                continue;
            }
        };
        for request in plan(last_archived, now) {
            let result = match request {
                Request::Past(from, to) => client.historical_candles(key, 1, from, to).await,
                Request::Today => client.intraday_candles(key, 1).await,
            };
            tokio::time::sleep(REQUEST_GAP).await;
            match result {
                Ok(rows) => {
                    let rows = finished(&rows, now.timestamp_millis());
                    saved += rows.len();
                    write(db_tx, symbol, rows).await;
                }
                Err(e) if e.is_unauthorized() => {
                    tracing::warn!(error = %e, "Candle archive: Upstox refused the analytics token — sweep stopped");
                    return;
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(symbol, ?request, error = %e, "Candle archive: Upstox candle request failed");
                }
            }
        }
    }
    tracing::info!(symbols = symbols.len(), candles = saved, failed, "Candle archive sweep done");
}

/// Every symbol to archive and its Upstox key: the indices, then each listed
/// future. A failed instruments download only leaves that exchange's futures
/// out of this sweep.
async fn symbols(client: &UpstoxClient) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = INDEX_NAMES
        .iter()
        .filter_map(|index| Some((index.to_string(), upstox_index_key(index)?.to_string())))
        .collect();
    for (url, wanted) in UPSTOX_INDEX_FILES {
        match client.index_futures(url, wanted).await {
            Ok(list) => out.extend(list.into_iter().map(|f| (future_name(&f.underlying, f.expiry), f.instrument_key))),
            Err(e) => tracing::warn!(error = %e, url, "Candle archive: Upstox instruments download failed — its futures wait for the next sweep"),
        }
    }
    out
}

/// The archive's name for a future: `NIFTY:FUT:2026-11-25`.
fn future_name(index: &str, expiry: NaiveDate) -> String {
    format!("{index}:FUT:{}", expiry.format("%Y-%m-%d"))
}

/// What to fetch for a symbol last archived on `last_archived`: the past days
/// from then (or [`BACKFILL_DAYS`] back) through yesterday, in month-sized
/// requests, then today's minutes once today has finished.
fn plan(last_archived: Option<NaiveDate>, now: DateTime<FixedOffset>) -> Vec<Request> {
    let today = now.date_naive();
    let last_day = last_finished_day(now);
    let earliest = last_day - ChronoDuration::days(BACKFILL_DAYS);
    let from = last_archived.map_or(earliest, |d| d.max(earliest));
    let yesterday = today - ChronoDuration::days(1);
    let mut out: Vec<Request> = spans(from, last_day.min(yesterday)).into_iter().map(|(a, b)| Request::Past(a, b)).collect();
    if last_day == today {
        out.push(Request::Today);
    }
    out
}

/// `from..=to` in consecutive requests of at most [`REQUEST_SPAN_DAYS`] days.
fn spans(from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out = Vec::new();
    let mut start = from;
    while start <= to {
        let end = (start + ChronoDuration::days(REQUEST_SPAN_DAYS - 1)).min(to);
        out.push((start, end));
        start = end + ChronoDuration::days(1);
    }
    out
}

/// Today once the sweep time has passed, otherwise yesterday.
fn last_finished_day(now: DateTime<FixedOffset>) -> NaiveDate {
    let today = now.date_naive();
    if now >= at(today, SWEEP_AT) {
        today
    } else {
        today - ChronoDuration::days(1)
    }
}

/// A weekday between [`DAY_STARTS_AT`] and [`SWEEP_AT`]: the Kotak session is
/// being set up or the market is open, so the archive keeps off the writer.
fn trading_day_under_way(now: DateTime<FixedOffset>) -> bool {
    let today = now.date_naive();
    let weekday = !matches!(today.weekday(), Weekday::Sat | Weekday::Sun);
    weekday && now >= at(today, DAY_STARTS_AT) && now < at(today, SWEEP_AT)
}

/// The next [`SWEEP_AT`] strictly after `now`.
fn next_sweep_at(now: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    let today = at(now.date_naive(), SWEEP_AT);
    if now < today {
        today
    } else {
        at(now.date_naive() + ChronoDuration::days(1), SWEEP_AT)
    }
}

fn at(day: NaiveDate, (hour, minute): (u32, u32)) -> DateTime<FixedOffset> {
    day.and_hms_opt(hour, minute, 0)
        .expect("valid time")
        .and_local_timezone(shared_domain::ist_offset())
        .single()
        .expect("IST is unambiguous")
}

/// The IST day of a Unix-ms instant.
fn ist_day(ms: i64) -> Option<NaiveDate> {
    Some(DateTime::from_timestamp_millis(ms)?.with_timezone(&shared_domain::ist_offset()).date_naive())
}

/// The candles whose minute has ended by `now_ms`, for the archive.
fn finished(rows: &[Candle], now_ms: i64) -> Vec<ArchivedCandle> {
    rows.iter()
        .filter(|c| c.start_ms + MINUTE_MS <= now_ms)
        .map(|c| ArchivedCandle {
            t: c.start_ms,
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
            volume: c.volume,
            oi: c.oi,
        })
        .collect()
}

/// Hands `candles` to the DB writer in small batches, each only once the
/// writer's queue is all but empty.
async fn write(db_tx: &mpsc::Sender<DbWriteMessage>, symbol: &str, candles: Vec<ArchivedCandle>) {
    for batch in candles.chunks(WRITE_BATCH) {
        while db_tx.max_capacity() - db_tx.capacity() > MAX_QUEUED {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let msg = DbWriteMessage::ArchiveCandles { symbol: symbol.to_string(), candles: batch.to_vec() };
        if db_tx.send(msg).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn ist(m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<FixedOffset> {
        date(m, d).and_hms_opt(h, min, s).unwrap().and_local_timezone(shared_domain::ist_offset()).single().unwrap()
    }

    #[test]
    fn spans_cover_the_range_in_month_sized_requests() {
        let (from, to) = (date(8, 7), date(11, 6));
        let spans = spans(from, to);
        assert_eq!(spans.first().unwrap().0, from);
        assert_eq!(spans.last().unwrap().1, to);
        for (a, b) in &spans {
            assert!(a <= b && (*b - *a).num_days() < REQUEST_SPAN_DAYS, "{a}..{b}");
        }
        for pair in spans.windows(2) {
            assert_eq!(pair[0].1 + ChronoDuration::days(1), pair[1].0, "no gap, no overlap");
        }
        assert_eq!(spans.len(), 4);
        assert_eq!(super::spans(to, to), [(to, to)]);
        assert!(super::spans(to, from).is_empty());
    }

    #[test]
    fn a_day_counts_as_finished_from_the_sweep_time() {
        assert_eq!(last_finished_day(ist(11, 6, 15, 49, 59)), date(11, 5));
        assert_eq!(last_finished_day(ist(11, 6, 15, 50, 0)), date(11, 6));
        assert_eq!(last_finished_day(ist(11, 6, 0, 30, 0)), date(11, 5));
    }

    #[test]
    fn the_startup_sweep_keeps_out_of_the_trading_day() {
        // 2026-11-06 is a Friday.
        assert_eq!(date(11, 6).weekday(), Weekday::Fri);
        assert!(!trading_day_under_way(ist(11, 6, 8, 59, 59)));
        assert!(trading_day_under_way(ist(11, 6, 9, 0, 0)));
        assert!(trading_day_under_way(ist(11, 6, 15, 49, 59)));
        assert!(!trading_day_under_way(ist(11, 6, 15, 50, 0)));
        assert!(!trading_day_under_way(ist(11, 7, 12, 0, 0)), "Saturday");
    }

    #[test]
    fn the_next_sweep_is_today_until_it_has_run() {
        assert_eq!(next_sweep_at(ist(11, 6, 10, 0, 0)), ist(11, 6, 15, 50, 0));
        assert_eq!(next_sweep_at(ist(11, 6, 15, 50, 0)), ist(11, 7, 15, 50, 0));
        assert_eq!(next_sweep_at(ist(11, 6, 23, 0, 0)), ist(11, 7, 15, 50, 0));
    }

    #[test]
    fn a_new_symbol_is_backfilled_and_a_known_one_refetches_its_last_day() {
        // Never archived, swept after the close: 92 days back to yesterday,
        // then today.
        let after_close = ist(11, 6, 15, 50, 0);
        let first = plan(None, after_close);
        assert_eq!(date(11, 6) - ChronoDuration::days(BACKFILL_DAYS), date(8, 6));
        assert_eq!(
            first,
            [
                Request::Past(date(8, 6), date(9, 2)),
                Request::Past(date(9, 3), date(9, 30)),
                Request::Past(date(10, 1), date(10, 28)),
                Request::Past(date(10, 29), date(11, 5)),
                Request::Today,
            ],
        );

        // Archived through yesterday: yesterday again, then today.
        assert_eq!(plan(Some(date(11, 5)), after_close), [Request::Past(date(11, 5), date(11, 5)), Request::Today]);
        // Already swept today (a restart in the evening): today again only.
        assert_eq!(plan(Some(date(11, 6)), after_close), [Request::Today]);
        // Before the close, today is left for the 15:50 sweep.
        assert_eq!(plan(Some(date(11, 4)), ist(11, 6, 8, 0, 0)), [Request::Past(date(11, 4), date(11, 5))]);
        // An archive older than the backfill window starts at the window.
        let stale = plan(Some(date(1, 2)), after_close);
        assert_eq!(stale.first(), first.first());
    }

    #[test]
    fn futures_are_named_by_contract() {
        assert_eq!(future_name("NIFTY", date(11, 24)), "NIFTY:FUT:2026-11-24");
    }

    #[test]
    fn only_finished_minutes_are_archived() {
        let candle = |start_ms| Candle { start_ms, open: 1.0, high: 2.0, low: 0.5, close: 1.5, volume: 900.0, oi: 12.0 };
        let now = ist(11, 6, 15, 30, 30).timestamp_millis();
        let done = ist(11, 6, 15, 29, 0).timestamp_millis();
        let forming = ist(11, 6, 15, 30, 0).timestamp_millis();
        let kept = finished(&[candle(done), candle(forming)], now);
        assert_eq!(kept, [ArchivedCandle { t: done, open: 1.0, high: 2.0, low: 0.5, close: 1.5, volume: 900.0, oi: 12.0 }]);
    }

    #[test]
    fn days_are_read_in_ist() {
        // 2026-11-05 18:45 UTC is 00:15 on the 6th in IST.
        let ms = ist(11, 6, 0, 15, 0).timestamp_millis();
        assert_eq!(ist_day(ms), Some(date(11, 6)));
    }
}
