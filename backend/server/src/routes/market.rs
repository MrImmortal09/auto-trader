//! Market page API — Upstox option-chain analytics (read-only).
//!
//! - `GET /api/market/live`                          — latest state for all indices
//! - `GET /api/market/intraday-trend?underlying=&interval=&window=&date=`
//! - `GET /api/market/option-chain?underlying=`      — latest per-strike chain
//! - `GET /api/market/indicator`                     — last closed VWAP + PCR check per
//!   index and the CE / PE sizing verdict the engine would apply right now

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use shared_domain::entry_gate::{
    self, check_minute, Bias, IndicatorReading, SizingVerdict, INDICATOR_INDICES, INDICATOR_INTERVALS,
};

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

/// Same set the indicator (and the Settings page) accepts.
pub const TREND_INTERVALS: [u32; 4] = INDICATOR_INTERVALS;

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

// ---------------------------------------------------------------------------
// /api/market/indicator — what the entry gate would decide right now
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CallVerdictDto {
    /// `"SCALE"` (buy, scaled) or `"SKIP"` (the call would be dropped).
    action: &'static str,
    multiplier: Option<f64>,
    /// The index's default lot count before the indicator.
    base_lots: i32,
    /// `base_lots` × `multiplier` after rounding — what would actually be bought.
    lots: Option<i32>,
    /// The same explanation the engine logs.
    note: String,
}

#[derive(Serialize)]
struct IndicatorIndexDto {
    index: &'static str,
    /// The last closed check, or `None` when there is no fresh one.
    reading: Option<IndicatorReading>,
    bias: Bias,
    ce: CallVerdictDto,
    pe: CallVerdictDto,
}

#[derive(Serialize)]
struct IndicatorDto {
    /// Upstox market data is configured at all.
    configured: bool,
    /// `TradingConfig::indicator_sizing`.
    enabled: bool,
    /// IST time the verdicts were computed for.
    now: String,
    indices: Vec<IndicatorIndexDto>,
}

/// `GET /api/market/indicator` — each index's last closed check and the
/// BUY CE / BUY PE verdict a call triggering right now would get, computed by
/// the very `entry_gate` code the position monitor runs (with the saved
/// settings). Read-only.
pub async fn market_indicator_handler(State(state): State<AppState>) -> axum::response::Response {
    let cfg = state.trading_cfg.read().await.clone();
    let params = cfg.indicator_params();
    let now = shared_domain::current_ist_timestamp_string();

    let indices = INDICATOR_INDICES
        .iter()
        .map(|&index| {
            let reading = entry_gate::feed_reading(&state.indicator_feed, index, &params, &now);
            let base_lots = trading_engine::lots_for_instrument(&cfg, index);
            let verdict = |option_type: &str| match entry_gate::sizing_verdict(
                option_type,
                index,
                reading.as_ref(),
                &cfg.indicator_multipliers(),
            ) {
                SizingVerdict::Scale { multiplier, note, .. } => CallVerdictDto {
                    action: "SCALE",
                    multiplier: Some(multiplier),
                    base_lots,
                    lots: Some(entry_gate::scale_lots(base_lots, multiplier)),
                    note,
                },
                // Disagreement: the call keeps waiting or is dropped, per
                // `indicator_wait_on_disagree` — never bought either way.
                SizingVerdict::Skip { reason } => CallVerdictDto {
                    action: if cfg.indicator_wait_on_disagree { "WAIT" } else { "SKIP" },
                    multiplier: None,
                    base_lots,
                    lots: None,
                    note: reason,
                },
            };
            let ce = verdict("CE");
            let pe = verdict("PE");
            let bias = reading.as_ref().map_or(Bias::Neutral, |r| r.bias);
            IndicatorIndexDto { index, reading, bias, ce, pe }
        })
        .collect();

    Json(IndicatorDto { configured: state.market.configured, enabled: cfg.indicator_sizing, now, indices })
        .into_response()
}
