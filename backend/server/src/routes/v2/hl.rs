//! Hyperliquid perps' market data: `HL:` symbols on the candle routes,
//! `GET /api/v2/hl/markets` and `GET /api/v2/hl/catalog`. All behind the
//! `hyperliquid` switch: while it is off they answer 404. Read-only — see
//! `crate::hyperliquid`.

use std::collections::BTreeMap;
use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use chrono::{NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

use super::candles::{bad_request, max_days, parse_date, parse_tf, DEFAULT_DAYS, MAX_STREAM_SYMBOLS, STREAM_EVERY};
use crate::candles::bars::{Bar, Tf};
use crate::hyperliquid::{markets, Market};
use crate::AppState;

const SYMBOL_HELP: &str = "a Hyperliquid symbol is HL: and a perp on the app's list (HL:BTC, HL:GOLD, HL:xyz:NVDA)";
/// Most perps one `/api/v2/hl/markets` call asks about, beside the header's.
const MAX_ASKED: usize = 40;

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
/// over IST days `from..=to`, oldest first: the kept series, with only what
/// it lacks read from Hyperliquid (see `crate::hyperliquid`). The same
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

/// How often an open stream tells the market data its series are still
/// watched (so closed bars keep being corrected).
const TOUCH_EVERY_MS: i64 = 30_000;

struct Follow {
    rx: broadcast::Receiver<crate::hyperliquid::BarChange>,
    keys: Vec<&'static str>,
    tf: Tf,
    every: tokio::time::Interval,
    hl: std::sync::Arc<crate::hyperliquid::HlMarketData>,
    touched_at: i64,
}

impl Follow {
    /// The bars that changed since the last call, newest of each.
    async fn next_updates(&mut self) -> Option<Vec<Update>> {
        loop {
            self.every.tick().await;
            let now = shared_domain::now_ist().timestamp_millis();
            if now - self.touched_at >= TOUCH_EVERY_MS {
                self.touched_at = now;
                self.hl.touch(&self.keys, self.tf);
            }
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
/// chart: every 250 ms the perps' bars that changed — the forming bar, and a
/// closed one corrected from Hyperliquid's candles — as event `bars`
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
    state.hl.touch(&keys, tf);
    let follow = Follow { rx: state.hl.subscribe(), keys, tf, every, hl: state.hl.clone(), touched_at: shared_domain::now_ist().timestamp_millis() };
    let updates = futures::stream::unfold(follow, |mut f| async move {
        let updates = f.next_updates().await?;
        let data = json!({ "tf": f.tf.as_str(), "updates": updates });
        Some((Ok::<_, Infallible>(Event::default().event("bars").data(data.to_string())), f))
    });
    Sse::new(updates).keep_alive(KeepAlive::default()).into_response()
}

/// A perp's rules, as the screens need them to size and price an order.
#[derive(Serialize)]
struct RulesRow {
    key: &'static str,
    symbol: String,
    coin: &'static str,
    base: &'static str,
    /// The builder dex it trades on; `null` on a core perp.
    dex: Option<&'static str>,
    /// `Hyperliquid`, `trade.xyz`, `Paragon`, …
    venue: &'static str,
    label: &'static str,
    name: &'static str,
    sz_decimals: u32,
    max_leverage: u32,
    /// The most the app takes on it: Hyperliquid's limit, at most 20x.
    leverage_cap: u32,
    taker_fee: f64,
    maker_fee: f64,
    /// Added; `false`: close-only (removed, still held somewhere).
    tradable: bool,
    /// In the Trade header.
    header: bool,
}

fn rules_row(e: &markets::Entry) -> RulesRow {
    let m = e.market;
    RulesRow {
        key: m.key,
        symbol: m.symbol(),
        coin: m.coin,
        base: m.base,
        dex: m.dex,
        venue: markets::venue(m.dex),
        label: m.label,
        name: m.name,
        sz_decimals: m.sz_decimals,
        max_leverage: m.max_leverage,
        leverage_cap: m.leverage_cap(),
        taker_fee: m.taker_fee,
        maker_fee: m.maker_fee,
        tradable: e.added,
        header: e.header,
    }
}

/// One perp in `/api/v2/hl/markets`: its rules and its prices now.
#[derive(Serialize)]
struct MarketRow {
    #[serde(flatten)]
    rules: RulesRow,
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

#[derive(Deserialize)]
pub struct MarketsQuery {
    /// Perps to price beside the header's, by key, comma-separated: the
    /// ones a screen charts, trades or holds.
    keys: Option<String>,
}

/// `GET /api/v2/hl/markets?keys=xyz:NVDA,SOL` — the header's perps and the
/// ones asked about (on the list, added or close-only), with their mark,
/// mid, oracle price, 24 h change, funding and open interest from the live
/// feed (empty figures until its first update). Asking follows a perp for
/// the next minute, so the screen's next poll finds it priced. `up` says
/// the feed is connected; `rev` changes whenever the list does
/// (`/api/v2/hl/catalog`).
pub async fn markets_handler(State(state): State<AppState>, Query(q): Query<MarketsQuery>) -> Response {
    if !on(&state).await {
        return switched_off();
    }
    let all = markets::entries();
    let mut asked: Vec<&'static str> = Vec::new();
    for raw in q.keys.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|k| !k.is_empty()) {
        if asked.len() >= MAX_ASKED {
            return bad_request(format!("at most {MAX_ASKED} keys"));
        }
        if let Some(m) = Market::of_key(raw) {
            if !asked.contains(&m.key) {
                asked.push(m.key);
            }
        }
    }
    state.hl.ask(&asked);
    let rows: Vec<MarketRow> = all
        .iter()
        .filter(|e| (e.added && e.header) || asked.contains(&e.market.key))
        .map(|e| {
            let q = state.hl.quote(e.market.key);
            let ctx = q.as_ref().map(|q| &q.ctx);
            let price = ctx.map(|c| c.mid.unwrap_or(c.mark));
            let prev = ctx.map(|c| c.prev_day).filter(|p| *p > 0.0);
            MarketRow {
                rules: rules_row(e),
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
    Json(json!({ "up": state.hl.is_up(), "rev": markets::rev(), "markets": rows })).into_response()
}

/// `GET /api/v2/hl/catalog` — every perp on the app's list with its rules,
/// in the order added: the added ones (what search and the pickers offer)
/// and the close-only ones (`tradable: false`), so a position still held in
/// one is shown and closed. No prices: `/api/v2/hl/markets` has them.
pub async fn catalog_handler(State(state): State<AppState>) -> Response {
    if !on(&state).await {
        return switched_off();
    }
    let rows: Vec<RulesRow> = markets::entries().iter().map(rules_row).collect();
    Json(json!({ "rev": markets::rev(), "markets": rows })).into_response()
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
