//! `GET /api/v2/expiries`, `GET /api/v2/chain` and `GET /api/v2/marks` —
//! what the v2 Trade charts need beyond bars: an index's listed expiries,
//! one expiry's strikes with each option's delta, and today's fills,
//! Telegram calls and Telegram positions to draw. Read-only.

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared_domain::{TradeState, INDEX_NAMES};

use crate::history;
use crate::market_data::StrikeRow;
use crate::AppState;

/// The delta assumed for a call when Upstox has none (a put's is minus this).
const ASSUMED_DELTA: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Delta {
    /// Signed: a put's is negative.
    pub value: f64,
    /// `ASSUMED_DELTA`, not Upstox's.
    pub assumed: bool,
}

/// A side's delta: Upstox's from its option chain when it has one for this
/// strike, else the assumed 0.50.
pub fn delta_of(row: Option<&StrikeRow>, call: bool) -> Delta {
    let upstox = row.map(|r| if call { r.call_delta } else { r.put_delta }).filter(|d| d.is_finite() && *d != 0.0);
    match upstox {
        Some(value) => Delta { value, assumed: false },
        None => Delta { value: if call { ASSUMED_DELTA } else { -ASSUMED_DELTA }, assumed: true },
    }
}

#[derive(Serialize)]
struct Side {
    /// The broker's trading symbol — what a Buy sends.
    symbol: String,
    delta: Delta,
}

#[derive(Serialize)]
struct StrikeOut {
    strike: f64,
    ce: Option<Side>,
    pe: Option<Side>,
}

#[derive(Serialize)]
struct ChainAnswer {
    index: String,
    /// `YYYY-MM-DD`.
    expiry: String,
    lot_size: u32,
    tick_size: f64,
    /// When Upstox's chain was last read, if its deltas are used.
    delta_at: Option<String>,
    strikes: Vec<StrikeOut>,
}

#[derive(Deserialize)]
pub struct ChainQuery {
    index: String,
    /// `YYYY-MM-DD`; left out, the nearest expiry.
    expiry: Option<String>,
}

fn index_named(wanted: &str) -> Option<&'static str> {
    let wanted = wanted.trim().to_uppercase();
    INDEX_NAMES.iter().copied().find(|i| *i == wanted)
}

#[derive(Serialize)]
struct ExpiryOut {
    /// `YYYY-MM-DD`.
    date: String,
    days_to_expiry: i64,
    lot_size: u32,
}

#[derive(Serialize)]
struct ExpiriesAnswer {
    index: String,
    /// `YYYY-MM-DD`, IST.
    today: String,
    /// Earliest first; the first is the nearest.
    expiries: Vec<ExpiryOut>,
}

/// `GET /api/v2/expiries?index=NIFTY` — every expiry this index's options
/// are listed for from today, earliest first, with each one's lot size.
pub async fn expiries_handler(State(state): State<AppState>, Query(q): Query<ChainQuery>) -> Response {
    let Some(index) = index_named(&q.index) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "index must be one of the six indices" }))).into_response();
    };
    if state.scrip_store.read().await.is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "no option chain yet — the scrip master loads after the Kotak login" }))).into_response();
    }
    let today = shared_domain::today_ist();
    let mut expiries = Vec::new();
    for &date in state.execute.expiries(&state, index).await.iter() {
        let Some(chain) = state.execute.chain_at(&state, index, date).await else { continue };
        expiries.push(ExpiryOut {
            date: date.format("%Y-%m-%d").to_string(),
            days_to_expiry: (date - today).num_days(),
            lot_size: chain.lot_size,
        });
    }
    Json(ExpiriesAnswer { index: index.to_string(), today: today.format("%Y-%m-%d").to_string(), expiries }).into_response()
}

/// `GET /api/v2/chain?index=NIFTY&expiry=` — one expiry's strikes (the
/// nearest by default), as the Execute screen trades them, each side with
/// its symbol and delta (Upstox's, read once a minute, or an assumed ±0.50).
pub async fn chain_handler(State(state): State<AppState>, Query(q): Query<ChainQuery>) -> Response {
    let Some(index) = index_named(&q.index) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "index must be one of the six indices" }))).into_response();
    };
    let chain = match q.expiry.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") else {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": "expiry must be YYYY-MM-DD" }))).into_response();
            };
            match state.execute.chain_at(&state, index, date).await {
                Some(c) => c,
                None => return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no {index} options listed for {s}") }))).into_response(),
            }
        }
        None => match state.execute.chain(&state, index).await {
            Some(c) => c,
            None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "no option chain yet — the scrip master loads after the Kotak login" }))).into_response(),
        },
    };
    let expiry = chain.expiry.format("%Y-%m-%d").to_string();
    // Upstox's deltas only count for the same expiry.
    let (rows, delta_at): (HashMap<i64, StrikeRow>, Option<String>) = {
        let live = state.market.indices.read().await;
        match live.get(index).filter(|l| l.expiry.as_deref() == Some(expiry.as_str())) {
            Some(l) => (l.chain.iter().map(|r| ((r.strike * 100.0).round() as i64, r.clone())).collect(), l.updated_at.clone()),
            None => (HashMap::new(), None),
        }
    };
    let strikes = chain
        .strikes
        .iter()
        .map(|s| {
            let row = rows.get(&((s.strike * 100.0).round() as i64));
            StrikeOut {
                strike: s.strike,
                ce: s.ce.as_ref().map(|r| Side { symbol: r.trading_symbol.clone(), delta: delta_of(row, true) }),
                pe: s.pe.as_ref().map(|r| Side { symbol: r.trading_symbol.clone(), delta: delta_of(row, false) }),
            }
        })
        .collect();
    Json(ChainAnswer {
        index: index.to_string(),
        expiry,
        lot_size: chain.lot_size,
        tick_size: chain.tick_size,
        delta_at: if rows.is_empty() { None } else { delta_at },
        strikes,
    })
    .into_response()
}

#[derive(Serialize)]
struct FillMark {
    id: i64,
    t: String,
    action: String,
    qty: i64,
    price: f64,
    index: String,
    strike: Option<f64>,
    side: Option<String>,
    /// An Execute-screen scalp's, not a Telegram call's.
    scalp: bool,
    reason: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct CallMark {
    id: i64,
    /// When the call was sent, IST `YYYY-MM-DD HH:MM:SS`.
    #[sqlx(rename = "sent_at")]
    t: String,
    #[sqlx(rename = "chat_title")]
    channel: Option<String>,
    #[sqlx(rename = "instrument")]
    index: String,
    strike: Option<f64>,
    side: Option<String>,
    text: String,
}

#[derive(Serialize)]
struct TelegramLine {
    id: String,
    index: String,
    strike: Option<f64>,
    side: Option<String>,
    /// `YYYY-MM-DD`, when the contract was resolved.
    expiry: Option<String>,
    /// `waiting`, `active` or `target1`.
    state: &'static str,
    channel: String,
    qty: i32,
    avg: f64,
    /// The call's entry price.
    entry: f64,
    ltp: Option<f64>,
    pnl: Option<f64>,
    /// The live stop.
    stop: f64,
    targets: Vec<f64>,
}

#[derive(Serialize)]
struct MarksAnswer {
    day: String,
    fills: Vec<FillMark>,
    calls: Vec<CallMark>,
    telegram: Vec<TelegramLine>,
}

/// `GET /api/v2/marks` — today's buys and sells (the current mode's), the
/// Telegram calls that named an index option and when they arrived, and the
/// open Telegram positions — for the Scalp charts' markers and read-only lines.
pub async fn marks_handler(State(state): State<AppState>) -> Response {
    let mode = state.trading_cfg.read().await.mode.clone();
    let day = shared_domain::today_ist().format("%Y-%m-%d").to_string();
    let legs = match history::load_legs(&state.db_pool, &mode).await {
        Ok(legs) => legs,
        Err(e) => {
            tracing::error!("v2 marks: trades: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "could not read trades" }))).into_response();
        }
    };
    let fills = legs
        .into_iter()
        .filter(|l| l.timestamp.starts_with(&day))
        .map(|l| FillMark {
            scalp: l.signal_id.as_deref().is_some_and(|s| s.starts_with("manual-")),
            id: l.id,
            t: l.timestamp,
            action: l.action.to_ascii_uppercase(),
            qty: l.qty,
            price: l.executed_price,
            index: l.ticker,
            strike: l.strike,
            side: l.option_type,
            reason: l.exit_reason,
        })
        .collect();
    let calls = sqlx::query_as::<_, CallMark>(
        "SELECT id, sent_at, chat_title, instrument, strike, side, text FROM telegram_messages
         WHERE sent_at >= ? AND sent_at <= ? AND kind = 'signal' AND instrument IS NOT NULL
         ORDER BY sent_at ASC",
    )
    .bind(format!("{day} 00:00:00"))
    .bind(format!("{day} 23:59:59"))
    .fetch_all(&state.db_pool)
    .await
    .unwrap_or_else(|e| {
        tracing::error!("v2 marks: telegram calls: {e}");
        Vec::new()
    });
    let positions = state.positions.read().await.clone();
    let telegram = positions
        .iter()
        .filter(|p| !p.manual && !matches!(p.state, TradeState::Closed))
        .map(|p| {
            let ltp = p.ws_scrip_key.as_ref().and_then(|k| state.prices.get(k).map(|v| *v)).filter(|v| *v > 0.0);
            let held = !matches!(p.state, TradeState::WaitingForEntry) && p.executed_qty > 0;
            TelegramLine {
                id: p.id.clone(),
                index: p.signal.instrument_name.clone(),
                strike: p.signal.strike,
                side: p.signal.option_type.clone(),
                expiry: p.resolved_expiry.map(|d| d.format("%Y-%m-%d").to_string()),
                state: match p.state {
                    TradeState::WaitingForEntry => "waiting",
                    TradeState::Target1Hit => "target1",
                    _ => "active",
                },
                channel: crate::desk::source_of(p).title,
                qty: p.executed_qty,
                avg: p.avg_buy_price,
                entry: p.signal.entry_price,
                ltp,
                pnl: ltp.filter(|_| held).map(|l| (l - p.avg_buy_price) * f64::from(p.executed_qty)),
                stop: p.current_sl,
                targets: p.signal.targets.clone(),
            }
        })
        .collect();
    Json(MarksAnswer { day, fills, calls, telegram }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(call_delta: f64, put_delta: f64) -> StrikeRow {
        StrikeRow {
            strike: 25_100.0,
            call_oi: 0.0,
            call_prev_oi: 0.0,
            call_ltp: 0.0,
            call_volume: 0.0,
            call_iv: 0.0,
            call_delta,
            put_oi: 0.0,
            put_prev_oi: 0.0,
            put_ltp: 0.0,
            put_volume: 0.0,
            put_iv: 0.0,
            put_delta,
        }
    }

    #[test]
    fn a_delta_is_upstoxs_when_it_has_one_and_half_otherwise() {
        let r = row(0.62, -0.38);
        assert_eq!(delta_of(Some(&r), true), Delta { value: 0.62, assumed: false });
        assert_eq!(delta_of(Some(&r), false), Delta { value: -0.38, assumed: false });
        // Upstox sent none for this strike, or has no chain for the index.
        assert_eq!(delta_of(Some(&row(0.0, 0.0)), true), Delta { value: 0.5, assumed: true });
        assert_eq!(delta_of(None, false), Delta { value: -0.5, assumed: true });
    }
}
