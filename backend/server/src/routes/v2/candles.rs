//! `GET /api/v2/candles` and the SSE `GET /api/v2/candles/stream` — chart
//! bars from the candle service (`crate::candles`). Analytics only.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use chrono::NaiveDate;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

use crate::candles::bars::{self, Bar, Tf};
use crate::candles::symbols::Symbol;
use crate::candles::{Change, Running, StreamGuard, TrackError};
use crate::AppState;

/// Days shown when the request names none.
const DEFAULT_DAYS: u64 = 5;
/// Most symbols one stream follows.
const MAX_STREAM_SYMBOLS: usize = 6;
/// How often a stream sends what changed.
const STREAM_EVERY: Duration = Duration::from_millis(250);
/// How often a stream tells the service its symbols are still wanted.
const TOUCH_EVERY_MS: i64 = 30_000;

/// Most days one request may span: Upstox serves at most a month of minute
/// candles per call, and a week of 1-minute bars is already ~2,600 of them.
fn max_days(tf: Tf) -> i64 {
    match tf {
        Tf::M1 => 7,
        Tf::M5 | Tf::M15 => 31,
    }
}

fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.into() }))).into_response()
}

const SYMBOL_HELP: &str =
    "symbol must be an index (NIFTY), its nearest future (NIFTY:FUT) or an option (NIFTY:2026-10-27:22750:CE)";

fn track_error(symbol: &Symbol, e: TrackError) -> Response {
    match e {
        TrackError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("{} is not a contract either broker lists today", symbol.name()) })),
        )
            .into_response(),
        TrackError::Stopped => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "the candle service is restarting — try again" })),
        )
            .into_response(),
    }
}

fn parse_tf(raw: Option<&str>) -> Option<Tf> {
    raw.map_or(Some(Tf::M5), Tf::parse)
}

fn parse_date(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok()
}

/// Whether `bar` is still open at `now`.
fn is_forming(bar: &Bar, tf: Tf, now: i64) -> bool {
    bars::in_session(now) && bar.t <= now && now < bar.t + tf.ms()
}

#[derive(Deserialize)]
pub struct CandlesQuery {
    symbol: String,
    tf: Option<String>,
    /// First IST day, `YYYY-MM-DD`. Default: four days before `to`.
    from: Option<String>,
    /// Last IST day, `YYYY-MM-DD`. Default: today.
    to: Option<String>,
}

/// `GET /api/v2/candles?symbol=NIFTY&tf=5m&from=2026-10-16&to=2026-10-21` —
/// bars oldest first. Past days are Upstox's candles; today's are the
/// service's, built from live ticks and corrected with Upstox's official
/// minutes. `forming` says the last bar is still open.
pub async fn candles_handler(State(state): State<AppState>, Query(q): Query<CandlesQuery>) -> Response {
    let Some(symbol) = Symbol::parse(&q.symbol) else {
        return bad_request(SYMBOL_HELP);
    };
    let Some(tf) = parse_tf(q.tf.as_deref()) else {
        return bad_request("tf must be 1m, 5m or 15m");
    };
    let today = shared_domain::today_ist();
    let to = match q.to.as_deref() {
        None => today,
        Some(raw) => match parse_date(raw) {
            Some(d) => d,
            None => return bad_request("to must be YYYY-MM-DD"),
        },
    };
    let from = match q.from.as_deref() {
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

    let (running, tracking) = match state.candles.track(&state, symbol).await {
        Ok(found) => found,
        Err(e) => return track_error(&symbol, e),
    };

    let mut bars: Vec<Bar> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if from < today {
        let until = to.min(today - chrono::Days::new(1));
        match running.history(tracking.upstox.as_deref(), tf, from, until).await {
            Ok(past) => bars.extend(past),
            Err(e) => notes.push(format!("past days unavailable: {e}")),
        }
    }
    if to >= today {
        bars.extend(running.today(&tracking.name, tf));
    }
    let now = shared_domain::now_ist().timestamp_millis();
    let forming = bars.last().is_some_and(|b| is_forming(b, tf, now));

    Json(json!({
        "symbol": tracking.name,
        "tf": tf.as_str(),
        "bars": bars,
        "forming": forming,
        "feeds": { "kotak": tracking.kotak, "upstox": tracking.upstox },
        "notes": notes,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct StreamQuery {
    /// Comma-separated symbols, as for `/api/v2/candles`.
    symbols: String,
    tf: Option<String>,
}

#[derive(Serialize)]
struct Update {
    symbol: String,
    bar: Bar,
    forming: bool,
}

/// One open stream's state; dropping it (the client went away) releases the
/// stream's hold on the service.
struct Stream {
    guard: StreamGuard,
    changes: broadcast::Receiver<Change>,
    symbols: Vec<Symbol>,
    names: Vec<String>,
    tf: Tf,
    every: tokio::time::Interval,
    touched_at: i64,
}

impl Stream {
    fn running(&self) -> &Arc<Running> {
        self.guard.running()
    }

    /// Tell the service the symbols are still wanted, and track again any it
    /// dropped (a new day, or a lapse while idle).
    async fn keep_tracked(&mut self, now: i64) {
        if now - self.touched_at < TOUCH_EVERY_MS {
            return;
        }
        self.touched_at = now;
        for name in self.running().touch(&self.names) {
            if let Some(symbol) = self.symbols.iter().find(|s| s.name() == name) {
                let _ = self.running().track(*symbol).await;
            }
        }
    }

    /// The bars that changed since the last call, if any.
    async fn next_updates(&mut self) -> Option<Vec<Update>> {
        loop {
            self.every.tick().await;
            let now = shared_domain::now_ist().timestamp_millis();
            self.keep_tracked(now).await;

            let mut dirty: BTreeSet<(String, i64)> = BTreeSet::new();
            loop {
                match self.changes.try_recv() {
                    Ok(c) => {
                        if self.names.iter().any(|n| n.as_str() == &*c.symbol) {
                            dirty.insert((c.symbol.to_string(), self.tf.bucket(c.minute)));
                        }
                    }
                    Err(broadcast::error::TryRecvError::Empty) => break,
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        // Too far behind to know what changed: resend each
                        // symbol's current bar.
                        for name in &self.names {
                            dirty.insert((name.clone(), self.tf.bucket(now)));
                        }
                    }
                    Err(broadcast::error::TryRecvError::Closed) => return None,
                }
            }
            let updates: Vec<Update> = dirty
                .into_iter()
                .filter_map(|(name, start)| {
                    let bar = self.running().bar_at(&name, self.tf, start)?;
                    Some(Update { forming: is_forming(&bar, self.tf, now), symbol: name, bar })
                })
                .collect();
            if !updates.is_empty() {
                return Some(updates);
            }
        }
    }
}

/// `GET /api/v2/candles/stream?symbols=NIFTY,NIFTY:FUT&tf=5m` — SSE. Every
/// 250 ms, the bars that changed: the forming bar as ticks arrive, and a
/// closed bar again when Upstox's official minutes correct it (which is when
/// its volume appears). Event `bars`:
/// `{"tf":"5m","updates":[{"symbol","bar":{t,o,h,l,c,v},"forming"}]}`. A first
/// `error` event lists symbols that could not be followed.
pub async fn candles_stream_handler(State(state): State<AppState>, Query(q): Query<StreamQuery>) -> Response {
    let Some(tf) = parse_tf(q.tf.as_deref()) else {
        return bad_request("tf must be 1m, 5m or 15m");
    };
    let mut symbols: Vec<Symbol> = Vec::new();
    for raw in q.symbols.split(',').filter(|s| !s.trim().is_empty()) {
        let Some(symbol) = Symbol::parse(raw) else {
            return bad_request(format!("{raw}: {SYMBOL_HELP}"));
        };
        if !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
    }
    if symbols.is_empty() || symbols.len() > MAX_STREAM_SYMBOLS {
        return bad_request(format!("symbols must name 1 to {MAX_STREAM_SYMBOLS} instruments"));
    }

    let mut followed: Vec<Symbol> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for symbol in symbols {
        match state.candles.track(&state, symbol).await {
            Ok(_) => followed.push(symbol),
            Err(_) => failed.push(symbol.name()),
        }
    }
    let running = state.candles.get(&state).await;
    let mut every = tokio::time::interval(STREAM_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stream = Stream {
        changes: running.subscribe_changes(),
        guard: running.open_stream(),
        names: followed.iter().map(Symbol::name).collect(),
        symbols: followed,
        tf,
        every,
        touched_at: shared_domain::now_ist().timestamp_millis(),
    };

    let first = (!failed.is_empty()).then(|| {
        Ok::<_, Infallible>(
            Event::default()
                .event("error")
                .data(json!({ "error": "not followed", "symbols": failed }).to_string()),
        )
    });
    let updates = futures::stream::unfold(stream, |mut s| async move {
        let updates = s.next_updates().await?;
        let data = json!({ "tf": s.tf.as_str(), "updates": updates });
        Some((Ok::<_, Infallible>(Event::default().event("bars").data(data.to_string())), s))
    });
    Sse::new(futures::stream::iter(first).chain(updates))
        .keep_alive(KeepAlive::default())
        .into_response()
}
