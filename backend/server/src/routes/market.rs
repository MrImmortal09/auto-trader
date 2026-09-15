//! Market page API — Upstox option-chain analytics (read-only).
//!
//! - `GET /api/market/live`                          — latest state for all indices
//! - `GET /api/market/intraday-trend?underlying=&interval=&window=&date=`
//! - `GET /api/market/option-chain?underlying=`      — latest per-strike chain

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::market_data::{aggregate, IndexLive, StrikeRow, WindowAgg, INDICES, STRIKE_WINDOWS};
use crate::AppState;

fn known_index(id: &str) -> Option<&'static str> {
    INDICES.iter().map(|i| i.id).find(|k| k.eq_ignore_ascii_case(id))
}

fn bad_request(msg: &str) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg }))).into_response()
}

// ---------------------------------------------------------------------------
// /api/market/live
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct LiveIndexDto {
    underlying: &'static str,
    #[serde(flatten)]
    live: IndexLive,
    windows: Vec<WindowAgg>,
}

#[derive(Serialize)]
struct LiveDto {
    configured: bool,
    indices: Vec<LiveIndexDto>,
}

pub async fn market_live_handler(State(state): State<AppState>) -> impl IntoResponse {
    let guard = state.market.indices.read().await;
    let indices = INDICES
        .iter()
        .map(|spec| {
            let live = guard.get(spec.id).cloned().unwrap_or_default();
            let windows = match live.spot {
                Some(spot) if !live.chain.is_empty() => {
                    STRIKE_WINDOWS.iter().map(|w| aggregate(&live.chain, spot, *w)).collect()
                }
                _ => Vec::new(),
            };
            LiveIndexDto { underlying: spec.id, live, windows }
        })
        .collect();
    Json(LiveDto { configured: state.market.configured, indices })
}

// ---------------------------------------------------------------------------
// /api/market/option-chain
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ChainQuery {
    underlying: String,
}

#[derive(Serialize)]
struct ChainDto {
    underlying: &'static str,
    expiry: Option<String>,
    spot: Option<f64>,
    atm_strike: Option<f64>,
    updated_at: Option<String>,
    strikes: Vec<StrikeRow>,
}

pub async fn market_option_chain_handler(
    State(state): State<AppState>,
    Query(q): Query<ChainQuery>,
) -> axum::response::Response {
    let Some(id) = known_index(&q.underlying) else {
        return bad_request("unknown underlying");
    };
    let live = state.market.indices.read().await.get(id).cloned().unwrap_or_default();
    Json(ChainDto {
        underlying: id,
        expiry: live.expiry,
        spot: live.spot,
        atm_strike: live.atm_strike,
        updated_at: live.updated_at,
        strikes: live.chain,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// /api/market/intraday-trend
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TrendQuery {
    underlying: String,
    interval: Option<u32>,
    window: Option<i64>,
    date: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SnapshotRow {
    ts: String,
    expiry: String,
    call_oi: f64,
    put_oi: f64,
    call_oi_chg: f64,
    put_oi_chg: f64,
    spot: f64,
    fut_ltp: Option<f64>,
    fut_vwap: Option<f64>,
}

#[derive(Serialize)]
struct TrendRowDto {
    /// Bucket label `HH:MM`; for the still-open latest bucket, the time of the
    /// snapshot it currently holds (and `live = true`).
    time: String,
    live: bool,
    ts: String,
    expiry: String,
    call_oi: f64,
    put_oi: f64,
    call_oi_chg: f64,
    put_oi_chg: f64,
    spot: f64,
    fut_ltp: Option<f64>,
    fut_vwap: Option<f64>,
}

#[derive(Serialize)]
struct TrendDto {
    underlying: &'static str,
    date: Option<String>,
    interval: u32,
    window: i64,
    /// Newest first.
    rows: Vec<TrendRowDto>,
}

pub const TREND_INTERVALS: [u32; 4] = [5, 15, 30, 60];

/// First check of the day: one minute after the 09:15 open.
const OPEN_CHECK_MINUTE: u32 = 9 * 60 + 16;
/// Last check of the day: one minute after the 15:30 close. Caps intervals that
/// would otherwise land after the market (60 min → 16:01).
const CLOSE_CHECK_MINUTE: u32 = 15 * 60 + 31;

/// The check a snapshot taken at `minute` (of day, IST) belongs to: the first
/// "interval boundary + 1 minute" at or after it — 09:16, 09:31, 09:46… for
/// 15 min. Boundaries are clock-aligned multiples of `interval`; 09:16 is
/// always the first check and 15:31 always the last, regardless of interval
/// (so 60 min → 09:16, 10:01, 11:01 … 15:01, 15:31).
/// The +1 lets the boundary minute fully complete before it is read.
fn check_minute(minute: u32, interval: u32) -> u32 {
    if minute <= OPEN_CHECK_MINUTE {
        return OPEN_CHECK_MINUTE;
    }
    ((minute - 1).div_ceil(interval) * interval + 1).min(CLOSE_CHECK_MINUTE.max(minute))
}

fn minute_of_day(ts: &str) -> Option<u32> {
    let h: u32 = ts.get(11..13)?.parse().ok()?;
    let m: u32 = ts.get(14..16)?.parse().ok()?;
    Some(h * 60 + m)
}

pub async fn market_trend_handler(
    State(state): State<AppState>,
    Query(q): Query<TrendQuery>,
) -> axum::response::Response {
    let Some(id) = known_index(&q.underlying) else {
        return bad_request("unknown underlying");
    };
    let interval = q.interval.unwrap_or(15);
    if !TREND_INTERVALS.contains(&interval) {
        return bad_request("interval must be one of 5, 15, 30, 60");
    }
    let window = q.window.unwrap_or(10);
    if !STRIKE_WINDOWS.contains(&window) {
        return bad_request("window must be one of 5, 10, 20, 0");
    }

    // Default to today, falling back to the most recent day with data
    // (weekends / before the first poll).
    let date = match q.date.filter(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()) {
        Some(d) => Some(d),
        None => {
            let today = shared_domain::today_ist().format("%Y-%m-%d").to_string();
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT MAX(substr(ts, 1, 10)) FROM oi_snapshots WHERE underlying = ? AND ts <= ?",
            )
            .bind(id)
            .bind(format!("{today} 23:59:59"))
            .fetch_one(&state.db_pool)
            .await
            .ok()
            .flatten()
        }
    };

    let Some(date) = date else {
        return Json(TrendDto { underlying: id, date: None, interval, window, rows: Vec::new() }).into_response();
    };

    let rows = match sqlx::query_as::<_, SnapshotRow>(
        "SELECT ts, expiry, call_oi, put_oi, call_oi_chg, put_oi_chg, spot, fut_ltp, fut_vwap
         FROM oi_snapshots
         WHERE underlying = ? AND strike_window = ? AND ts >= ? AND ts <= ?
         ORDER BY ts ASC",
    )
    .bind(id)
    .bind(window)
    .bind(format!("{date} 00:00:00"))
    .bind(format!("{date} 23:59:59"))
    .fetch_all(&state.db_pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("oi_snapshots query: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "query failed" })))
                .into_response();
        }
    };

    // Row label = the check minute at or after the snapshot, so the "11:01" row
    // (15 min) holds the last snapshot taken in (10:46, 11:01] — normally the
    // one taken at 11:01 itself.
    let mut buckets: BTreeMap<u32, SnapshotRow> = BTreeMap::new();
    for r in rows {
        let Some(m) = minute_of_day(&r.ts) else { continue };
        buckets.insert(check_minute(m, interval), r);
    }

    // A check whose snapshot hasn't been taken yet today is still open ("live").
    let now = shared_domain::now_ist();
    let now_minute = (now.format("%Y-%m-%d").to_string() == date)
        .then(|| minute_of_day(&now.format("%Y-%m-%d %H:%M:%S").to_string()))
        .flatten();

    let out = buckets
        .into_iter()
        .rev()
        .map(|(label, r)| {
            let snap_min = minute_of_day(&r.ts).unwrap_or(label);
            // Open until the snapshot for the label minute itself has landed
            // (the poller writes it a few seconds into that minute). Once the
            // label minute has passed, the bucket is closed either way.
            let live = snap_min < label && now_minute.is_some_and(|n| n <= label);
            let shown = if live { snap_min } else { label };
            TrendRowDto {
                time: format!("{:02}:{:02}", shown / 60, shown % 60),
                live,
                ts: r.ts,
                expiry: r.expiry,
                call_oi: r.call_oi,
                put_oi: r.put_oi,
                call_oi_chg: r.call_oi_chg,
                put_oi_chg: r.put_oi_chg,
                spot: r.spot,
                fut_ltp: r.fut_ltp,
                fut_vwap: r.fut_vwap,
            }
        })
        .collect();

    Json(TrendDto { underlying: id, date: Some(date), interval, window, rows: out }).into_response()
}

#[cfg(test)]
mod tests {
    use super::check_minute;

    const fn hm(h: u32, m: u32) -> u32 {
        h * 60 + m
    }

    #[test]
    fn checks_one_minute_after_each_boundary() {
        // Opening snapshots all belong to the 09:16 check, for every interval.
        for iv in [5, 15, 30, 60] {
            assert_eq!(check_minute(hm(9, 15), iv), hm(9, 16));
            assert_eq!(check_minute(hm(9, 16), iv), hm(9, 16));
        }
        assert_eq!(check_minute(hm(9, 17), 5), hm(9, 21));
        assert_eq!(check_minute(hm(9, 21), 5), hm(9, 21));
        assert_eq!(check_minute(hm(9, 17), 15), hm(9, 31));
        assert_eq!(check_minute(hm(9, 31), 15), hm(9, 31));
        assert_eq!(check_minute(hm(9, 32), 15), hm(9, 46));
        assert_eq!(check_minute(hm(9, 17), 30), hm(9, 31));
        assert_eq!(check_minute(hm(9, 32), 30), hm(10, 1));
        assert_eq!(check_minute(hm(9, 17), 60), hm(10, 1));
        assert_eq!(check_minute(hm(10, 2), 60), hm(11, 1));
        assert_eq!(check_minute(hm(15, 30), 15), hm(15, 31));
        // 60 min: the afternoon tail closes at 15:31, not 16:01.
        assert_eq!(check_minute(hm(15, 1), 60), hm(15, 1));
        assert_eq!(check_minute(hm(15, 2), 60), hm(15, 31));
        assert_eq!(check_minute(hm(15, 31), 60), hm(15, 31));
    }
}
