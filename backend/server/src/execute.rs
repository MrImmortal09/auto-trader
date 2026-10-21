//! Execute screen ("Scalp Desk"): each index's nearest-expiry option ladder —
//! the strikes around ATM with live Kotak prices — streamed to the
//! dashboard, and the board the manual-order routes check a tap against.
//!
//! Everything here reads in-memory state only (scrip master, the shared price
//! map, positions, telemetry). It never calls Kotak; the only thing it sends
//! anywhere is feed subscriptions through the market-data bridge.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;
use shared_domain::{telemetry, ChartLevels, MonitoredPosition, TradeState, INDEX_NAMES};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use trading_engine::{ChainStrike, OptionChain, ScripRecord};

use crate::AppState;

/// Strikes subscribed and streamed each side of ATM. The screen shows ±5 of
/// them; the spare two each side let it hold its rows still while the user's
/// finger is on the ladder, and give a re-centred ladder prices at once.
const SUBSCRIBE_HALF: usize = 7;
/// How often the stream pushes a frame.
pub const STREAM_EVERY: Duration = Duration::from_millis(250);
/// A feed quieter than this is stale: no manual buy is accepted on it.
pub const MAX_FEED_AGE_SECS: f64 = 5.0;
/// A contract whose own price hasn't ticked for this long isn't bought: the
/// price on screen may be long gone even while the feed itself is alive.
/// Near-the-money index options tick about once a second.
pub const MAX_CONTRACT_AGE_SECS: f64 = 10.0;
/// Engine errors are kept this long, so a failed tap can say why.
const RECENT_ERROR_MS: i64 = 60_000;

/// One index's chain as last built, with what it was built from.
struct CachedChain {
    chain: Arc<OptionChain>,
    loaded_at: Option<DateTime<FixedOffset>>,
    day: NaiveDate,
}

#[derive(Default)]
struct Subscriptions {
    /// The bridge channel these were sent on. A re-login starts a fresh
    /// bridge with a fresh channel, which knows none of them.
    channel: Option<mpsc::UnboundedSender<String>>,
    /// The IST day they were sent on: the feed drops ladder subscriptions at
    /// each day's close, so a new day starts from none.
    day: Option<NaiveDate>,
    scrips: HashSet<String>,
    indices_sent: bool,
}

/// An engine error from the log stream, kept briefly for the Execute screen.
#[derive(Clone, Serialize)]
pub struct RecentError {
    /// The position's underlying (`NIFTY`) — what the engine logs errors under.
    instrument: String,
    /// The position the error is about, when the engine says.
    position_id: Option<String>,
    message: String,
    /// Unix ms, on the same clock as [`ExecuteFrame::ts`].
    ts: i64,
}

#[derive(Default)]
pub struct ExecuteBoard {
    chains: RwLock<HashMap<String, CachedChain>>,
    subs: Mutex<Subscriptions>,
    errors: std::sync::Mutex<Vec<RecentError>>,
}

impl ExecuteBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep the engine's error lines from the last minute, so the screen can
    /// tell the user why a tap didn't fill ("not enough funds…").
    pub fn spawn_error_watch(self: &std::sync::Arc<Self>, log_tx: &broadcast::Sender<String>) {
        let board = std::sync::Arc::clone(self);
        let mut rx = log_tx.subscribe();
        tokio::spawn(async move {
            loop {
                let line = match rx.recv().await {
                    Ok(line) => line,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                if !matches!(v["event"].as_str(), Some("LIVE_ERROR" | "ERROR")) {
                    continue;
                }
                let (Some(instrument), Some(message)) = (v["instrument"].as_str(), v["message"].as_str()) else {
                    continue;
                };
                let now = shared_domain::now_ist().timestamp_millis();
                let mut errors = board.errors.lock().unwrap_or_else(|e| e.into_inner());
                errors.retain(|e| now - e.ts < RECENT_ERROR_MS);
                errors.push(RecentError {
                    instrument: instrument.to_string(),
                    position_id: v["position_id"].as_str().map(str::to_string),
                    message: message.to_string(),
                    ts: now,
                });
                let excess = errors.len().saturating_sub(20);
                errors.drain(..excess);
            }
        });
    }

    fn recent_errors(&self, now: i64) -> Vec<RecentError> {
        let errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
        errors.iter().filter(|e| now - e.ts < RECENT_ERROR_MS).cloned().collect()
    }

    /// `index`'s nearest-expiry chain, rebuilt whenever the scrip master is
    /// reloaded or the IST date rolls over (yesterday's expiry is gone).
    pub async fn chain(&self, state: &AppState, index: &str) -> Option<Arc<OptionChain>> {
        let loaded_at = *state.scrip_loaded_at.read().await;
        let today = shared_domain::today_ist();
        if let Some(c) = self.chains.read().await.get(index) {
            if c.loaded_at == loaded_at && c.day == today {
                return Some(Arc::clone(&c.chain));
            }
        }
        let chain = {
            let store = state.scrip_store.read().await;
            Arc::new(store.as_ref()?.nearest_expiry_chain(index, today)?)
        };
        self.chains.write().await.insert(
            index.to_string(),
            CachedChain { chain: Arc::clone(&chain), loaded_at, day: today },
        );
        Some(chain)
    }

    /// The contract `trading_symbol` names, if it is one of an index's
    /// nearest-expiry options — the only contracts the Execute screen trades.
    pub async fn find_contract(&self, state: &AppState, trading_symbol: &str) -> Option<(String, Arc<OptionChain>, ScripRecord)> {
        let wanted = trading_symbol.trim();
        for index in INDEX_NAMES {
            let Some(chain) = self.chain(state, index).await else { continue };
            let hit = chain
                .strikes
                .iter()
                .flat_map(|s| [s.ce.as_ref(), s.pe.as_ref()])
                .flatten()
                .find(|r| r.trading_symbol.trim() == wanted)
                .cloned();
            if let Some(record) = hit {
                return Some((index.to_string(), chain, record));
            }
        }
        None
    }

    /// Make sure the feed streams every index's spot value and `keys`.
    /// Subscriptions are never withdrawn: open positions share the feed.
    async fn ensure_subscribed(&self, state: &AppState, keys: &[String]) {
        let Some(tx) = state.ws_tx.lock().await.clone() else { return };
        let today = shared_domain::today_ist();
        let mut subs = self.subs.lock().await;
        if !subs.channel.as_ref().is_some_and(|c| c.same_channel(&tx)) || subs.day != Some(today) {
            *subs = Subscriptions { channel: Some(tx.clone()), day: Some(today), ..Default::default() };
        }
        if !subs.indices_sent {
            let indices: Vec<String> = INDEX_NAMES
                .iter()
                .filter_map(|i| shared_domain::kotak_index_feed(i))
                .map(|(seg, name)| format!("{seg}|{name}"))
                .collect();
            let msg = serde_json::json!({ "action": "subscribe_indices", "indices": indices.join(",") });
            subs.indices_sent = tx.send(msg.to_string()).is_ok();
        }
        let fresh: Vec<String> = keys.iter().filter(|k| !subs.scrips.contains(*k)).cloned().collect();
        if fresh.is_empty() {
            return;
        }
        // `ladder`: the feed drops these at the day's close (unlike a
        // position's own subscription), so they never pile up across days.
        let msg = serde_json::json!({ "action": "subscribe", "scrips": fresh.join("&"), "ladder": true });
        if tx.send(msg.to_string()).is_ok() {
            subs.scrips.extend(fresh);
        }
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ExecuteFrame {
    /// Unix milliseconds the frame was built.
    ts: i64,
    mode: String,
    kill_switch: bool,
    /// Manual buys are accepted: 09:15–15:30 IST on a weekday.
    market_open: bool,
    feed: FeedStatus,
    /// LIVE: a Kotak session is held and its last probe succeeded.
    kotak_ready: bool,
    indices: Vec<IndexHeadline>,
    board: Option<Board>,
    /// Why there is no board (scrip master not loaded, no contracts…).
    board_error: Option<String>,
    positions: Vec<PositionRow>,
    /// Engine errors from the last minute — why a tap didn't fill.
    errors: Vec<RecentError>,
}

#[derive(Serialize)]
struct FeedStatus {
    connected: bool,
    last_tick_age_ms: Option<u64>,
}

#[derive(Serialize)]
struct IndexHeadline {
    index: &'static str,
    spot: Option<f64>,
    change_pct: Option<f64>,
}

#[derive(Serialize)]
struct Board {
    index: String,
    /// `YYYY-MM-DD`.
    expiry: String,
    days_to_expiry: i64,
    lot_size: u32,
    spot: Option<f64>,
    /// `"kotak"` (live index tick), `"upstox"` (60 s option-chain poll) or
    /// `"parity"` (estimated from the ladder's own call/put prices).
    spot_source: Option<&'static str>,
    atm: Option<f64>,
    rows: Vec<Row>,
}

#[derive(Serialize)]
struct Row {
    strike: f64,
    ce: Option<Quote>,
    pe: Option<Quote>,
}

#[derive(Serialize)]
struct Quote {
    sym: String,
    ltp: Option<f64>,
    /// Milliseconds since this contract's own last tick.
    age_ms: Option<u64>,
}

#[derive(Serialize)]
struct PositionRow {
    id: String,
    manual: bool,
    index: String,
    strike: Option<f64>,
    option_type: Option<String>,
    symbol: String,
    lot_size: i32,
    qty: i32,
    avg: f64,
    ltp: Option<f64>,
    pnl: Option<f64>,
    /// A manual first buy not filled yet.
    placing: bool,
    /// More is being bought (a manual add, or an indicator top-up).
    adding: bool,
    /// A sell or exit is pending.
    exiting: bool,
    /// The engine stopped acting on it by itself — see the log.
    halted: bool,
    /// A scalp's stop and target set from a v2 chart; left out while unset.
    #[serde(skip_serializing_if = "ChartLevels::is_empty")]
    chart: ChartLevels,
}

pub(crate) fn price(state: &AppState, key: &str) -> Option<f64> {
    state.prices.get(key).map(|p| *p).filter(|p| *p > 0.0)
}

/// Seconds since the contract behind feed key `key` last ticked, if it has.
pub fn contract_age_secs(key: &str) -> Option<f64> {
    telemetry::scrip_last_tick(key).map(|t| (telemetry::unix_now() - t).max(0.0))
}

/// Feed status from the same telemetry the System screen shows.
fn feed_status() -> FeedStatus {
    let last = telemetry::FEED_LAST_TICK.get();
    FeedStatus {
        connected: telemetry::FEED_CONNECTED.get() == 1,
        last_tick_age_ms: (last > 0.0).then(|| ((telemetry::unix_now() - last).max(0.0) * 1000.0) as u64),
    }
}

/// Whether the price feed is live enough to buy on.
pub fn feed_fresh() -> bool {
    let f = feed_status();
    f.connected && f.last_tick_age_ms.is_some_and(|ms| (ms as f64) / 1000.0 <= MAX_FEED_AGE_SECS)
}

/// LIVE: whether a Kotak session is held and healthy. The client mutex is
/// held only briefly; when it is busy, the System screen's last reading is used.
pub fn kotak_ready(state: &AppState) -> bool {
    let present = match state.kotak.try_lock() {
        Ok(k) => k.is_some(),
        Err(_) => telemetry::KOTAK_CONNECTED.get() == 1,
    };
    present && state.kotak_session_healthy.load(std::sync::atomic::Ordering::Relaxed)
}

/// Manual buys are accepted 09:15–15:30 IST on a weekday — the exchange's
/// normal session, inside the engine's own 09:15–15:40 window.
pub fn manual_buy_window_open() -> bool {
    use chrono::Timelike;
    let now = shared_domain::now_ist();
    shared_domain::is_market_open() && (now.hour(), now.minute()) < (15, 30)
}

/// Live index value and its % change on the previous close, from Kotak's feed.
fn kotak_index(state: &AppState, index: &str) -> Option<(f64, Option<f64>)> {
    let (_, name) = shared_domain::kotak_index_feed(index)?;
    let spot = price(state, &shared_domain::index_price_key(name))?;
    let change = price(state, &shared_domain::index_close_key(name)).map(|c| (spot - c) / c * 100.0);
    Some((spot, change))
}

async fn upstox_spot(state: &AppState, index: &str) -> Option<f64> {
    state.market.indices.read().await.get(index).and_then(|l| l.spot).filter(|s| *s > 0.0)
}

/// Spot from put-call parity on whatever strikes have both prices: at the
/// strike where call and put are closest, `K + CE − PE`.
fn parity_spot(state: &AppState, strikes: &[ChainStrike]) -> Option<f64> {
    strikes
        .iter()
        .filter_map(|s| {
            let ce = price(state, &s.ce.as_ref()?.ws_key())?;
            let pe = price(state, &s.pe.as_ref()?.ws_key())?;
            Some((s.strike, ce, pe))
        })
        .min_by(|a, b| (a.1 - a.2).abs().total_cmp(&(b.1 - b.2).abs()))
        .map(|(k, ce, pe)| k + ce - pe)
}

/// Index of the strike nearest `spot`.
fn atm_index(strikes: &[ChainStrike], spot: f64) -> Option<usize> {
    strikes
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (a.strike - spot).abs().total_cmp(&(b.strike - spot).abs()))
        .map(|(i, _)| i)
}

/// `[atm - half, atm + half]`, clamped to the chain.
fn window(len: usize, atm: usize, half: usize) -> std::ops::Range<usize> {
    atm.saturating_sub(half)..(atm + half + 1).min(len)
}

async fn build_board(state: &AppState, index: &str) -> Result<Board, String> {
    if state.scrip_store.read().await.is_none() {
        return Err("Scrip Master not loaded yet — connect Kotak first".to_string());
    }
    let chain = state
        .execute
        .chain(state, index)
        .await
        .ok_or_else(|| format!("No {index} option contracts in the Scrip Master"))?;

    let (spot, spot_source) = match kotak_index(state, index) {
        Some((s, _)) => (Some(s), Some("kotak")),
        None => match upstox_spot(state, index).await {
            Some(s) => (Some(s), Some("upstox")),
            None => match parity_spot(state, &chain.strikes) {
                Some(s) => (Some(s), Some("parity")),
                None => (None, None),
            },
        },
    };

    // Without any spot yet, centre on the middle of the chain so prices start
    // flowing; parity takes over as soon as they do.
    let atm = spot.and_then(|s| atm_index(&chain.strikes, s)).unwrap_or(chain.strikes.len() / 2);
    let keys: Vec<String> = chain.strikes[window(chain.strikes.len(), atm, SUBSCRIBE_HALF)]
        .iter()
        .flat_map(|s| [s.ce.as_ref(), s.pe.as_ref()])
        .flatten()
        .map(ScripRecord::ws_key)
        .collect();
    state.execute.ensure_subscribed(state, &keys).await;

    let quote = |r: &Option<ScripRecord>| {
        r.as_ref().map(|r| {
            let key = r.ws_key();
            Quote {
                sym: r.trading_symbol.clone(),
                ltp: price(state, &key),
                age_ms: contract_age_secs(&key).map(|s| (s * 1000.0) as u64),
            }
        })
    };
    // The whole subscribed window goes out: the screen shows ATM ± 5 of
    // it, and while a finger is on the ladder it holds its
    // rows still — the spare strikes keep their prices live meanwhile.
    let rows = chain.strikes[window(chain.strikes.len(), atm, SUBSCRIBE_HALF)]
        .iter()
        .map(|s| Row { strike: s.strike, ce: quote(&s.ce), pe: quote(&s.pe) })
        .collect();

    Ok(Board {
        index: index.to_string(),
        expiry: chain.expiry.format("%Y-%m-%d").to_string(),
        days_to_expiry: (chain.expiry - shared_domain::today_ist()).num_days(),
        lot_size: chain.lot_size,
        spot,
        spot_source,
        atm: spot.map(|_| chain.strikes[atm].strike),
        rows,
    })
}

fn position_row(state: &AppState, p: &MonitoredPosition) -> PositionRow {
    let ltp = p
        .ws_scrip_key
        .as_deref()
        .and_then(|k| price(state, k))
        .or_else(|| price(state, &p.signal.instrument_name));
    let lot_size = p
        .resolved_order
        .as_ref()
        .and_then(|o| o.quantity.parse::<i32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1);
    PositionRow {
        id: p.id.clone(),
        manual: p.manual,
        index: p.signal.instrument_name.clone(),
        strike: p.signal.strike,
        option_type: p.signal.option_type.clone(),
        symbol: p.resolved_order.as_ref().map(|o| o.trading_symbol.clone()).unwrap_or_default(),
        lot_size,
        qty: p.executed_qty,
        avg: p.avg_buy_price,
        ltp,
        pnl: ltp.filter(|_| p.executed_qty > 0).map(|l| (l - p.avg_buy_price) * p.executed_qty as f64),
        placing: matches!(p.state, TradeState::WaitingForEntry),
        adding: p.buy_in_flight(),
        exiting: p.sell_in_flight(),
        halted: p.live_halt.is_some(),
        chart: p.chart,
    }
}

/// One frame of the Execute screen for `index`.
pub async fn build_frame(state: &AppState, index: &str) -> ExecuteFrame {
    let (mode, kill_switch) = {
        let cfg = state.trading_cfg.read().await;
        (cfg.mode.clone(), cfg.kill_switch_active)
    };

    let mut indices = Vec::with_capacity(INDEX_NAMES.len());
    for &name in INDEX_NAMES {
        let (spot, change_pct) = match kotak_index(state, name) {
            Some((s, c)) => (Some(s), c),
            None => (upstox_spot(state, name).await, None),
        };
        indices.push(IndexHeadline { index: name, spot, change_pct });
    }

    let (board, board_error) = match build_board(state, index).await {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e)),
    };

    // What is actually held (or being bought by hand) — a signal still
    // waiting for its trigger is not a position yet.
    let positions = state
        .positions
        .read()
        .await
        .iter()
        .filter(|p| match p.state {
            TradeState::Closed => false,
            TradeState::WaitingForEntry => p.manual,
            _ => true,
        })
        .map(|p| position_row(state, p))
        .collect();

    let ts = shared_domain::now_ist().timestamp_millis();
    ExecuteFrame {
        ts,
        mode,
        kill_switch,
        market_open: manual_buy_window_open(),
        feed: feed_status(),
        kotak_ready: kotak_ready(state),
        indices,
        board,
        board_error,
        positions,
        errors: state.execute.recent_errors(ts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strike(k: f64) -> ChainStrike {
        ChainStrike { strike: k, ce: None, pe: None }
    }

    #[test]
    fn the_ladder_window_is_clamped_to_the_chain() {
        assert_eq!(window(30, 15, 5), 10..21);
        assert_eq!(window(30, 2, 5), 0..8);
        assert_eq!(window(30, 28, 5), 23..30);
        assert_eq!(window(4, 1, 5), 0..4);
    }

    #[test]
    fn atm_is_the_strike_nearest_spot() {
        let chain: Vec<ChainStrike> = [22850.0, 22900.0, 22950.0, 23000.0].map(strike).into();
        assert_eq!(atm_index(&chain, 22963.6), Some(2));
        assert_eq!(atm_index(&chain, 22976.0), Some(3));
        assert_eq!(atm_index(&chain, 10.0), Some(0));
        assert_eq!(atm_index(&[], 22963.6), None);
    }
}
