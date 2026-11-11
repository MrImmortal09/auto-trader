//! Execute screen ("Scalp Desk" in v1, the Trade window in v2): an
//! underlying's option ladder for one expiry (the nearest unless asked
//! otherwise) — the strikes around ATM, or around a strike the screen
//! scrolled to, with live Kotak prices — streamed to the dashboard, and the
//! board the manual-order routes check a tap against. The underlying is one
//! of the six indices, or, behind the `stock_charts` switch, any F&O
//! underlying the instrument catalog lists; a stock's ladder is read-only on
//! the owner's desk (`POST /api/execute/buy` resolves index options only).
//!
//! Everything here reads in-memory state only (scrip master, the shared price
//! map, the quote hub, positions, telemetry). It never calls Kotak; the only
//! thing it sends anywhere is feed subscriptions (and their release) through
//! the market-data bridge, and the stock deltas it caches come from Upstox's
//! read-only option chain.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;
use shared_domain::{telemetry, ChartLevels, MonitoredPosition, TradeState, INDEX_NAMES};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use trading_engine::{index_fo_segment, ChainStrike, OptionChain, ScripRecord};

use crate::auth::Identity;
use crate::market_data::StrikeRow;
use crate::paper::PaperPosition;
use crate::quotes::hub::Source as QuoteSource;
use crate::AppState;

/// Strikes streamed each side of the strike the screen is centred on (ATM
/// unless it scrolled). The screen shows ±5 of them; the spare two each side
/// let it hold its rows still while the user's finger is on the ladder.
const ROWS_HALF: usize = 7;
/// Strikes kept on the feed each side of ATM for every index ladder someone
/// signed in has open: ATM−7..ATM+7, calls and puts (30 contracts). Only
/// those get prices; a strike scrolled to past them shows none. A strike that
/// leaves every band (spot moved, or the ladder closed) is unsubscribed once
/// no ladder has wanted it for [`BAND_GRACE`], unless a position holds it.
const BAND_HALF: usize = 7;
/// How long a band strike no ladder wants stays on the feed: spot wobbling
/// across a strike doesn't churn the feed, and a ladder reopened within it
/// has its prices at once.
const BAND_GRACE: Duration = Duration::from_secs(30);
/// How often band strikes nobody wants are looked for.
const BAND_SWEEP_EVERY: Duration = Duration::from_secs(5);
/// Band strikes on the feed at once at most, everyone's together: six full
/// bands. The feed's own cap (200 on the retired HSM feed; 3000 in Kotak's
/// reference client for the native one) must keep room for positions'
/// contracts, whose stops depend on their ticks. Past it a strike shows no
/// price, and the buy route refuses a contract with no recent price: it
/// fails toward not trading. A stock chart's subscription (`candles`, behind
/// `stock_charts`) is counted inside this same total, never on top of it.
const MAX_BAND_SCRIPS: usize = 180;
/// Signed-in users' ladders add strikes only while the total is under this
/// (two bands), so the owner always has room for four more of their own.
const MAX_PUBLIC_BAND_SCRIPS: usize = 60;
/// Upstox's option chain for a stock is read at most this often per expiry,
/// for the deltas the chain route shows.
const STOCK_DELTAS_EVERY: Duration = Duration::from_secs(60);
/// The "band full" warning is logged at most this often: every open ladder
/// asks four times a second.
const BAND_WARN_EVERY: Duration = Duration::from_secs(60);

/// Whose share of the band budget a stream draws on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubPool {
    /// The admin: the owner's desk.
    Owner,
    /// A signed-in user.
    Public,
    /// A visitor: subscribes nothing, and sees prices only for contracts
    /// someone signed in is already streaming — one unsigned request could
    /// otherwise use up the users' whole share for the day.
    None,
}

impl SubPool {
    pub fn of(identity: &Identity) -> Self {
        match identity {
            Identity::Admin { .. } => SubPool::Owner,
            Identity::User { .. } => SubPool::Public,
            Identity::Anonymous => SubPool::None,
        }
    }

    fn budget(self) -> usize {
        match self {
            SubPool::Owner => MAX_BAND_SCRIPS,
            SubPool::Public => MAX_PUBLIC_BAND_SCRIPS,
            SubPool::None => 0,
        }
    }
}
/// How often the stream pushes a frame.
pub const STREAM_EVERY: Duration = Duration::from_millis(250);
/// A feed quieter than this is stale: no manual buy is accepted on it.
pub const MAX_FEED_AGE_SECS: f64 = 5.0;
/// A contract whose own price hasn't ticked for this long is stale: a paper
/// position's chart level doesn't trigger on it, the price may be long gone
/// even while the feed itself is alive. Near-the-money index options tick
/// about once a second.
pub const MAX_CONTRACT_AGE_SECS: f64 = 10.0;
/// A contract whose own price hasn't ticked for this long isn't bought by
/// hand on the owner's desk. Illiquid options trade minutes apart, so this is
/// far longer than [`MAX_CONTRACT_AGE_SECS`] — the owner's call. The buy goes
/// in at market, so the fill may be well away from the price on screen. A
/// paper desk fills at the price it reads, where a stale one would be free
/// P&L, so it keeps [`MAX_CONTRACT_AGE_SECS`].
pub const MAX_BUY_PRICE_AGE_SECS: f64 = 300.0;

/// How old a contract's price may be for a buy from `pool`'s desk.
pub fn buy_max_age_secs(pool: SubPool) -> f64 {
    match pool {
        SubPool::Owner => MAX_BUY_PRICE_AGE_SECS,
        SubPool::Public | SubPool::None => MAX_CONTRACT_AGE_SECS,
    }
}
/// Engine errors are kept this long, so a failed tap can say why.
const RECENT_ERROR_MS: i64 = 60_000;

/// One index's chain for one expiry as last built, with what it was built from.
struct CachedChain {
    chain: Arc<OptionChain>,
    loaded_at: Option<DateTime<FixedOffset>>,
    day: NaiveDate,
}

/// One index's listed expiries, each with its lot size, as last read, with
/// what they were read from.
struct CachedExpiries {
    list: Arc<Vec<(NaiveDate, u32)>>,
    loaded_at: Option<DateTime<FixedOffset>>,
    day: NaiveDate,
}

/// The strikes a board's rows cover.
#[derive(Serialize)]
struct Window {
    from: f64,
    to: f64,
}

#[derive(Default)]
struct Subscriptions {
    /// The bridge channel these were sent on. A re-login starts a fresh
    /// bridge with a fresh channel, which knows none of them.
    channel: Option<mpsc::UnboundedSender<String>>,
    /// The IST day they were sent on: the feed drops band subscriptions at
    /// each day's close, so a new day starts from none.
    day: Option<NaiveDate>,
    /// Every band strike on the feed, with when a ladder last wanted it.
    band: HashMap<String, Instant>,
    /// Every stock chart's subscription (`"ladder": true` on the bridge:
    /// dropped at the close, never released before it), with when it was
    /// sent. Counted with `band` against the one budget, and never taken
    /// off by the band sweep.
    chart: HashMap<String, Instant>,
    indices_sent: bool,
    /// When the "band full" warning was last logged.
    warned_at: Option<Instant>,
}

impl Subscriptions {
    /// Strikes and chart keys on the feed out of the budget.
    fn used(&self) -> usize {
        self.band.len() + self.chart.len()
    }

    /// How many more keys `pool` may put on the feed.
    fn room(&self, pool: SubPool) -> usize {
        pool.budget().saturating_sub(self.used())
    }

    /// Start over: a fresh bridge knows none of these, and the feed drops
    /// bands and chart keys at each day's close.
    fn is_current(&self, tx: &mpsc::UnboundedSender<String>, today: NaiveDate) -> bool {
        self.channel.as_ref().is_some_and(|c| c.same_channel(tx)) && self.day == Some(today)
    }
}

/// Upstox's option chain for one stock expiry, as last read, for the deltas.
struct CachedDeltas {
    read_at: Instant,
    /// IST `YYYY-MM-DD HH:MM:SS` of the read; `None` when it failed.
    updated_at: Option<String>,
    /// By strike in paise; empty when Upstox had none or the read failed.
    rows: Arc<HashMap<i64, StrikeRow>>,
}

/// A stock chain's deltas from Upstox — see [`ExecuteBoard::stock_deltas`].
#[derive(Clone)]
pub struct StockDeltas {
    /// IST `YYYY-MM-DD HH:MM:SS` the chain was read.
    pub updated_at: String,
    /// By strike in paise (`(strike * 100).round()`).
    pub rows: Arc<HashMap<i64, StrikeRow>>,
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
    chains: RwLock<HashMap<(String, NaiveDate), CachedChain>>,
    expiries: RwLock<HashMap<String, CachedExpiries>>,
    subs: Mutex<Subscriptions>,
    errors: std::sync::Mutex<Vec<RecentError>>,
    deltas: Mutex<HashMap<(String, NaiveDate), CachedDeltas>>,
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

    /// The expiries `index`'s options are listed for, from today, earliest
    /// first, each with its lot size — read again whenever the scrip master
    /// is reloaded or the IST date rolls over (yesterday's expiry is gone).
    /// `index` is any underlying the scrip master keys records under: an
    /// index or a stock, each in its own F&O segment (`fo_segment`).
    pub async fn expiries(&self, state: &AppState, index: &str) -> Arc<Vec<(NaiveDate, u32)>> {
        let loaded_at = *state.scrip_loaded_at.read().await;
        let today = shared_domain::today_ist();
        if let Some(c) = self.expiries.read().await.get(index) {
            if c.loaded_at == loaded_at && c.day == today {
                return Arc::clone(&c.list);
            }
        }
        let list = {
            let store = state.scrip_store.read().await;
            Arc::new(store.as_ref().map(|s| s.expiry_lots(index, today)).unwrap_or_default())
        };
        self.expiries.write().await.insert(
            index.to_string(),
            CachedExpiries { list: Arc::clone(&list), loaded_at, day: today },
        );
        list
    }

    /// `index`'s chain for `expiry`, rebuilt whenever the scrip master is
    /// reloaded or the IST date rolls over. `None` once the expiry has passed
    /// or nothing is listed for it.
    pub async fn chain_at(&self, state: &AppState, index: &str, expiry: NaiveDate) -> Option<Arc<OptionChain>> {
        let loaded_at = *state.scrip_loaded_at.read().await;
        let today = shared_domain::today_ist();
        if expiry < today {
            return None;
        }
        let key = (index.to_string(), expiry);
        if let Some(c) = self.chains.read().await.get(&key) {
            if c.loaded_at == loaded_at && c.day == today {
                return Some(Arc::clone(&c.chain));
            }
        }
        let chain = {
            let store = state.scrip_store.read().await;
            Arc::new(store.as_ref()?.chain_for_expiry(index, expiry)?)
        };
        let mut chains = self.chains.write().await;
        // Yesterday's and a reloaded scrip master's chains are never read again.
        chains.retain(|_, c| c.loaded_at == loaded_at && c.day == today);
        chains.insert(key, CachedChain { chain: Arc::clone(&chain), loaded_at, day: today });
        Some(chain)
    }

    /// `index`'s nearest-expiry chain.
    pub async fn chain(&self, state: &AppState, index: &str) -> Option<Arc<OptionChain>> {
        let (expiry, _) = *self.expiries(state, index).await.first()?;
        self.chain_at(state, index, expiry).await
    }

    /// The contract `trading_symbol` names, if it is a listed index option
    /// on any expiry from today — what the Execute screen trades. Comes with
    /// its index and that expiry's chain. Only the six indices' records are
    /// searched, never a stock's: the owner's one-tap buy stays index-only
    /// whatever the `stock_charts` switch says (question 8, to be
    /// re-confirmed before it is ever widened).
    pub async fn find_contract(&self, state: &AppState, trading_symbol: &str) -> Option<(String, Arc<OptionChain>, ScripRecord)> {
        let record = state.scrip_store.read().await.as_ref()?.find_index_option(trading_symbol)?;
        let index = index_option(&record, shared_domain::today_ist())?;
        let chain = self.chain_at(state, index, record.expiry_date).await?;
        Some((index.to_string(), chain, record))
    }

    /// Upstox's deltas for a stock's `expiry` chain, read at most once a
    /// minute per expiry (a failed read is not retried for a minute either).
    /// `None` without Upstox, without an ISIN for the stock, or while the
    /// last read failed — the chain route then shows no delta, never an
    /// assumed one.
    pub async fn stock_deltas(&self, state: &AppState, underlying: &str, expiry: NaiveDate) -> Option<StockDeltas> {
        let client = state.upstox.as_ref()?;
        let key = (underlying.to_string(), expiry);
        let now = Instant::now();
        {
            let deltas = self.deltas.lock().await;
            if let Some(c) = deltas.get(&key).filter(|c| now.duration_since(c.read_at) < STOCK_DELTAS_EVERY) {
                return c.updated_at.clone().map(|updated_at| StockDeltas { updated_at, rows: Arc::clone(&c.rows) });
            }
        }
        let upstox_key = state.instruments.spot_key(underlying).and_then(|k| state.instruments.upstox_key(&k))?;
        // Fetched outside the lock: a slow Upstox must not hold up another
        // underlying's chain. Two callers racing both fetch; harmless.
        let read = client.option_chain(&upstox_key, expiry).await;
        let (updated_at, rows) = match read {
            Ok(chain) => {
                let rows: HashMap<i64, StrikeRow> = crate::market_data::to_strike_rows(&chain)
                    .into_iter()
                    .map(|r| ((r.strike * 100.0).round() as i64, r))
                    .collect();
                (Some(shared_domain::current_ist_timestamp_string()), rows)
            }
            Err(e) => {
                tracing::debug!(underlying, expiry = %expiry, error = %e, "Upstox option chain for a stock failed — no deltas for a minute");
                (None, HashMap::new())
            }
        };
        let rows = Arc::new(rows);
        let mut deltas = self.deltas.lock().await;
        deltas.retain(|_, c| now.duration_since(c.read_at) < 10 * STOCK_DELTAS_EVERY);
        deltas.insert(key, CachedDeltas { read_at: Instant::now(), updated_at: updated_at.clone(), rows: Arc::clone(&rows) });
        updated_at.map(|updated_at| StockDeltas { updated_at, rows })
    }

    /// Put one stock chart's feed key on the feed out of `pool`'s share of
    /// the ladder budget (`"ladder": true`: the bridge drops it at the close
    /// and the band sweep never releases it). `true` when it is on the feed
    /// (now or already); `false` when the budget is full or nothing can be
    /// sent, and the chart then has no Kotak ticks — the candle service
    /// falls back to Upstox's minutes. A visitor's pool subscribes nothing.
    pub async fn reserve_chart(&self, ws_tx: &Mutex<Option<mpsc::UnboundedSender<String>>>, key: &str, pool: SubPool) -> bool {
        if pool == SubPool::None {
            return false;
        }
        let Some(tx) = ws_tx.lock().await.clone() else { return false };
        let today = shared_domain::today_ist();
        let now = Instant::now();
        let mut subs = self.subs.lock().await;
        if !subs.is_current(&tx, today) {
            *subs = Subscriptions { channel: Some(tx.clone()), day: Some(today), ..Default::default() };
        }
        if subs.chart.contains_key(key) {
            return true;
        }
        // A strike some ladder's band already streams: it becomes the
        // chart's for the day (the band sweep must not release it), and the
        // count does not change.
        if subs.band.remove(key).is_some() {
            let msg = serde_json::json!({ "action": "subscribe", "scrips": key, "ladder": true });
            let _ = tx.send(msg.to_string());
            subs.chart.insert(key.to_string(), now);
            return true;
        }
        if subs.room(pool) == 0 {
            if subs.warned_at.map_or(true, |t| now.duration_since(t) >= BAND_WARN_EVERY) {
                subs.warned_at = Some(now);
                tracing::warn!(
                    "Ladder budget full ({pool:?}): {} of {} keys on the feed; the chart for {key} gets no Kotak ticks today",
                    subs.used(),
                    pool.budget()
                );
            }
            return false;
        }
        let msg = serde_json::json!({ "action": "subscribe", "scrips": key, "ladder": true });
        if tx.send(msg.to_string()).is_err() {
            return false;
        }
        subs.chart.insert(key.to_string(), now);
        true
    }

    /// Make sure the feed streams every index's spot value and `keys` (a
    /// band, nearest ATM first), paid for out of `pool`'s share, and note
    /// that a ladder wants them now.
    async fn ensure_subscribed(&self, ws_tx: &Mutex<Option<mpsc::UnboundedSender<String>>>, keys: &[String], pool: SubPool) {
        if pool == SubPool::None {
            return;
        }
        let Some(tx) = ws_tx.lock().await.clone() else { return };
        let today = shared_domain::today_ist();
        let now = Instant::now();
        let mut subs = self.subs.lock().await;
        if !subs.is_current(&tx, today) {
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
        let mut wanted = Vec::new();
        for key in keys {
            match subs.band.get_mut(key) {
                Some(at) => *at = now,
                None => wanted.push(key.clone()),
            }
        }
        if wanted.is_empty() {
            return;
        }
        let room = subs.room(pool);
        if wanted.len() > room && subs.warned_at.map_or(true, |t| now.duration_since(t) >= BAND_WARN_EVERY) {
            subs.warned_at = Some(now);
            tracing::warn!(
                "Ladder band full ({pool:?}): {} of {} keys on the feed, {} more asked for; they show no feed price until a band frees",
                subs.used(),
                pool.budget(),
                wanted.len()
            );
        }
        wanted.truncate(room);
        if wanted.is_empty() {
            return;
        }
        // `band`: released once no ladder wants them, and dropped by the feed
        // at the day's close (unlike a position's own subscription).
        let msg = serde_json::json!({ "action": "subscribe", "scrips": wanted.join("&"), "band": true });
        if tx.send(msg.to_string()).is_ok() {
            for key in wanted {
                subs.band.insert(key, now);
            }
        }
    }

    /// Every [`BAND_SWEEP_EVERY`], take off the feed the band strikes no
    /// ladder has wanted for [`BAND_GRACE`].
    pub fn spawn_band_release(state: AppState) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(BAND_SWEEP_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                state.execute.release_unwanted(&state).await;
            }
        });
    }

    /// Unsubscribe the band strikes no ladder wants any more — never one a
    /// position holds, the owner's or a user's, whatever the bands say. (The
    /// market-data stream checks again, against what positions and charts
    /// subscribed.)
    async fn release_unwanted(&self, state: &AppState) {
        // Read before the subscriptions lock is taken, which nothing holds
        // while waiting on these.
        let mut held: HashSet<String> = state
            .positions
            .read()
            .await
            .iter()
            .filter(|p| !matches!(p.state, TradeState::Closed))
            .filter_map(|p| p.ws_scrip_key.clone())
            .collect();
        held.extend(state.paper.feed_keys().await.0);
        let Some(tx) = state.ws_tx.lock().await.clone() else { return };
        let mut subs = self.subs.lock().await;
        // A fresh bridge or a new day: the next ladder frame starts over.
        if !subs.is_current(&tx, shared_domain::today_ist()) {
            return;
        }
        // A stock chart's key stays for the day, whatever the bands say.
        held.extend(subs.chart.keys().cloned());
        let stale = unwanted_band_keys(&subs.band, &held, Instant::now());
        if stale.is_empty() {
            return;
        }
        let msg = serde_json::json!({ "action": "unsubscribe", "scrips": stale.join("&"), "band": true });
        if tx.send(msg.to_string()).is_ok() {
            for key in &stale {
                subs.band.remove(key);
            }
            tracing::info!("Ladder band: released {} strikes no ladder wants ({} left on the feed)", stale.len(), subs.band.len());
        }
    }
}

/// The band strikes no ladder has wanted for [`BAND_GRACE`] by `now`, leaving
/// out any in `held` (positions' contracts), sorted.
fn unwanted_band_keys(band: &HashMap<String, Instant>, held: &HashSet<String>, now: Instant) -> Vec<String> {
    let mut keys: Vec<String> = band
        .iter()
        .filter(|(key, at)| now.duration_since(**at) >= BAND_GRACE && !held.contains(*key))
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();
    keys
}

/// The feed keys of the strikes within `half` of `atm`, calls and puts,
/// nearest ATM first, so a band cut short by the budget keeps the strikes
/// that matter most.
fn band_keys(strikes: &[ChainStrike], atm: usize, half: usize) -> Vec<String> {
    let mut picked: Vec<usize> = window(strikes.len(), atm, half).collect();
    picked.sort_by_key(|i| i.abs_diff(atm));
    picked
        .into_iter()
        .flat_map(|i| [strikes[i].ce.as_ref(), strikes[i].pe.as_ref()])
        .flatten()
        .map(ScripRecord::ws_key)
        .collect()
}

/// Whether `record` is a listed option of `underlying` a ladder may show:
/// a CE/PE with a readable strike, in the underlying's own F&O segment
/// (SENSEX and BANKEX in `bse_fo`, every other index and every stock in
/// `nse_fo`), expiring on or after `today`. Futures, expired contracts, a
/// contract in the other exchange's segment and another underlying's rows
/// are not.
fn listed_option(record: &ScripRecord, underlying: &str, today: NaiveDate) -> bool {
    record.symbol_name == underlying
        && record.exchange_segment_code == index_fo_segment(underlying)
        && record.normalized_strike().is_some()
        && record.expiry_date >= today
}

/// The index `record` is an option of, if it is one the owner's one-tap buy
/// may trade: [`listed_option`] on one of the six indices. Stocks give
/// `None` whatever the `stock_charts` switch says.
fn index_option(record: &ScripRecord, today: NaiveDate) -> Option<&'static str> {
    let index = INDEX_NAMES.iter().copied().find(|i| *i == record.symbol_name)?;
    listed_option(record, index, today).then_some(index)
}

/// The underlying `wanted` names for a ladder, chain or expiry list: one of
/// the six indices always; with the `stock_charts` switch on, any F&O
/// underlying the instrument catalog lists (a stock, or an index the app has
/// no feed name for). `None` for anything else, so nothing typed reaches
/// the scrip master unchecked.
pub async fn underlying_named(state: &AppState, wanted: &str) -> Option<String> {
    let wanted = wanted.trim().to_ascii_uppercase();
    if let Some(index) = INDEX_NAMES.iter().find(|i| **i == wanted) {
        return Some(index.to_string());
    }
    if !state.flags.read().await.stock_charts {
        return None;
    }
    let u = state.instruments.underlying(&wanted)?;
    u.has_fo.then_some(u.symbol)
}

/// What a ladder's underlying is: `"index"` for the six, `"stock"` for
/// anything else the catalog lists.
pub fn underlying_kind(underlying: &str) -> &'static str {
    if INDEX_NAMES.contains(&underlying) { "index" } else { "stock" }
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
    /// The previous session's close: the day's change in points is `spot` less this.
    prev_close: Option<f64>,
    change_pct: Option<f64>,
}

#[derive(Serialize)]
struct Board {
    /// The underlying, under its older name: `NIFTY`, or a stock such as
    /// `RELIANCE` behind the `stock_charts` switch.
    index: String,
    /// The same as `index`.
    underlying: String,
    /// `"index"` or `"stock"`. A stock's rows are read-only on the owner's
    /// desk: `POST /api/execute/buy` refuses them.
    kind: &'static str,
    /// `YYYY-MM-DD`: the expiry the rows are from.
    expiry: String,
    days_to_expiry: i64,
    /// Every expiry this index's options are listed for, `YYYY-MM-DD`,
    /// earliest first.
    expiries: Vec<String>,
    lot_size: u32,
    spot: Option<f64>,
    /// `"kotak"` (the feed's tick: an index's value, or a stock's cash
    /// price while a chart streams it), `"polled"` (Kotak's REST quote
    /// through the quote hub, ~2 s), `"upstox"` (the 60 s option-chain poll
    /// for an index; the hub's Upstox fallback for a stock), `"close"` (the
    /// 15:35 snapshot, after hours) or `"parity"` (estimated from the
    /// ladder's own call/put prices).
    spot_source: Option<&'static str>,
    atm: Option<f64>,
    /// The strikes `rows` run from and to: the subscribed window, around ATM
    /// or around the strike the stream was asked to centre on.
    window: Window,
    rows: Vec<Row>,
    /// A contract price older than this (ms) can't be bought from this
    /// board: 5 min on the owner's desk, 10 s on a paper desk.
    buy_max_age_ms: u64,
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
    /// Milliseconds since this contract's own last tick — or, for a polled
    /// row, since the quote hub read it.
    age_ms: Option<u64>,
    /// The price is the quote hub's (Kotak's REST quote every ~2 s, or the
    /// close after hours), not the feed's: the strike is past the feed
    /// budget. No buy route accepts a polled price — the paper and live buys
    /// read the feed alone.
    polled: bool,
}

#[derive(Serialize)]
struct PositionRow {
    id: String,
    manual: bool,
    index: String,
    strike: Option<f64>,
    option_type: Option<String>,
    /// `YYYY-MM-DD`, once the contract is resolved.
    expiry: Option<String>,
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
/// normal session, inside the engine's own 09:15–15:40 window — or throughout
/// an active special session on its date.
pub fn manual_buy_window_open_at(now: chrono::DateTime<chrono::FixedOffset>) -> bool {
    use chrono::Timelike;
    shared_domain::in_special_session_at(now)
        || (shared_domain::is_regular_market_hours_at(now) && (now.hour(), now.minute()) < (15, 30))
}

pub fn manual_buy_window_open() -> bool {
    manual_buy_window_open_at(shared_domain::now_ist())
}

/// Refusal message for when manual buys are closed: mentions today's special session
/// window if one is set for today, else regular weekday hours.
pub fn manual_buy_closed_message() -> String {
    if let Some(session) = shared_domain::special_session_on(shared_domain::today_ist()) {
        format!(
            "Market is closed — today's special session runs {}–{} IST",
            shared_domain::format_hhmm(session.start),
            shared_domain::format_hhmm(session.end)
        )
    } else {
        "Market is closed — manual buys run 09:15–15:30 IST".to_string()
    }
}

/// Live index value and its % change on the previous close, from Kotak's feed.
/// The index's value on Kotak's feed, and the previous session's close when
/// the feed has it.
fn kotak_index(state: &AppState, index: &str) -> Option<(f64, Option<f64>)> {
    let (_, name) = shared_domain::kotak_index_feed(index)?;
    let spot = price(state, &shared_domain::index_price_key(name))?;
    let close = price(state, &shared_domain::index_close_key(name)).filter(|c| *c > 0.0);
    Some((spot, close))
}

async fn upstox_spot(state: &AppState, index: &str) -> Option<f64> {
    state.market.indices.read().await.get(index).and_then(|l| l.spot).filter(|s| *s > 0.0)
}

async fn upstox_prev_close(state: &AppState, index: &str) -> Option<f64> {
    state.market.indices.read().await.get(index).and_then(|l| l.prev_close).filter(|c| *c > 0.0)
}

/// What a quote hub source is called on a board.
fn hub_source_label(source: QuoteSource) -> &'static str {
    match source {
        QuoteSource::Feed => "kotak",
        QuoteSource::Rest => "polled",
        QuoteSource::Upstox => "upstox",
        QuoteSource::Close => "close",
    }
}

/// A stock's spot: its cash price from the feed while a chart streams it,
/// else the quote hub's (asked to keep polling it). `None` for an
/// underlying with no cash row (`NIFTYNXT50`), or before any quote.
fn stock_spot(state: &AppState, underlying: &str, now: DateTime<FixedOffset>) -> Option<(f64, &'static str)> {
    let key = state.instruments.spot_key(underlying)?;
    if let Some(ltp) = price(state, &key).filter(|_| contract_age_secs(&key).is_some()) {
        return Some((ltp, "kotak"));
    }
    state.quotes.want([key.clone()]);
    let quote = state.quotes.get_at(&key, now)?;
    (quote.ltp > 0.0).then(|| (quote.ltp, hub_source_label(quote.source)))
}

/// The row for one option: the feed's price with its own tick age; without
/// one, and with stock charts on, the quote hub's with the hub's read time
/// (the key is added to `polled` so the hub keeps asking for it).
fn row_quote(state: &AppState, r: &ScripRecord, stocks_on: bool, now: DateTime<FixedOffset>, polled: &mut Vec<String>) -> Quote {
    let key = r.ws_key();
    let sym = r.trading_symbol.clone();
    let age = contract_age_secs(&key);
    if let Some(ltp) = price(state, &key).filter(|_| age.is_some()) {
        return Quote { sym, ltp: Some(ltp), age_ms: age.map(|s| (s * 1000.0) as u64), polled: false };
    }
    if !stocks_on {
        return Quote { sym, ltp: None, age_ms: None, polled: false };
    }
    polled.push(key.clone());
    match state.quotes.get_at(&key, now).filter(|q| q.ltp > 0.0) {
        Some(q) => Quote { sym, ltp: Some(q.ltp), age_ms: Some(now.timestamp_millis().saturating_sub(q.at).max(0) as u64), polled: true },
        None => Quote { sym, ltp: None, age_ms: None, polled: true },
    }
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

/// The board for `index` (an index, or a stock with `stocks_on`): its
/// `expiry`'s chain (the nearest when `None`; an unlisted one is an error),
/// with the band around ATM subscribed (out of `pool`'s share) and the rows
/// streamed around `centre` (ATM when `None`).
async fn build_board(state: &AppState, index: &str, expiry: Option<NaiveDate>, centre: Option<f64>, pool: SubPool, stocks_on: bool) -> Result<Board, String> {
    let kind = underlying_kind(index);
    if kind == "stock" && !stocks_on {
        return Err("Stock ladders are off — the admin turns them on with the `stock_charts` switch".to_string());
    }
    if state.scrip_store.read().await.is_none() {
        return Err("Scrip Master not loaded yet — connect Kotak first".to_string());
    }
    let expiries = state.execute.expiries(state, index).await;
    let listed = |e: NaiveDate| expiries.iter().any(|(d, _)| *d == e);
    // An expiry asked for by name must exist: a ladder quietly swapped to
    // another expiry could sell a tap the wrong contract.
    let expiry = match expiry {
        Some(e) if listed(e) => e,
        Some(e) => return Err(format!("No {index} options listed for {}", e.format("%Y-%m-%d"))),
        None => expiries.first().map(|(d, _)| *d).ok_or_else(|| format!("No {index} option contracts in the Scrip Master"))?,
    };
    let chain = state
        .execute
        .chain_at(state, index, expiry)
        .await
        .ok_or_else(|| format!("No {index} option contracts in the Scrip Master"))?;

    // Spot, best source first: the feed's index value; a stock's cash price
    // from the feed, else the quote hub (Kotak REST, Upstox while Kotak is
    // down, the close after hours); Upstox's index poll; put-call parity.
    let now = shared_domain::now_ist();
    let (spot, spot_source) = match kotak_index(state, index) {
        Some((s, _)) => (Some(s), Some("kotak")),
        None => match if kind == "stock" { stock_spot(state, index, now) } else { None } {
            Some((s, source)) => (Some(s), Some(source)),
            None => match upstox_spot(state, index).await {
                Some(s) => (Some(s), Some("upstox")),
                None => match parity_spot(state, &chain.strikes) {
                    Some(s) => (Some(s), Some("parity")),
                    None => (None, None),
                },
            },
        },
    };

    // Without any spot yet, centre on the middle of the chain so prices start
    // flowing; parity takes over as soon as they do.
    let atm = spot.and_then(|s| atm_index(&chain.strikes, s)).unwrap_or(chain.strikes.len() / 2);
    // Only the band around ATM is on the feed, wherever the screen scrolled.
    state.execute.ensure_subscribed(&state.ws_tx, &band_keys(&chain.strikes, atm, BAND_HALF), pool).await;
    // The screen may have scrolled away from ATM: its rows are streamed there.
    let centre = centre.and_then(|c| atm_index(&chain.strikes, c)).unwrap_or(atm);
    let range = window(chain.strikes.len(), centre, ROWS_HALF);

    // The whole window goes out: v1 shows ATM ± 5 of it and v2 merges it
    // into the full strike list, and while a finger is on the ladder it
    // holds its rows still — the spare strikes keep their prices live
    // meanwhile. A row the feed has no price for is polled through the hub
    // (stock charts on), so every row has a price, labelled.
    let mut polled: Vec<String> = Vec::new();
    let rows: Vec<Row> = chain.strikes[range]
        .iter()
        .map(|s| Row {
            strike: s.strike,
            ce: s.ce.as_ref().map(|r| row_quote(state, r, stocks_on, now, &mut polled)),
            pe: s.pe.as_ref().map(|r| row_quote(state, r, stocks_on, now, &mut polled)),
        })
        .collect();
    if !polled.is_empty() {
        state.quotes.want(polled);
    }
    let window = Window {
        from: rows.first().map(|r| r.strike).unwrap_or(0.0),
        to: rows.last().map(|r| r.strike).unwrap_or(0.0),
    };

    Ok(Board {
        index: index.to_string(),
        underlying: index.to_string(),
        kind,
        expiry: chain.expiry.format("%Y-%m-%d").to_string(),
        days_to_expiry: (chain.expiry - shared_domain::today_ist()).num_days(),
        expiries: expiries.iter().map(|(d, _)| d.format("%Y-%m-%d").to_string()).collect(),
        lot_size: chain.lot_size,
        spot,
        spot_source,
        atm: spot.map(|_| chain.strikes[atm].strike),
        window,
        rows,
        buy_max_age_ms: (buy_max_age_secs(pool) * 1000.0) as u64,
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
        expiry: p.resolved_expiry.map(|d| d.format("%Y-%m-%d").to_string()),
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
        chart: p.chart.clone(),
    }
}

/// A user's paper position, as the stream's row: always a manual scalp,
/// filled at once, never busy.
fn paper_row(state: &AppState, p: &PaperPosition) -> PositionRow {
    let ltp = price(state, &p.ws_key);
    PositionRow {
        id: p.id.clone(),
        manual: true,
        index: p.index.clone(),
        strike: Some(p.strike),
        option_type: Some(p.option_type.clone()),
        expiry: Some(p.expiry.format("%Y-%m-%d").to_string()),
        symbol: p.trading_symbol.clone(),
        lot_size: p.lot_size.max(1),
        qty: p.qty,
        avg: p.avg_price,
        ltp,
        pnl: ltp.map(|l| (l - p.avg_price) * f64::from(p.qty)),
        placing: false,
        adding: false,
        exiting: false,
        halted: false,
        chart: p.chart.clone(),
    }
}

/// One frame of the Execute screen for `index` (an index, or any F&O
/// underlying `underlying_named` accepted): its `expiry`'s ladder (the
/// nearest when `None`), the subscribed window around `centre` (ATM when
/// `None`). The market data is the same for everyone; whose positions ride
/// along, and whether the owner's mode, kill switch and Kotak state show,
/// depends on `identity`: the admin sees the engine's, a user their own
/// paper positions, a visitor none.
pub async fn build_frame(state: &AppState, identity: &Identity, index: &str, expiry: Option<NaiveDate>, centre: Option<f64>) -> ExecuteFrame {
    let owner_view = identity.can_read_owner_data();
    let (mode, kill_switch) = if owner_view {
        let cfg = state.trading_cfg.read().await;
        (cfg.mode.clone(), cfg.kill_switch_active)
    } else {
        // Users trade on paper whatever the owner does, and the owner's kill
        // switch is not theirs.
        ("PAPER".to_string(), false)
    };

    let mut indices = Vec::with_capacity(INDEX_NAMES.len());
    for &name in INDEX_NAMES {
        let kotak = kotak_index(state, name);
        let spot = match kotak {
            Some((s, _)) => Some(s),
            None => upstox_spot(state, name).await,
        };
        // Kotak's previous close, else Upstox's (NIFTY, BANKNIFTY and
        // SENSEX): Kotak's is gone after its session ends.
        let prev_close = match kotak.and_then(|(_, c)| c) {
            Some(c) => Some(c),
            None => upstox_prev_close(state, name).await,
        };
        let change_pct = spot.zip(prev_close).map(|(s, c)| (s - c) / c * 100.0);
        indices.push(IndexHeadline { index: name, spot, prev_close, change_pct });
    }

    let stocks_on = state.flags.read().await.stock_charts;
    let (board, board_error) = match build_board(state, index, expiry, centre, SubPool::of(identity), stocks_on).await {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e)),
    };

    // What is actually held (or being bought by hand) — a signal still
    // waiting for its trigger is not a position yet. The engine's positions
    // are the owner's: nobody else sees them. A user sees their own paper
    // positions instead.
    let positions = match identity {
        Identity::Admin { .. } => state
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
            .collect(),
        Identity::User { user_id } => state.paper.open(*user_id).await.iter().map(|p| paper_row(state, p)).collect(),
        Identity::Anonymous => Vec::new(),
    };

    let ts = shared_domain::now_ist().timestamp_millis();
    ExecuteFrame {
        ts,
        mode,
        kill_switch,
        market_open: manual_buy_window_open(),
        feed: feed_status(),
        kotak_ready: owner_view && kotak_ready(state),
        indices,
        board,
        board_error,
        positions,
        errors: if owner_view { state.execute.recent_errors(ts) } else { Vec::new() },
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

    #[test]
    fn the_window_follows_the_strike_the_screen_scrolled_to() {
        let chain: Vec<ChainStrike> = (0..40).map(|i| strike(22000.0 + 50.0 * i as f64)).collect();
        let atm = atm_index(&chain, 22980.0).unwrap();
        assert_eq!(window(chain.len(), atm, 2), 18..23);
        // Centred far above ATM, the window moves there; off the end it clamps.
        let centre = atm_index(&chain, 23800.0).unwrap();
        assert_eq!(window(chain.len(), centre, 2), 34..39);
        let centre = atm_index(&chain, 99999.0).unwrap();
        assert_eq!(window(chain.len(), centre, 2), 37..40);
    }

    fn record(symbol_name: &str, trading_symbol: &str, segment: &str, raw_strike: f64, opt: &str, expiry: (i32, u32, u32)) -> ScripRecord {
        ScripRecord {
            instrument_token: "1".to_string(),
            trading_symbol: trading_symbol.to_string(),
            symbol_name: symbol_name.to_string(),
            exchange_segment_code: segment.to_string(),
            strike_price: raw_strike,
            option_type: opt.to_string(),
            expiry_date: NaiveDate::from_ymd_opt(expiry.0, expiry.1, expiry.2).unwrap(),
            lot_size: 65,
            tick_size: 0.05,
        }
    }

    #[test]
    fn the_admin_draws_on_the_owners_budget_users_on_a_share_and_visitors_on_none() {
        assert_eq!(SubPool::of(&Identity::Admin { user_id: 1 }), SubPool::Owner);
        assert_eq!(SubPool::of(&Identity::User { user_id: 2 }), SubPool::Public);
        assert_eq!(SubPool::of(&Identity::Anonymous), SubPool::None);
        assert_eq!(SubPool::None.budget(), 0);
        // The public share sits inside the one total, leaving the owner four bands.
        assert!(SubPool::Public.budget() + 4 * (2 * BAND_HALF + 1) * 2 <= SubPool::Owner.budget());
        assert_eq!(SubPool::Owner.budget(), MAX_BAND_SCRIPS);
    }

    #[test]
    fn only_the_owners_desk_buys_on_an_old_price() {
        assert_eq!(buy_max_age_secs(SubPool::Owner), MAX_BUY_PRICE_AGE_SECS);
        // A paper fill takes the price it reads: an old one would be free P&L.
        assert_eq!(buy_max_age_secs(SubPool::Public), MAX_CONTRACT_AGE_SECS);
        assert_eq!(buy_max_age_secs(SubPool::None), MAX_CONTRACT_AGE_SECS);
        assert!(MAX_CONTRACT_AGE_SECS < MAX_BUY_PRICE_AGE_SECS);
    }

    fn priced(k: f64, token: u32) -> ChainStrike {
        let leg = |opt: &str, tok: u32| ScripRecord {
            instrument_token: tok.to_string(),
            exchange_segment_code: "nse_fo".to_string(),
            option_type: opt.to_string(),
            ..record("NIFTY", "X", "nse_fo", k * 100.0, opt, (2026, 11, 10))
        };
        ChainStrike { strike: k, ce: Some(leg("CE", token)), pe: Some(leg("PE", token + 1)) }
    }

    #[test]
    fn the_band_is_atm_plus_minus_seven_nearest_first() {
        let chain: Vec<ChainStrike> = (0..60).map(|i| priced(24000.0 + 50.0 * i as f64, 1000 + 2 * i)).collect();
        let atm = atm_index(&chain, 25010.0).unwrap();
        assert_eq!(chain[atm].strike, 25000.0);
        let keys = band_keys(&chain, atm, BAND_HALF);
        assert_eq!(keys.len(), 30);
        // ATM's call and put first, then one strike below and above, and on.
        assert_eq!(&keys[..2], [chain[atm].ce.as_ref().unwrap().ws_key(), chain[atm].pe.as_ref().unwrap().ws_key()]);
        assert_eq!(keys[2], chain[atm - 1].ce.as_ref().unwrap().ws_key());
        assert_eq!(keys[4], chain[atm + 1].ce.as_ref().unwrap().ws_key());
        assert!(keys.contains(&chain[atm - 7].pe.as_ref().unwrap().ws_key()));
        assert!(keys.contains(&chain[atm + 7].ce.as_ref().unwrap().ws_key()));
        assert!(!keys.contains(&chain[atm + 8].ce.as_ref().unwrap().ws_key()));
        assert!(!keys.contains(&chain[atm - 8].pe.as_ref().unwrap().ws_key()));
        // Near the chain's end it clamps.
        assert_eq!(band_keys(&chain, 2, BAND_HALF).len(), 2 * 10);
    }

    #[test]
    fn only_unwanted_band_strikes_no_position_holds_are_released() {
        let now = Instant::now();
        let Some(old) = now.checked_sub(BAND_GRACE + Duration::from_secs(1)) else { return };
        let fresh = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
        let band: HashMap<String, Instant> = [
            ("nse_fo|1", old),
            ("nse_fo|2", old),
            ("nse_fo|3", fresh),
            ("nse_fo|4", old),
        ]
        .into_iter()
        .map(|(k, at)| (k.to_string(), at))
        .collect();
        let held: HashSet<String> = ["nse_fo|2".to_string()].into();
        assert_eq!(unwanted_band_keys(&band, &held, now), ["nse_fo|1", "nse_fo|4"]);
        assert!(unwanted_band_keys(&band, &held, fresh).len() <= 2);
    }

    #[test]
    fn a_ladder_shows_an_underlyings_listed_options_in_its_own_segment() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let rel = |sym: &str, seg: &str, opt: &str, exp: (i32, u32, u32)| record("RELIANCE", sym, seg, 140000.0, opt, exp);
        assert!(listed_option(&rel("RELIANCE26NOV1400CE", "nse_fo", "CE", (2026, 11, 24)), "RELIANCE", today));
        assert!(listed_option(&rel("RELIANCE26NOV1400PE", "nse_fo", "PE", (2026, 11, 24)), "RELIANCE", today));
        // Expired, a future, the BSE listing, another underlying's row.
        assert!(!listed_option(&rel("RELIANCE26OCT1400CE", "nse_fo", "CE", (2026, 10, 23)), "RELIANCE", today));
        assert!(!listed_option(&record("RELIANCE", "RELIANCE26NOVFUT", "nse_fo", 0.0, "XX", (2026, 11, 24)), "RELIANCE", today));
        assert!(!listed_option(&rel("RELIANCE26NOV1400CE", "bse_fo", "CE", (2026, 11, 24)), "RELIANCE", today));
        assert!(!listed_option(&rel("RELIANCE26NOV1400CE", "nse_fo", "CE", (2026, 11, 24)), "TCS", today));
        // The indices keep their segments.
        assert!(listed_option(&record("SENSEX", "SENSEX26O2982000CE", "bse_fo", 8200000.0, "CE", (2026, 10, 29)), "SENSEX", today));
        assert!(!listed_option(&record("SENSEX", "SENSEX26O2982000CE", "nse_fo", 8200000.0, "CE", (2026, 10, 29)), "SENSEX", today));
        assert!(listed_option(&record("NIFTY", "NIFTY26O2725100CE", "nse_fo", 2510000.0, "CE", (2026, 10, 27)), "NIFTY", today));
        assert_eq!(underlying_kind("NIFTY"), "index");
        assert_eq!(underlying_kind("BANKEX"), "index");
        assert_eq!(underlying_kind("RELIANCE"), "stock");
        assert_eq!(underlying_kind("NIFTYNXT50"), "stock", "an index the app has no feed name for is treated as a stock");
    }

    #[test]
    fn only_listed_index_options_can_be_bought() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        // Any expiry from today, weekly or monthly.
        assert_eq!(index_option(&record("NIFTY", "NIFTY26O2725100CE", "nse_fo", 2510000.0, "CE", (2026, 10, 27)), today), Some("NIFTY"));
        assert_eq!(index_option(&record("NIFTY", "NIFTY26N0325100PE", "nse_fo", 2510000.0, "PE", (2026, 11, 3)), today), Some("NIFTY"));
        assert_eq!(index_option(&record("NIFTY", "NIFTY26NOV25100CE", "nse_fo", 2510000.0, "CE", (2026, 11, 24)), today), Some("NIFTY"));
        assert_eq!(index_option(&record("SENSEX", "SENSEX26O2982000CE", "bse_fo", 8200000.0, "CE", (2026, 10, 29)), today), Some("SENSEX"));
        // Expired, a future, a stock option, or an index option in the wrong segment.
        assert_eq!(index_option(&record("NIFTY", "NIFTY26O2225100CE", "nse_fo", 2510000.0, "CE", (2026, 10, 22)), today), None);
        assert_eq!(index_option(&record("NIFTY", "NIFTY26OCTFUT", "nse_fo", 0.0, "XX", (2026, 10, 27)), today), None);
        assert_eq!(index_option(&record("RELIANCE", "RELIANCE26OCT1400CE", "nse_fo", 140000.0, "CE", (2026, 10, 27)), today), None);
        assert_eq!(index_option(&record("SENSEX", "SENSEX26O2982000CE", "nse_fo", 8200000.0, "CE", (2026, 10, 29)), today), None);
        // A stock option is listed for its ladder, and still never for the buy.
        let stock = record("RELIANCE", "RELIANCE26NOV1400CE", "nse_fo", 140000.0, "CE", (2026, 11, 24));
        assert!(listed_option(&stock, "RELIANCE", today));
        assert_eq!(index_option(&stock, today), None);
    }

    /// A bridge channel and the lock the board takes it from.
    fn bridge() -> (Mutex<Option<mpsc::UnboundedSender<String>>>, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Mutex::new(Some(tx)), rx)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(serde_json::from_str(&msg).unwrap());
        }
        out
    }

    #[tokio::test]
    async fn a_stock_chart_draws_on_the_same_budget_as_the_bands() {
        let board = ExecuteBoard::new();
        let (ws_tx, mut rx) = bridge();

        // A chart's key goes out with the ladder flag, once.
        assert!(board.reserve_chart(&ws_tx, "nse_cm|2885", SubPool::Owner).await);
        assert!(board.reserve_chart(&ws_tx, "nse_cm|2885", SubPool::Owner).await, "already on the feed");
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], serde_json::json!({ "action": "subscribe", "scrips": "nse_cm|2885", "ladder": true }));
        {
            let subs = board.subs.lock().await;
            assert_eq!((subs.band.len(), subs.chart.len(), subs.used()), (0, 1, 1));
            assert_eq!(subs.room(SubPool::Owner), MAX_BAND_SCRIPS - 1);
            assert_eq!(subs.room(SubPool::Public), MAX_PUBLIC_BAND_SCRIPS - 1);
        }

        // Users' charts fill their share; the next one is refused and sends nothing.
        for i in 1..MAX_PUBLIC_BAND_SCRIPS {
            assert!(board.reserve_chart(&ws_tx, &format!("nse_cm|{i}"), SubPool::Public).await, "{i}");
        }
        assert!(!board.reserve_chart(&ws_tx, "nse_cm|refused", SubPool::Public).await);
        assert_eq!(drain(&mut rx).len(), MAX_PUBLIC_BAND_SCRIPS - 1);
        assert!(!board.subs.lock().await.chart.contains_key("nse_cm|refused"));
        // The owner still has room, and a visitor never subscribes.
        assert!(board.reserve_chart(&ws_tx, "nse_cm|owner", SubPool::Owner).await);
        assert!(!board.reserve_chart(&ws_tx, "nse_cm|visitor", SubPool::None).await);
        assert_eq!(drain(&mut rx).len(), 1);

        // A band asked for next counts the chart keys: with the users' share
        // used up, a user's band gets nothing, and the owner's band is cut
        // to what is left of the total.
        let chain: Vec<ChainStrike> = (0..60).map(|i| priced(24000.0 + 50.0 * i as f64, 1000 + 2 * i)).collect();
        let keys = band_keys(&chain, 30, BAND_HALF);
        board.ensure_subscribed(&ws_tx, &keys, SubPool::Public).await;
        // The first band of the day also puts the six indices on the feed.
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1, "no room in the users' share: only the indices go out");
        assert_eq!(sent[0]["action"], serde_json::json!("subscribe_indices"));
        let used_before = board.subs.lock().await.used();
        board.ensure_subscribed(&ws_tx, &keys, SubPool::Owner).await;
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["band"], serde_json::json!(true));
        let subs = board.subs.lock().await;
        assert_eq!(subs.band.len(), keys.len().min(MAX_BAND_SCRIPS - used_before));
        assert!(subs.used() <= MAX_BAND_SCRIPS);
        // Chart keys are never among what the band sweep releases.
        let held = HashSet::new();
        let later = Instant::now() + BAND_GRACE + Duration::from_secs(1);
        let stale = unwanted_band_keys(&subs.band, &held, later);
        assert!(stale.iter().all(|k| !subs.chart.contains_key(k)));
        assert!(stale.iter().all(|k| k.starts_with("nse_fo|")));
    }

    #[tokio::test]
    async fn a_chart_takes_over_a_strike_a_band_already_streams() {
        let board = ExecuteBoard::new();
        let (ws_tx, mut rx) = bridge();
        let chain: Vec<ChainStrike> = (0..20).map(|i| priced(24000.0 + 50.0 * i as f64, 1000 + 2 * i)).collect();
        let keys = band_keys(&chain, 10, BAND_HALF);
        board.ensure_subscribed(&ws_tx, &keys, SubPool::Owner).await;
        assert_eq!(drain(&mut rx).len(), 2, "the indices and the band");
        let used = board.subs.lock().await.used();

        // The chart wants a strike the band has: it moves to the chart, is
        // sent once more with the ladder flag, and the total is unchanged.
        let shared = keys[0].clone();
        assert!(board.reserve_chart(&ws_tx, &shared, SubPool::Owner).await);
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], serde_json::json!({ "action": "subscribe", "scrips": shared, "ladder": true }));
        let subs = board.subs.lock().await;
        assert_eq!(subs.used(), used);
        assert!(subs.chart.contains_key(&shared) && !subs.band.contains_key(&shared));
        // The band sweep leaves it alone once no ladder wants the band.
        let later = Instant::now() + BAND_GRACE + Duration::from_secs(1);
        let stale = unwanted_band_keys(&subs.band, &HashSet::new(), later);
        assert_eq!(stale.len(), keys.len() - 1);
        assert!(!stale.contains(&shared));
    }

    #[tokio::test]
    async fn a_fresh_bridge_starts_the_budget_over() {
        let board = ExecuteBoard::new();
        let (ws_tx, mut rx) = bridge();
        assert!(board.reserve_chart(&ws_tx, "nse_cm|2885", SubPool::Owner).await);
        assert_eq!(drain(&mut rx).len(), 1);
        // A re-login starts a new bridge with a new channel: nothing it was
        // sent survives, so the key is sent again and counted once.
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        *ws_tx.lock().await = Some(tx2);
        assert!(board.reserve_chart(&ws_tx, "nse_cm|2885", SubPool::Owner).await);
        assert_eq!(drain(&mut rx2).len(), 1);
        assert_eq!(board.subs.lock().await.used(), 1);
        // No bridge at all: nothing can be put on the feed.
        *ws_tx.lock().await = None;
        assert!(!board.reserve_chart(&ws_tx, "nse_cm|11536", SubPool::Owner).await);
    }
}
