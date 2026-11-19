//! Hyperliquid perps' market data: `HL:` symbols on the candle routes, and
//! `GET /api/v2/hl/markets`. All behind the `hyperliquid` switch: while it
//! is off they answer 404. Read-only — see `crate::hyperliquid`.

use std::collections::BTreeMap;
use std::convert::Infallible;

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use chrono::{NaiveDate, TimeZone};
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;

use super::candles::{bad_request, max_days, parse_date, parse_tf, DEFAULT_DAYS, MAX_STREAM_SYMBOLS, STREAM_EVERY};
use crate::candles::bars::{Bar, Tf};
use crate::hyperliquid::{Market, MARKETS};
use crate::AppState;

const SYMBOL_HELP: &str = "a Hyperliquid symbol must be HL:BTC, HL:ETH or HL:GOLD";

fn switched_off() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Hyperliquid is switched off" }))).into_response()
}

async fn on(state: &AppState) -> bool {
    state.flags.read().await.hyperliquid
}

/// 00:00 IST on `day`, as Unix ms.
fn ist_midnight_ms(day: NaiveDate) -> i64 {
    shared_domain::ist_offset()
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight"))
        .single()
        .map(|t| t.timestamp_millis())
        .unwrap_or_default()
}

fn forming(bar: &Bar, tf: Tf, now: i64) -> bool {
    bar.t <= now && now < bar.t + tf.ms()
}

/// `GET /api/v2/candles?symbol=HL:BTC&tf=5m&from=…&to=…` — a perp's bars
/// over IST days `from..=to`, oldest first, from Hyperliquid. The same
/// answer's shape as any chart's; perps trade around the clock, so every
/// day is whole.
pub async fn candles(state: &AppState, raw: &str, tf: Option<&str>, from: Option<&str>, to: Option<&str>) -> Response {
    if !on(state).await {
        return switched_off();
    }
    let Some(m) = Market::of_symbol(raw) else {
        return bad_request(SYMBOL_HELP);
    };
    let Some(tf) = parse_tf(tf) else {
        return bad_request("tf must be 1m, 5m or 15m");
    };
    let today = shared_domain::today_ist();
    let to = match to {
        None => today,
        Some(raw) => match parse_date(raw) {
            Some(d) => d,
            None => return bad_request("to must be YYYY-MM-DD"),
        },
    };
    let from = match from {
        None => to - chrono::Days::new(DEFAULT_DAYS - 1),
        Some(raw) => match parse_date(raw) {
            Some(d) => d,
            None => return bad_request("from must be YYYY-MM-DD"),
        },
    };
    if from > to || to > today {
        return bad_request("from must not be after to, and to not after today");
    }
    if (to - from).num_days() >= max_days(tf) {
        return bad_request(format!("at most {} days of {} bars per request", max_days(tf), tf.as_str()));
    }
    let start = ist_midnight_ms(from);
    let end = ist_midnight_ms(to + chrono::Days::new(1)) - 1;
    match state.hl.history(m, tf, start, end).await {
        Ok(bars) => {
            let now = shared_domain::now_ist().timestamp_millis();
            let forming = bars.last().is_some_and(|b| forming(b, tf, now));
            Json(json!({
                "symbol": m.symbol(),
                "tf": tf.as_str(),
                "bars": bars,
                "forming": forming,
                "feeds": { "hyperliquid": m.coin },
                "polled": false,
                "notes": [],
            }))
            .into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("{}: {e}", m.label) }))).into_response(),
    }
}

#[derive(Serialize)]
struct Update {
    symbol: String,
    bar: Bar,
    forming: bool,
}

struct Follow {
    rx: broadcast::Receiver<crate::hyperliquid::BarChange>,
    keys: Vec<&'static str>,
    tf: Tf,
    every: tokio::time::Interval,
    hl: std::sync::Arc<crate::hyperliquid::HlMarketData>,
}

impl Follow {
    /// The bars that changed since the last call, newest of each.
    async fn next_updates(&mut self) -> Option<Vec<Update>> {
        loop {
            self.every.tick().await;
            let mut changed: BTreeMap<(&'static str, i64), Bar> = BTreeMap::new();
            loop {
                match self.rx.try_recv() {
                    Ok(c) if c.tf == self.tf && self.keys.contains(&c.key) => {
                        changed.insert((c.key, c.bar.t), c.bar);
                    }
                    Ok(_) => {}
                    Err(broadcast::error::TryRecvError::Empty) => break,
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        // Too far behind to know what changed: resend each
                        // perp's latest bar.
                        for key in &self.keys {
                            if let Some(bar) = self.hl.last_bar(key, self.tf) {
                                changed.insert((key, bar.t), bar);
                            }
                        }
                    }
                    Err(broadcast::error::TryRecvError::Closed) => return None,
                }
            }
            if changed.is_empty() {
                continue;
            }
            let now = shared_domain::now_ist().timestamp_millis();
            let tf = self.tf;
            return Some(
                changed
                    .into_iter()
                    .map(|((key, _), bar)| Update { symbol: format!("{}{key}", crate::hyperliquid::SYMBOL_PREFIX), forming: forming(&bar, tf, now), bar })
                    .collect(),
            );
        }
    }
}

/// `GET /api/v2/candles/stream?symbols=HL:BTC&tf=5m` — SSE, as for any
/// chart: every 250 ms the perps' bars that changed, as event `bars`
/// (`{"tf","updates":[{"symbol","bar","forming"}]}`). Perps only: a list
/// mixing them with Indian instruments is refused.
pub async fn stream(state: &AppState, raw: &str, tf: Tf) -> Response {
    if !on(state).await {
        return switched_off();
    }
    let mut keys: Vec<&'static str> = Vec::new();
    for raw in raw.split(',').filter(|s| !s.trim().is_empty()) {
        let Some(m) = Market::of_symbol(raw) else {
            return bad_request(format!("{raw}: {SYMBOL_HELP}; one stream follows Hyperliquid perps or Indian instruments, not both"));
        };
        if !keys.contains(&m.key) {
            keys.push(m.key);
        }
    }
    if keys.is_empty() || keys.len() > MAX_STREAM_SYMBOLS {
        return bad_request(format!("symbols must name 1 to {MAX_STREAM_SYMBOLS} instruments"));
    }
    let mut every = tokio::time::interval(STREAM_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let follow = Follow { rx: state.hl.subscribe(), keys, tf, every, hl: state.hl.clone() };
    let updates = futures::stream::unfold(follow, |mut f| async move {
        let updates = f.next_updates().await?;
        let data = json!({ "tf": f.tf.as_str(), "updates": updates });
        Some((Ok::<_, Infallible>(Event::default().event("bars").data(data.to_string())), f))
    });
    Sse::new(updates).keep_alive(KeepAlive::default()).into_response()
}

/// One perp in `/api/v2/hl/markets`.
#[derive(Serialize)]
struct MarketRow {
    key: &'static str,
    symbol: String,
    coin: &'static str,
    label: &'static str,
    name: &'static str,
    mark: Option<f64>,
    mid: Option<f64>,
    oracle: Option<f64>,
    /// The price 24 hours ago, and the change since, in percent.
    prev_day: Option<f64>,
    change_pct: Option<f64>,
    /// The hourly funding rate (0.0001 = 0.01 %/h); a long pays it when positive.
    funding_hourly: Option<f64>,
    open_interest: Option<f64>,
    day_volume: Option<f64>,
    /// Unix ms the context arrived.
    at: Option<i64>,
}

/// `GET /api/v2/hl/markets` — the three perps with their mark, mid, oracle
/// price, 24 h change, funding and open interest, from the live feed (empty
/// figures until its first update); `up` says the feed is connected.
pub async fn markets_handler(State(state): State<AppState>) -> Response {
    if !on(&state).await {
        return switched_off();
    }
    let rows: Vec<MarketRow> = MARKETS
        .iter()
        .map(|m| {
            let q = state.hl.quote(m.key);
            let ctx = q.as_ref().map(|q| &q.ctx);
            let price = ctx.map(|c| c.mid.unwrap_or(c.mark));
            let prev = ctx.map(|c| c.prev_day).filter(|p| *p > 0.0);
            MarketRow {
                key: m.key,
                symbol: m.symbol(),
                coin: m.coin,
                label: m.label,
                name: m.name,
                mark: ctx.map(|c| c.mark),
                mid: ctx.and_then(|c| c.mid),
                oracle: ctx.map(|c| c.oracle),
                prev_day: prev,
                change_pct: price.zip(prev).map(|(p, prev)| (p - prev) / prev * 100.0),
                funding_hourly: ctx.map(|c| c.funding),
                open_interest: ctx.map(|c| c.open_interest),
                day_volume: ctx.map(|c| c.day_volume),
                at: q.map(|q| q.at_ms),
            }
        })
        .collect();
    Json(json!({ "up": state.hl.is_up(), "markets": rows })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_perps_day_runs_from_ist_midnight() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        // 2026-11-19 00:00 IST is 2026-11-18 18:30 UTC.
        assert_eq!(ist_midnight_ms(day), chrono::Utc.with_ymd_and_hms(2026, 11, 18, 18, 30, 0).unwrap().timestamp_millis());
        let bar = Bar { t: 1_000_000, o: 1.0, h: 1.0, l: 1.0, c: 1.0, v: None };
        assert!(forming(&bar, Tf::M1, 1_000_000 + 59_999));
        assert!(!forming(&bar, Tf::M1, 1_000_000 + 60_000));
    }
}
