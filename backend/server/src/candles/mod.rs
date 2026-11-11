//! The candle service behind `/api/v2/candles`. Analytics only: nothing here
//! touches orders, positions or the Kotak client; it reads the scrip master,
//! and its only message to the bridge is a feed subscription.
//!
//! The first request starts it. For each instrument a chart asks for it keeps
//! today's 1-minute bars, built from live Kotak ticks — or from Upstox's V3
//! feed while Kotak's feed is down — and replaced minute by minute with
//! Upstox's official candles, which carry the volume. Past days come straight
//! from Upstox and are not kept. Once nothing has asked for ten minutes it
//! stops, so with no v2 client open it holds nothing and taps nothing.
//!
//! An index, its futures or its options subscribe to Kotak's feed as they
//! always have. A stock, or a stock's contract (behind `stock_charts`), is
//! put on the feed through the Execute board's ladder budget
//! (`ExecuteBoard::reserve_chart`), so charts and ladders share the one
//! daily allowance and positions' ticks keep their room; past the budget
//! (for users, past their ten chart keys), or when the feed sends no tick
//! within 15 s of subscribing, the chart is `polled`: its bars come from
//! Upstox's official minutes alone. A refused key is asked for again when
//! a caller whose share has room asks for the chart.

pub mod archive;
pub mod bars;
pub mod symbols;

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use shared_domain::{telemetry, DbWriteMessage};
use sqlx::sqlite::SqlitePool;
use tokio::sync::{broadcast, mpsc, watch, Mutex, RwLock};
use tokio::task::JoinHandle;
use trading_engine::ScripStore;
use upstox_client::{Derivative, LtpTick, UpstoxClient};

use bars::{Bar, DaySeries, Tf, MINUTE_MS};
use symbols::Symbol;

use crate::execute::{ExecuteBoard, SubPool};
use crate::instruments::Catalog;
use crate::AppState;

/// A symbol nothing has asked for in this long is dropped; once none is left
/// and no stream is open, the service stops.
const IDLE_MS: i64 = 10 * MINUTE_MS;
/// Kotak's feed counts as down once it has been quiet this long.
const KOTAK_STALE_SECS: f64 = 10.0;
/// Upstox's official minutes are fetched this long after each minute ends.
const OFFICIAL_DELAY_MS: i64 = 8_000;
/// How often the Upstox fallback is reconsidered.
const FALLBACK_CHECK: Duration = Duration::from_secs(5);
const FALLBACK_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Room for a burst of bar changes while a stream catches up.
const CHANGES_CAPACITY: usize = 1024;
/// Falling behind Kotak's ticks is logged at most this often.
const LAG_WARN_EVERY: Duration = Duration::from_secs(60);
/// A Kotak subscription that has not ticked this long after it was sent is
/// taken as refused (the feed only logs a refusal): the chart is `polled`.
const FEED_TICK_WAIT_MS: i64 = 15_000;
/// An Upstox instruments file that could not be had (a failed download, or
/// a file that would not decode) is not asked for again for this long: while
/// Upstox is down, each new contract chart would otherwise wait out the
/// download's timeout again.
const DERIVATIVES_RETRY: Duration = Duration::from_secs(60);

/// Upstox's instruments file for each exchange, and the indices whose
/// derivatives it lists.
pub(crate) const UPSTOX_INDEX_FILES: [(&str, &[&str]); 2] = [
    (upstox_client::NSE_INSTRUMENTS_URL, &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"]),
    (upstox_client::BSE_INSTRUMENTS_URL, &["SENSEX", "BANKEX"]),
];

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

/// Kotak's feed is connected and ticked recently.
fn kotak_live() -> bool {
    let last = telemetry::FEED_LAST_TICK.get();
    telemetry::FEED_CONNECTED.get() == 1 && last > 0.0 && telemetry::unix_now() - last <= KOTAK_STALE_SECS
}

/// A minute of one symbol's bars changed.
#[derive(Debug, Clone)]
pub struct Change {
    pub symbol: Arc<str>,
    pub minute: i64,
}

/// Where a tracked symbol's prices come from.
#[derive(Debug, Clone, Serialize)]
pub struct Tracking {
    pub name: String,
    /// Kotak price-map key (`idx|NIFTY 50`, `nse_fo|51342`, `nse_cm|2885`),
    /// when Kotak's scrip master or the catalog knows the instrument.
    pub kotak: Option<String>,
    /// Upstox instrument key, when Upstox is configured and knows it.
    pub upstox: Option<String>,
    /// No Kotak tick is building the bars: there is no Kotak key, the
    /// ladder budget had no room for it, or the feed sent nothing within
    /// 15 s of subscribing. The bars are Upstox's official minutes alone
    /// (none without Upstox). Read when the tracking is asked for.
    pub polled: bool,
}

#[derive(Debug)]
pub enum TrackError {
    /// Neither broker lists this contract (expired, or no such strike).
    NotFound,
    /// Kotak's ladder budget has no room for it today and Upstox is not
    /// configured: no price source at all.
    NoFeed,
    /// The service stopped while this was being set up; ask again.
    Stopped,
}

/// How a tracked symbol's Kotak subscription is sent.
#[derive(Clone)]
enum KotakSub {
    /// Nothing: Kotak does not know it.
    None,
    /// Sent to the bridge as is — an index, or an index's contract.
    Direct(String),
    /// A stock's key, put on the feed through the ladder budget.
    Budgeted { key: String, pool: SubPool },
}

struct Tracked {
    tracking: Tracking,
    kotak_sub: KotakSub,
    /// The Kotak subscription went out (not refused by the budget).
    kotak_on_feed: bool,
    /// When it last went out, to tell a refused one by its silence.
    subscribed_at_ms: Option<i64>,
    /// A Kotak tick has arrived since.
    kotak_ticked: bool,
    day: DaySeries,
    /// The minute ticks are currently building.
    forming: Option<i64>,
    last_used_ms: i64,
}

impl Tracked {
    fn tracking_at(&self, now: i64) -> Tracking {
        Tracking { polled: self.polled(now), ..self.tracking.clone() }
    }

    fn polled(&self, now: i64) -> bool {
        is_polled(self.tracking.kotak.is_some(), self.kotak_on_feed, self.kotak_ticked, self.subscribed_at_ms, now)
    }

    /// The stock key the ladder budget refused, to ask again with `pool`'s
    /// share: `None` once it is on the feed, for an index's subscription
    /// (never budgeted), and for a visitor, whose share is none.
    fn refused_key(&self, pool: SubPool) -> Option<String> {
        match &self.kotak_sub {
            KotakSub::Budgeted { key, .. } if !self.kotak_on_feed && pool != SubPool::None => Some(key.clone()),
            _ => None,
        }
    }

    /// The budget took `key` on `pool`'s share: the chart waits for its
    /// first tick from `now`, and a fresh bridge asks that share again.
    fn put_on_feed(&mut self, key: String, pool: SubPool, now: i64) {
        self.kotak_sub = KotakSub::Budgeted { key, pool };
        self.kotak_on_feed = true;
        self.subscribed_at_ms = Some(now);
        self.kotak_ticked = false;
    }
}

/// Whether a symbol's bars are Upstox's alone — see [`Tracking::polled`].
fn is_polled(has_kotak: bool, on_feed: bool, ticked: bool, subscribed_at_ms: Option<i64>, now: i64) -> bool {
    if !has_kotak || !on_feed {
        return true;
    }
    if ticked {
        return false;
    }
    subscribed_at_ms.is_none_or(|at| now - at > FEED_TICK_WAIT_MS)
}

struct State {
    day_start: i64,
    tracked: HashMap<String, Tracked>,
    /// Kotak or Upstox feed key → the names tracked on it. Usually one; two
    /// when a future is charted both bare and by its expiry (`NIFTY:FUT`
    /// and `NIFTY:FUT:2026-11-24` while November's is the nearest): each
    /// keeps its own bars from the same ticks.
    by_key: HashMap<String, Vec<String>>,
    /// The bridge channel the Kotak subscriptions went out on. A re-login
    /// starts a fresh bridge that knows none of them.
    kotak_channel: Option<mpsc::UnboundedSender<String>>,
    /// The last Upstox error logged per symbol, so a failing call is logged
    /// once rather than every minute.
    last_error: HashMap<String, String>,
}

pub struct CandleService {
    running: Mutex<Option<Arc<Running>>>,
}

impl Default for CandleService {
    fn default() -> Self {
        Self::new()
    }
}

impl CandleService {
    pub fn new() -> Self {
        Self { running: Mutex::new(None) }
    }

    /// The running service, started by this call if it isn't running.
    pub async fn get(&self, state: &AppState) -> Arc<Running> {
        let mut slot = self.running.lock().await;
        if let Some(running) = slot.as_ref().filter(|r| !r.stopped.load(Ordering::Acquire)) {
            return Arc::clone(running);
        }
        let running = Running::start(Deps {
            upstox: state.upstox.clone(),
            db_tx: state.db_tx.clone(),
            pool: state.db_pool.clone(),
            scrip_store: Arc::clone(&state.scrip_store),
            ws_tx: Arc::clone(&state.ws_tx),
            catalog: Some(Arc::clone(&state.instruments)),
            execute: Some(Arc::clone(&state.execute)),
        })
        .await;
        *slot = Some(Arc::clone(&running));
        running
    }

    /// Track `symbol` on the running service (starting it if need be). A
    /// stock's Kotak subscription draws on `pool`'s share of the ladder
    /// budget.
    pub async fn track(&self, state: &AppState, symbol: Symbol, pool: SubPool) -> Result<(Arc<Running>, Tracking), TrackError> {
        for _ in 0..2 {
            let running = self.get(state).await;
            match running.track(symbol.clone(), pool).await {
                Ok(tracking) => return Ok((running, tracking)),
                Err(TrackError::Stopped) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(TrackError::Stopped)
    }
}

/// What the service needs from the rest of the server.
struct Deps {
    upstox: Option<UpstoxClient>,
    db_tx: mpsc::Sender<DbWriteMessage>,
    pool: SqlitePool,
    scrip_store: Arc<RwLock<Option<ScripStore>>>,
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    /// The instrument catalog: a stock's feed key and Upstox key. Without
    /// it (the tests), no stock can be tracked.
    catalog: Option<Arc<Catalog>>,
    /// The Execute board, whose ladder budget a stock's subscription is
    /// counted in. Without it, a stock gets no Kotak subscription.
    execute: Option<Arc<ExecuteBoard>>,
}

pub struct Running {
    upstox: Option<UpstoxClient>,
    db_tx: mpsc::Sender<DbWriteMessage>,
    pool: SqlitePool,
    scrip_store: Arc<RwLock<Option<ScripStore>>>,
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    catalog: Option<Arc<Catalog>>,
    execute: Option<Arc<ExecuteBoard>>,
    state: StdMutex<State>,
    /// Upstox's futures and options by underlying, each list loaded once
    /// per instruments day on the first contract asked for: the six indices
    /// together from both exchanges' files, a stock on its own from NSE's
    /// (the day's file is cached on disk by the client, so a second stock
    /// filters the file again without downloading it).
    derivatives: DerivativeLists,
    changes: broadcast::Sender<Change>,
    /// The Upstox keys the fallback feed should stream.
    upstox_keys: watch::Sender<BTreeSet<String>>,
    open_streams: AtomicUsize,
    last_used_ms: AtomicI64,
    stopped: AtomicBool,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl Running {
    async fn start(deps: Deps) -> Arc<Running> {
        let now = now_ms();
        crate::db::prune_candles(&deps.pool, bars::ist_day_start(now)).await;
        let running = Arc::new(Running {
            upstox: deps.upstox,
            db_tx: deps.db_tx,
            pool: deps.pool,
            scrip_store: deps.scrip_store,
            ws_tx: deps.ws_tx,
            catalog: deps.catalog,
            execute: deps.execute,
            state: StdMutex::new(State {
                day_start: bars::ist_day_start(now),
                tracked: HashMap::new(),
                by_key: HashMap::new(),
                kotak_channel: None,
                last_error: HashMap::new(),
            }),
            derivatives: DerivativeLists::default(),
            changes: broadcast::channel(CHANGES_CAPACITY).0,
            upstox_keys: watch::channel(BTreeSet::new()).0,
            open_streams: AtomicUsize::new(0),
            last_used_ms: AtomicI64::new(now),
            stopped: AtomicBool::new(false),
            tasks: StdMutex::new(Vec::new()),
        });
        let tasks = vec![
            tokio::spawn(Arc::clone(&running).kotak_ticks()),
            tokio::spawn(Arc::clone(&running).upstox_fallback()),
            tokio::spawn(Arc::clone(&running).every_minute()),
        ];
        *running.tasks.lock().unwrap_or_else(|e| e.into_inner()) = tasks;
        tracing::info!(upstox = running.upstox.is_some(), "Candle service started");
        running
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start keeping `symbol`'s bars, or note that it was asked for again.
    /// A stock's Kotak subscription is paid for out of `pool`'s share of the
    /// ladder budget; an index's is sent as it always was.
    pub async fn track(self: &Arc<Self>, symbol: Symbol, pool: SubPool) -> Result<Tracking, TrackError> {
        let name = symbol.name();
        let now = now_ms();
        self.last_used_ms.store(now, Ordering::Release);
        let refused_key = {
            let mut st = self.state();
            if self.stopped.load(Ordering::Acquire) {
                return Err(TrackError::Stopped);
            }
            match st.tracked.get_mut(&name) {
                Some(t) => {
                    t.last_used_ms = now;
                    match t.refused_key(pool) {
                        Some(key) => Some(key),
                        None => return Ok(t.tracking_at(now)),
                    }
                }
                None => None,
            }
        };
        // Tracked already, but the budget refused its stock key to whoever
        // asked first: this caller's share may have room theirs lacked (the
        // admin opening a chart a full users' share left `polled`).
        if let Some(key) = refused_key {
            let on_feed = self.reserve_chart(&key, pool).await;
            let now = now_ms();
            let mut st = self.state();
            if let Some(t) = st.tracked.get_mut(&name) {
                if on_feed {
                    t.put_on_feed(key, pool, now);
                }
                return Ok(t.tracking_at(now));
            }
            // Dropped meanwhile (a new day): tracked afresh below.
        }

        let today = shared_domain::today_ist();
        if symbol.refusal(today).is_some() {
            return Err(TrackError::NotFound);
        }
        // A stock's cash row: its feed key, and Upstox's key by ISIN.
        let equity_key = match &symbol {
            Symbol::Equity { symbol, .. } => self.catalog.as_ref().and_then(|c| c.spot_key(symbol)),
            _ => None,
        };
        let upstox = match &symbol {
            _ if self.upstox.is_none() => None,
            Symbol::Index(index) => symbols::upstox_index_key(index).map(str::to_string),
            Symbol::Equity { .. } => equity_key.as_deref().and_then(|key| self.catalog.as_ref()?.upstox_key(key)),
            _ => match self.derivatives_for(symbol.underlying()).await {
                Some(list) => symbols::find_upstox(&symbol, &list, today).map(|d| d.instrument_key.clone()),
                None => None,
            },
        };
        let (kotak_key, kotak_sub) = match &symbol {
            Symbol::Index(index) => match shared_domain::kotak_index_feed(index) {
                Some((segment, feed_name)) => {
                    let msg = serde_json::json!({ "action": "subscribe_indices", "indices": format!("{segment}|{feed_name}") });
                    (Some(shared_domain::index_price_key(feed_name)), KotakSub::Direct(msg.to_string()))
                }
                None => (None, KotakSub::None),
            },
            Symbol::Equity { .. } => match equity_key {
                Some(key) => (Some(key.clone()), KotakSub::Budgeted { key, pool }),
                None => (None, KotakSub::None),
            },
            _ => {
                let store = self.scrip_store.read().await;
                match store.as_ref().and_then(|s| symbols::find_kotak(&symbol, s, today)) {
                    Some(record) => {
                        let key = record.ws_key();
                        if symbol.is_index() {
                            // `ladder`: the bridge drops these at the day's close.
                            let msg = serde_json::json!({ "action": "subscribe", "scrips": key, "ladder": true });
                            (Some(key), KotakSub::Direct(msg.to_string()))
                        } else {
                            (Some(key.clone()), KotakSub::Budgeted { key, pool })
                        }
                    }
                    None => (None, KotakSub::None),
                }
            }
        };
        if upstox.is_none() && kotak_key.is_none() {
            return Err(TrackError::NotFound);
        }
        // A stock's key goes on the feed only with room in the budget. Asked
        // before anything is kept, so a refusal with no Upstox leaves
        // nothing behind: no price source at all is an error, not a chart.
        let kotak_on_feed = match &kotak_sub {
            KotakSub::None => false,
            KotakSub::Direct(_) => true,
            KotakSub::Budgeted { key, pool } => self.reserve_chart(key, *pool).await,
        };
        if !kotak_on_feed && upstox.is_none() {
            return Err(TrackError::NoFeed);
        }
        let tracking = Tracking { name: name.clone(), kotak: kotak_key, upstox, polled: !kotak_on_feed };

        let mut day = DaySeries::default();
        for (t, o, h, l, c, v, source) in crate::db::load_candle_minutes(&self.pool, &name, bars::ist_day_start(now)).await {
            day.restore(Bar { t, o, h, l, c, v }, source == "upstox");
        }
        {
            let mut st = self.state();
            if self.stopped.load(Ordering::Acquire) {
                return Err(TrackError::Stopped);
            }
            for key in tracking.kotak.iter().chain(tracking.upstox.iter()) {
                let names = st.by_key.entry(key.clone()).or_default();
                if !names.contains(&name) {
                    names.push(name.clone());
                }
            }
            st.tracked.entry(name.clone()).or_insert(Tracked {
                tracking: tracking.clone(),
                kotak_sub: kotak_sub.clone(),
                kotak_on_feed,
                subscribed_at_ms: kotak_on_feed.then_some(now),
                kotak_ticked: false,
                day,
                forming: None,
                last_used_ms: now,
            });
        }
        if let KotakSub::Direct(msg) = &kotak_sub {
            self.subscribe_kotak(msg).await;
        }
        if let Some(key) = &tracking.upstox {
            self.upstox_keys.send_modify(|keys| {
                keys.insert(key.clone());
            });
            self.fetch_official(&name, key).await;
        }
        tracing::info!(symbol = %name, kotak = ?tracking.kotak, upstox = ?tracking.upstox, polled = tracking.polled, "Candle service tracking");
        Ok(tracking)
    }

    /// Put a stock's `key` on the feed through the ladder budget.
    async fn reserve_chart(&self, key: &str, pool: SubPool) -> bool {
        match &self.execute {
            Some(board) => board.reserve_chart(&self.ws_tx, key, pool).await,
            None => false,
        }
    }

    /// Where a tracked symbol's prices come from now, with `polled` as of
    /// now; `None` once it has lapsed.
    #[cfg(test)]
    pub fn tracking(&self, name: &str) -> Option<Tracking> {
        let now = now_ms();
        self.state().tracked.get(name).map(|t| t.tracking_at(now))
    }

    /// Mark `names` as still wanted; returns the ones no longer tracked
    /// (dropped while idle, or reset by a new day), to track again.
    pub fn touch(&self, names: &[String]) -> Vec<String> {
        let now = now_ms();
        self.last_used_ms.store(now, Ordering::Release);
        let mut st = self.state();
        names
            .iter()
            .filter(|name| match st.tracked.get_mut(name.as_str()) {
                Some(t) => {
                    t.last_used_ms = now;
                    false
                }
                None => true,
            })
            .cloned()
            .collect()
    }

    /// Today's bars for a tracked symbol at `tf`, oldest first.
    pub fn today(&self, name: &str, tf: Tf) -> Vec<Bar> {
        self.state().tracked.get(name).map(|t| t.day.bars(tf)).unwrap_or_default()
    }

    /// The `tf` bar of a tracked symbol containing `t`.
    pub fn bar_at(&self, name: &str, tf: Tf, t: i64) -> Option<Bar> {
        self.state().tracked.get(name)?.day.bar_at(tf, t)
    }

    /// Past days' bars from Upstox, oldest first. Not kept.
    pub async fn history(&self, upstox_key: Option<&str>, tf: Tf, from: NaiveDate, to: NaiveDate) -> Result<Vec<Bar>, String> {
        let client = self.upstox.as_ref().ok_or("Upstox is not configured (UPSTOX_ANALYTICS_TOKEN)")?;
        let key = upstox_key.ok_or("Upstox does not list this contract")?;
        let candles = client.historical_candles(key, tf.minutes(), from, to).await.map_err(|e| e.to_string())?;
        Ok(candles
            .into_iter()
            .filter(|c| bars::in_session(c.start_ms))
            .map(|c| Bar { t: c.start_ms, o: c.open, h: c.high, l: c.low, c: c.close, v: Some(c.volume) })
            .collect())
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    /// Count an open stream until the guard drops.
    pub fn open_stream(self: &Arc<Self>) -> StreamGuard {
        self.open_streams.fetch_add(1, Ordering::AcqRel);
        StreamGuard { running: Arc::clone(self) }
    }

    /// Fold one price into every tracked symbol `key` belongs to.
    fn apply_tick(&self, key: &str, at_ms: i64, price: f64) {
        let mut st = self.state();
        let Some(names) = st.by_key.get(key).cloned() else { return };
        let mut changed: Vec<(String, i64)> = Vec::with_capacity(names.len());
        for name in names {
            let Some(t) = st.tracked.get_mut(&name) else { continue };
            if t.tracking.kotak.as_deref() == Some(key) {
                t.kotak_ticked = true;
            }
            let Some(minute) = t.day.tick(at_ms, price) else { continue };
            // The minute before just closed: cache it unless Upstox already has it.
            let closed = t.forming.filter(|m| *m != minute).filter(|m| !t.day.has_official(*m));
            t.forming = Some(minute);
            if let Some(bar) = closed.and_then(|m| t.day.ticked_minute(m)) {
                let _ = self.db_tx.try_send(candle_minute(&name, bar, "ticks"));
            }
            changed.push((name, minute));
        }
        drop(st);
        for (name, minute) in changed {
            let _ = self.changes.send(Change { symbol: name.into(), minute });
        }
    }

    /// Ask the bridge to stream one symbol. Which channel already has every
    /// subscription is `resubscribe_kotak`'s to track, not this one's.
    async fn subscribe_kotak(&self, msg: &str) {
        if let Some(tx) = self.ws_tx.lock().await.clone() {
            let _ = tx.send(msg.to_string());
        }
    }

    /// A bridge this service hasn't sent everything to yet — the first one,
    /// or a fresh one after a Kotak re-login — gets every subscription: an
    /// index's as it was sent, a stock's through the ladder budget again
    /// (the board starts a fresh bridge's budget over too).
    async fn resubscribe_kotak(&self) {
        let Some(tx) = self.ws_tx.lock().await.clone() else { return };
        let subs: Vec<(String, KotakSub)> = {
            let mut st = self.state();
            if st.kotak_channel.as_ref().is_some_and(|c| c.same_channel(&tx)) {
                return;
            }
            st.kotak_channel = Some(tx.clone());
            st.tracked.iter().map(|(name, t)| (name.clone(), t.kotak_sub.clone())).collect()
        };
        for (name, sub) in subs {
            match sub {
                KotakSub::None => {}
                KotakSub::Direct(msg) => {
                    let _ = tx.send(msg);
                }
                KotakSub::Budgeted { key, pool } => {
                    let on_feed = self.reserve_chart(&key, pool).await;
                    let now = now_ms();
                    if let Some(t) = self.state().tracked.get_mut(&name) {
                        t.kotak_on_feed = on_feed;
                        t.subscribed_at_ms = on_feed.then_some(now);
                        t.kotak_ticked = false;
                    }
                }
            }
        }
    }

    /// Upstox's futures and options on `underlying`, for the instruments
    /// day (`upstox_client::instruments_day`: it starts at 07:00 IST, once
    /// Upstox has published the day's file, so a list read before that is
    /// not kept past it) — see [`DerivativeLists::get_or_load`].
    async fn derivatives_for(&self, underlying: &str) -> Option<Arc<Vec<Derivative>>> {
        let client = self.upstox.as_ref()?;
        let day = upstox_client::instruments_day(shared_domain::now_ist());
        self.derivatives.get_or_load(underlying, day, || load_derivatives(client, underlying)).await
    }

    /// Pull Upstox's official minutes for one symbol and keep the finished ones.
    async fn fetch_official(&self, name: &str, key: &str) {
        let Some(client) = &self.upstox else { return };
        let result = client.intraday_candles(key, 1).await;
        let changed = {
            let mut st = self.state();
            let error = match &result {
                Ok(_) => None,
                Err(e) => Some(e.to_string()),
            };
            let report = match &error {
                Some(msg) => st.last_error.insert(name.to_string(), msg.clone()).as_ref() != Some(msg),
                None => st.last_error.remove(name).is_some(),
            };
            if report {
                match &error {
                    Some(msg) => tracing::warn!(symbol = name, error = %msg, "Upstox intraday candles failed"),
                    None => tracing::info!(symbol = name, "Upstox intraday candles recovered"),
                }
            }
            let Ok(candles) = result else { return };
            let minutes = candles
                .into_iter()
                .map(|c| Bar { t: c.start_ms, o: c.open, h: c.high, l: c.low, c: c.close, v: Some(c.volume) })
                .collect();
            let Some(t) = st.tracked.get_mut(name) else { return };
            t.day.add_official(bars::finished_minutes(minutes, now_ms()))
        };
        let symbol: Arc<str> = name.into();
        for bar in changed {
            let _ = self.db_tx.send(candle_minute(name, bar, "upstox")).await;
            let _ = self.changes.send(Change { symbol: Arc::clone(&symbol), minute: bar.t });
        }
    }

    /// Every Kotak tick, for the tracked symbols. Kotak is the primary source.
    async fn kotak_ticks(self: Arc<Self>) {
        let mut rx = kotak_client::subscribe_ticks();
        // Ticks skipped since the last warning, which goes out at most once
        // a minute however long the lag lasts.
        let mut missed = 0u64;
        let mut warned_at: Option<std::time::Instant> = None;
        loop {
            match rx.recv().await {
                Ok(t) => self.apply_tick(&t.key, t.at_ms, t.ltp),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    missed += n;
                    if warned_at.is_none_or(|at| at.elapsed() >= LAG_WARN_EVERY) {
                        tracing::warn!(missed, "Candle service fell behind Kotak's ticks — some ticks skipped");
                        missed = 0;
                        warned_at = Some(std::time::Instant::now());
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    /// While Kotak's feed is down during the session, stream the tracked
    /// symbols from Upstox's V3 feed instead; stop as soon as Kotak is back.
    async fn upstox_fallback(self: Arc<Self>) {
        let Some(client) = self.upstox.clone() else { return };
        let (tick_tx, mut tick_rx) = mpsc::channel::<LtpTick>(1024);
        let mut feed: Option<JoinHandle<()>> = None;
        let mut check = tokio::time::interval(FALLBACK_CHECK);
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = check.tick() => {
                    let now = shared_domain::now_ist();
                    let weekday = !matches!(now.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun);
                    let want = ((weekday && bars::in_session(now.timestamp_millis())) || shared_domain::in_special_session_at(now))
                        && !kotak_live()
                        && !self.upstox_keys.borrow().is_empty();
                    let running = feed.as_ref().is_some_and(|f| !f.is_finished());
                    if want && !running {
                        tracing::warn!("Kotak's feed is down — charts follow Upstox's live feed until it is back");
                        feed = Some(tokio::spawn(run_upstox_feed(client.clone(), self.upstox_keys.subscribe(), tick_tx.clone())));
                    } else if !want && running {
                        tracing::info!("Charts back on Kotak's feed — Upstox live feed stopped");
                        if let Some(f) = feed.take() {
                            f.abort();
                        }
                    }
                }
                Some(t) = tick_rx.recv() => {
                    // Never both at once: Kotak wins the moment it is back.
                    if !kotak_live() {
                        self.apply_tick(&t.key, now_ms(), t.ltp);
                    }
                }
            }
        }
    }

    /// Once a minute, just after it ends: official minutes, re-subscription
    /// after a re-login, the day's rollover, and stopping when idle.
    async fn every_minute(self: Arc<Self>) {
        loop {
            let now = now_ms();
            let mut next = Tf::M1.bucket(now) + OFFICIAL_DELAY_MS;
            if next <= now {
                next += MINUTE_MS;
            }
            tokio::time::sleep(Duration::from_millis((next - now) as u64)).await;

            let now = now_ms();
            if self.sweep(now) {
                return;
            }
            self.resubscribe_kotak().await;

            // Official minutes exist from 09:16, and the last one settles a
            // couple of minutes after the 15:30 close.
            let open = bars::session_close(now) - (6 * 60 + 15) * MINUTE_MS;
            if now < open + MINUTE_MS || now > bars::session_close(now) + 5 * MINUTE_MS {
                continue;
            }
            let targets: Vec<(String, String)> = {
                let st = self.state();
                st.tracked
                    .iter()
                    .filter_map(|(name, t)| Some((name.clone(), t.tracking.upstox.clone()?)))
                    .collect()
            };
            for (name, key) in targets {
                self.fetch_official(&name, &key).await;
            }
        }
    }

    /// Drop symbols nothing has asked for lately, reset on a new day, and stop
    /// the whole service once idle. Returns whether it stopped.
    fn sweep(&self, now: i64) -> bool {
        let streams = self.open_streams.load(Ordering::Acquire);
        let mut st = self.state();
        let day_start = bars::ist_day_start(now);
        if st.day_start != day_start {
            st.day_start = day_start;
            st.tracked.clear();
            st.by_key.clear();
            tokio::spawn(crate::db::prune_candles_owned(self.pool.clone(), day_start));
            tracing::info!("Candle service: new day — today's bars start empty");
        }
        let idle: Vec<String> = st
            .tracked
            .iter()
            .filter(|(_, t)| now - t.last_used_ms > IDLE_MS)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &idle {
            if let Some(t) = st.tracked.remove(name) {
                for key in t.tracking.kotak.iter().chain(t.tracking.upstox.iter()) {
                    // Another name on the same contract keeps its ticks.
                    if let Some(names) = st.by_key.get_mut(key) {
                        names.retain(|n| n != name);
                        if names.is_empty() {
                            st.by_key.remove(key);
                        }
                    }
                }
                st.last_error.remove(name);
            }
        }
        let keys: BTreeSet<String> = st.tracked.values().filter_map(|t| t.tracking.upstox.clone()).collect();
        self.upstox_keys.send_if_modified(|current| {
            let changed = *current != keys;
            *current = keys;
            changed
        });

        let quiet = now - self.last_used_ms.load(Ordering::Acquire) > IDLE_MS;
        if !(st.tracked.is_empty() && streams == 0 && quiet) {
            return false;
        }
        self.stopped.store(true, Ordering::Release);
        st.by_key.clear();
        drop(st);
        // The contract lists are the biggest thing held; nothing asks for
        // them while stopped (a load still under way goes with the service).
        self.derivatives.clear();
        tracing::info!("Candle service stopped — no chart has asked for anything in 10 minutes");
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for task in tasks {
            // This sweep runs inside one of them; aborting itself is harmless.
            task.abort();
        }
        true
    }
}

/// Upstox's contract lists, by underlying, for one instruments day each.
///
/// A list already in memory is answered from a lock nothing holds across a
/// download or a decompress, so an index chart never waits on a stock's
/// load. Loads run one at a time: each is a download (a 180 s timeout) or a
/// stream-decompress of the cached file, seconds of the VM's two cores, and
/// a caller waiting on the same list finds it loaded when its turn comes
/// instead of loading it again. A file that could not be had is not asked
/// for again for [`DERIVATIVES_RETRY`]: every list that needs it is then
/// answered at once with nothing, rather than queueing behind one more
/// download that times out.
#[derive(Default)]
struct DerivativeLists {
    state: StdMutex<DerivativeState>,
    loading: Mutex<()>,
}

#[derive(Default)]
struct DerivativeState {
    /// Underlying → the instruments day its list was read for, and the list.
    lists: HashMap<String, (NaiveDate, Arc<Vec<Derivative>>)>,
    /// Instruments file (its URL) → when it last could not be had.
    failed: HashMap<&'static str, Instant>,
}

/// What the lists hold for one underlying.
enum Cached {
    Hit(Arc<Vec<Derivative>>),
    /// A file its list is read from failed under [`DERIVATIVES_RETRY`] ago.
    Failed,
    Missing,
}

/// The instruments files `underlying`'s list is read from: both
/// exchanges' for the six indices (they load together), NSE's for a stock.
fn list_files(underlying: &str) -> &'static [&'static str] {
    const INDEX_FILES: [&str; 2] = [UPSTOX_INDEX_FILES[0].0, UPSTOX_INDEX_FILES[1].0];
    const STOCK_FILES: [&str; 1] = [upstox_client::NSE_INSTRUMENTS_URL];
    if shared_domain::INDEX_NAMES.contains(&underlying) { &INDEX_FILES } else { &STOCK_FILES }
}

impl DerivativeState {
    fn cached(&self, underlying: &str, day: NaiveDate, now: Instant) -> Cached {
        if let Some((d, list)) = self.lists.get(underlying) {
            if *d == day {
                return Cached::Hit(Arc::clone(list));
            }
        }
        let failed = list_files(underlying)
            .iter()
            .any(|file| self.failed.get(file).is_some_and(|at| now.saturating_duration_since(*at) < DERIVATIVES_RETRY));
        if failed { Cached::Failed } else { Cached::Missing }
    }
}

impl DerivativeLists {
    fn state(&self) -> MutexGuard<'_, DerivativeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `underlying`'s list for the instruments `day`: from memory when it is
    /// there, else `load` brings it (every underlying that load lists), or
    /// names the file it could not have (logged by `load`). Meanwhile, and
    /// for [`DERIVATIVES_RETRY`] after such a failure, the chart has no
    /// Upstox data.
    async fn get_or_load<F, Fut>(&self, underlying: &str, day: NaiveDate, load: F) -> Option<Arc<Vec<Derivative>>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<(String, Vec<Derivative>)>, &'static str>>,
    {
        let cached = self.state().cached(underlying, day, Instant::now());
        match cached {
            Cached::Hit(list) => return Some(list),
            Cached::Failed => return None,
            Cached::Missing => {}
        }
        let _one_at_a_time = self.loading.lock().await;
        // Loaded, or failed, by the caller this one waited behind.
        let cached = self.state().cached(underlying, day, Instant::now());
        match cached {
            Cached::Hit(list) => return Some(list),
            Cached::Failed => return None,
            Cached::Missing => {}
        }
        let loaded = load().await;
        let mut st = self.state();
        // An older day's lists are never read again.
        st.lists.retain(|_, (d, _)| *d == day);
        match loaded {
            Ok(lists) => {
                for file in list_files(underlying) {
                    st.failed.remove(file);
                }
                for (u, list) in lists {
                    st.lists.insert(u, (day, Arc::new(list)));
                }
                st.lists.get(underlying).map(|(_, list)| Arc::clone(list))
            }
            Err(file) => {
                st.failed.insert(file, Instant::now());
                None
            }
        }
    }

    fn clear(&self) {
        *self.state() = DerivativeState::default();
    }
}

/// Every list `underlying`'s load brings from Upstox: the six indices'
/// (both exchanges' files), or one stock's from NSE's file. `Err` names the
/// file that could not be had, logged here.
async fn load_derivatives(client: &UpstoxClient, underlying: &str) -> Result<Vec<(String, Vec<Derivative>)>, &'static str> {
    if shared_domain::INDEX_NAMES.contains(&underlying) {
        let mut all: Vec<Derivative> = Vec::new();
        for (url, wanted) in UPSTOX_INDEX_FILES {
            match client.derivatives(url, wanted).await {
                Ok(rows) => all.extend(rows),
                Err(e) => {
                    tracing::warn!(error = %e, url, "Upstox instruments download failed — futures and options charts have no Upstox data for a minute, then it is tried again");
                    return Err(url);
                }
            }
        }
        tracing::info!(contracts = all.len(), "Upstox index derivatives loaded for the charts");
        Ok(shared_domain::INDEX_NAMES
            .iter()
            .map(|index| (index.to_string(), all.iter().filter(|d| d.underlying == *index).cloned().collect()))
            .collect())
    } else {
        // NSE only: a stock's contracts trade there (question 3).
        let url = upstox_client::NSE_INSTRUMENTS_URL;
        match client.derivatives(url, &[underlying]).await {
            Ok(rows) => {
                tracing::info!(underlying, contracts = rows.len(), "Upstox derivatives loaded for a stock's charts");
                Ok(vec![(underlying.to_string(), rows)])
            }
            Err(e) => {
                tracing::warn!(error = %e, underlying, "Upstox instruments download failed — stocks' futures and options charts have no Upstox data for a minute, then it is tried again");
                Err(url)
            }
        }
    }
}

/// Keeps one stream counted as open.
pub struct StreamGuard {
    running: Arc<Running>,
}

impl StreamGuard {
    pub fn running(&self) -> &Arc<Running> {
        &self.running
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.running.open_streams.fetch_sub(1, Ordering::AcqRel);
        self.running.last_used_ms.store(now_ms(), Ordering::Release);
    }
}

/// The Upstox V3 feed for `keys`, reconnecting with backoff until aborted or
/// `out` closes.
async fn run_upstox_feed(client: UpstoxClient, keys: watch::Receiver<BTreeSet<String>>, out: mpsc::Sender<LtpTick>) {
    let mut backoff = FALLBACK_CHECK;
    loop {
        match client.stream_ltps(keys.clone(), out.clone()).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(error = %e, retry_in_secs = backoff.as_secs(), "Upstox live feed dropped");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(FALLBACK_BACKOFF_MAX);
            }
        }
    }
}

fn candle_minute(symbol: &str, bar: Bar, source: &'static str) -> DbWriteMessage {
    DbWriteMessage::CandleMinute {
        symbol: symbol.to_string(),
        t: bar.t,
        open: bar.o,
        high: bar.h,
        low: bar.l,
        close: bar.c,
        volume: bar.v,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway SQLite file, removed (with its WAL files) when dropped.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("auto-trader-candles-{name}-{}.db", std::process::id()));
            let db = TempDb(path);
            db.remove();
            db
        }

        fn remove(&self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            self.remove();
        }
    }

    /// A service with no Upstox client and no Kotak bridge, on its own DB.
    async fn service(name: &str) -> (Arc<Running>, TempDb, mpsc::Receiver<DbWriteMessage>) {
        service_with(name, None, None, None).await
    }

    /// A service with no Upstox client, on its own DB, with this scrip
    /// master, bridge channel and Execute board.
    async fn service_with(
        name: &str,
        store: Option<ScripStore>,
        bridge: Option<mpsc::UnboundedSender<String>>,
        execute: Option<Arc<ExecuteBoard>>,
    ) -> (Arc<Running>, TempDb, mpsc::Receiver<DbWriteMessage>) {
        let db = TempDb::new(name);
        let pool = crate::db::init_db(&format!("sqlite://{}", db.0.display())).await;
        let (db_tx, db_rx) = mpsc::channel(64);
        let running = Running::start(Deps {
            upstox: None,
            db_tx,
            pool,
            scrip_store: Arc::new(RwLock::new(store)),
            ws_tx: Arc::new(Mutex::new(bridge)),
            catalog: None,
            execute,
        })
        .await;
        (running, db, db_rx)
    }

    fn nifty() -> Symbol {
        Symbol::Index("NIFTY".into())
    }

    /// Unix ms of `h:m` IST today.
    fn today_at(h: i64, m: i64) -> i64 {
        bars::ist_day_start(now_ms()) + (h * 60 + m) * MINUTE_MS
    }

    #[tokio::test]
    async fn ticks_for_a_tracked_symbol_build_its_bars() {
        let (running, _db, mut db_rx) = service("ticks").await;
        let tracking = running.track(nifty(), SubPool::Owner).await.unwrap();
        assert_eq!(tracking.kotak.as_deref(), Some("idx|NIFTY 50"));
        assert!(!tracking.polled, "an index subscription just sent counts as the feed's");
        let mut changes = running.subscribe_changes();

        let t = today_at(9, 20);
        running.apply_tick("idx|NIFTY 50", t + 1_000, 22_800.0);
        assert!(!running.tracking("NIFTY").unwrap().polled);
        running.apply_tick("idx|NIFTY 50", t + 30_000, 22_810.0);
        // Ticks for anything else are not this symbol's.
        running.apply_tick("nse_fo|999", t + 30_000, 1.0);
        let bars = running.today("NIFTY", Tf::M5);
        assert_eq!(bars.len(), 1);
        assert_eq!((bars[0].t, bars[0].o, bars[0].h, bars[0].c, bars[0].v), (t, 22_800.0, 22_810.0, 22_810.0, None));
        let change = changes.try_recv().unwrap();
        assert_eq!((&*change.symbol, change.minute), ("NIFTY", t));

        // The next minute's first tick closes 09:20, which is cached as ticked.
        running.apply_tick("idx|NIFTY 50", t + MINUTE_MS + 5_000, 22_805.0);
        match db_rx.try_recv() {
            Ok(DbWriteMessage::CandleMinute { symbol, t: minute, close, source, .. }) => {
                assert_eq!((symbol.as_str(), minute, close, source), ("NIFTY", t, 22_810.0, "ticks"));
            }
            other => panic!("expected the closed minute to be cached, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_service_stops_once_nothing_has_asked_for_ten_minutes() {
        let (running, _db, _db_rx) = service("idle").await;
        running.track(nifty(), SubPool::Owner).await.unwrap();
        let later = |ms: i64| now_ms() + ms;

        assert!(!running.sweep(later(MINUTE_MS)), "just asked for");
        // A chart left open keeps the service up, even once its symbol lapses.
        let stream = running.open_stream();
        assert!(!running.sweep(later(IDLE_MS + MINUTE_MS)));
        assert_eq!(running.touch(&["NIFTY".to_string()]), ["NIFTY"], "the idle symbol was dropped");
        drop(stream);
        assert!(!running.sweep(later(MINUTE_MS)), "the stream only just closed");
        assert!(running.sweep(later(IDLE_MS + MINUTE_MS)));
        assert!(matches!(running.track(nifty(), SubPool::Owner).await, Err(TrackError::Stopped)));
    }

    #[tokio::test]
    async fn a_stock_needs_the_catalog_and_a_bse_listing_is_refused() {
        let (running, _db, _db_rx) = service("stock").await;
        let nse = Symbol::Equity { symbol: "RELIANCE".into(), exchange: symbols::Exchange::Nse };
        let bse = Symbol::Equity { symbol: "RELIANCE".into(), exchange: symbols::Exchange::Bse };
        // No catalog here: neither broker's key is known.
        assert!(matches!(running.track(nse, SubPool::Owner).await, Err(TrackError::NotFound)));
        assert!(matches!(running.track(bse, SubPool::Owner).await, Err(TrackError::NotFound)));
        let future = Symbol::Future { underlying: "RELIANCE".into(), expiry: None };
        assert!(matches!(running.track(future, SubPool::Owner).await, Err(TrackError::NotFound)));
        assert!(running.tracking("RELIANCE").is_none());
    }

    #[test]
    fn a_chart_is_polled_without_a_kotak_tick_within_fifteen_seconds() {
        let at = 1_000_000;
        // No Kotak key, or no room in the budget: polled from the start.
        assert!(is_polled(false, false, false, None, at));
        assert!(is_polled(true, false, false, None, at));
        // Subscribed and waiting: the feed's for 15 s, then polled.
        assert!(!is_polled(true, true, false, Some(at), at + 1_000));
        assert!(!is_polled(true, true, false, Some(at), at + FEED_TICK_WAIT_MS));
        assert!(is_polled(true, true, false, Some(at), at + FEED_TICK_WAIT_MS + 1));
        // A tick arrived: the feed's, however long ago it subscribed.
        assert!(!is_polled(true, true, true, Some(at), at + 10 * FEED_TICK_WAIT_MS));
        // On the feed but never stamped: nothing to wait on.
        assert!(is_polled(true, true, false, None, at));
    }

    fn future_record(name: &str, token: &str, expiry: NaiveDate) -> trading_engine::ScripRecord {
        trading_engine::ScripRecord {
            instrument_token: token.into(),
            trading_symbol: format!("{name}{}FUT", expiry.format("%y%b").to_string().to_uppercase()),
            symbol_name: name.into(),
            exchange_segment_code: "nse_fo".into(),
            strike_price: 0.0,
            option_type: "XX".into(),
            expiry_date: expiry,
            lot_size: 65,
            tick_size: 0.05,
        }
    }

    #[tokio::test]
    async fn a_future_charted_bare_and_by_its_expiry_keeps_both_charts_ticking() {
        let today = shared_domain::today_ist();
        let (near, next) = (today + chrono::Days::new(10), today + chrono::Days::new(40));
        let yesterday = today - chrono::Days::new(1);
        let mut store = ScripStore::default();
        store.records.insert("NIFTY".into(), vec![
            future_record("NIFTY", "48700", yesterday),
            future_record("NIFTY", "48704", near),
            future_record("NIFTY", "48705", next),
        ]);
        let (running, _db, _db_rx) = service_with("dated", Some(store), None, None).await;
        let bare = Symbol::Future { underlying: "NIFTY".into(), expiry: None };
        let dated = Symbol::Future { underlying: "NIFTY".into(), expiry: Some(near) };
        let later = Symbol::Future { underlying: "NIFTY".into(), expiry: Some(next) };

        // The bare form is the nearest; the dated one names that contract,
        // and the later month is its own, never the nearest.
        let a = running.track(bare.clone(), SubPool::Owner).await.unwrap();
        let b = running.track(dated.clone(), SubPool::Owner).await.unwrap();
        let c = running.track(later.clone(), SubPool::Owner).await.unwrap();
        assert_eq!((a.name.as_str(), a.kotak.as_deref()), ("NIFTY:FUT", Some("nse_fo|48704")));
        assert_eq!(b.name, symbols::dated_future_name("NIFTY", near));
        assert_eq!(b.kotak.as_deref(), Some("nse_fo|48704"));
        assert_eq!(c.kotak.as_deref(), Some("nse_fo|48705"));
        // A date nothing expires on is not a contract (a 404); an expired
        // future is refused, though the scrip master still lists it.
        let nothing = Symbol::Future { underlying: "NIFTY".into(), expiry: Some(today + chrono::Days::new(11)) };
        assert!(matches!(running.track(nothing, SubPool::Owner).await, Err(TrackError::NotFound)));
        let expired = Symbol::Future { underlying: "NIFTY".into(), expiry: Some(yesterday) };
        assert!(matches!(running.track(expired, SubPool::Owner).await, Err(TrackError::NotFound)));

        // One tick of the shared contract builds both charts' bars.
        let t = today_at(9, 20);
        let mut changes = running.subscribe_changes();
        running.apply_tick("nse_fo|48704", t + 1_000, 24_100.0);
        assert_eq!(running.today("NIFTY:FUT", Tf::M1).len(), 1);
        assert_eq!(running.today(&b.name, Tf::M1).len(), 1);
        assert!(running.today(&c.name, Tf::M1).is_empty(), "another month's ticks are its own");
        let mut changed: Vec<String> = std::iter::from_fn(|| changes.try_recv().ok()).map(|c| c.symbol.to_string()).collect();
        changed.sort();
        assert_eq!(changed, ["NIFTY:FUT".to_string(), b.name.clone()]);

        // The bare chart lapses; the dated one keeps its ticks.
        running.state().tracked.get_mut("NIFTY:FUT").unwrap().last_used_ms -= 2 * IDLE_MS;
        assert!(!running.sweep(now_ms()));
        assert!(running.tracking("NIFTY:FUT").is_none());
        running.apply_tick("nse_fo|48704", t + MINUTE_MS + 1_000, 24_120.0);
        assert_eq!(running.today(&b.name, Tf::M1).len(), 2);
        assert_eq!(running.state().by_key.get("nse_fo|48704").unwrap(), &[b.name.clone()]);
    }

    #[tokio::test]
    async fn a_refused_stock_key_is_asked_for_again_with_the_next_callers_share() {
        let today = shared_domain::today_ist();
        let expiry = today + chrono::Days::new(10);
        let mut store = ScripStore::default();
        store.records.insert("RELIANCE".into(), vec![future_record("RELIANCE", "43001", expiry)]);
        let board = Arc::new(ExecuteBoard::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (running, _db, _db_rx) = service_with("retry", Some(store), Some(tx), Some(Arc::clone(&board))).await;
        let symbol = Symbol::Future { underlying: "RELIANCE".into(), expiry: None };
        let name = symbol.name();
        let key = "nse_fo|43001".to_string();

        // Tracked as a user's chart the budget refused (Upstox, here left
        // out, would draw it): `polled`, nothing sent.
        let now = now_ms();
        running.state().tracked.insert(name.clone(), Tracked {
            tracking: Tracking { name: name.clone(), kotak: Some(key.clone()), upstox: Some("NSE_FO|35001".into()), polled: true },
            kotak_sub: KotakSub::Budgeted { key: key.clone(), pool: SubPool::Public },
            kotak_on_feed: false,
            subscribed_at_ms: None,
            kotak_ticked: false,
            day: DaySeries::default(),
            forming: None,
            last_used_ms: now,
        });
        running.state().by_key.insert(key.clone(), vec![name.clone()]);

        // A visitor's share is none: still polled, nothing sent.
        assert!(running.track(symbol.clone(), SubPool::None).await.unwrap().polled);
        assert!(rx.try_recv().is_err());
        // The admin's share has room: the key goes out, and the chart waits
        // for the feed's first tick instead of staying polled.
        let tracking = running.track(symbol.clone(), SubPool::Owner).await.unwrap();
        assert!(!tracking.polled);
        let sent: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(sent, serde_json::json!({ "action": "subscribe", "scrips": key, "ladder": true }));
        {
            let st = running.state();
            let t = st.tracked.get(&name).unwrap();
            assert!(t.kotak_on_feed && t.subscribed_at_ms.is_some());
            assert!(matches!(&t.kotak_sub, KotakSub::Budgeted { pool: SubPool::Owner, .. }), "a fresh bridge asks the share that had room");
        }
        // On the feed now: asked again, nothing more is sent.
        assert!(!running.track(symbol, SubPool::Public).await.unwrap().polled);
        assert!(rx.try_recv().is_err());
    }

    fn contracts(underlying: &str, n: usize) -> Vec<(String, Vec<Derivative>)> {
        let list = (0..n)
            .map(|i| Derivative {
                underlying: underlying.into(),
                kind: "FUT".into(),
                expiry: NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
                strike: 0.0,
                instrument_key: format!("NSE_FO|{i}"),
                lot_size: 1,
            })
            .collect();
        vec![(underlying.to_string(), list)]
    }

    #[tokio::test]
    async fn a_contract_list_in_memory_never_waits_on_another_ones_load() {
        let lists = Arc::new(DerivativeLists::default());
        let day = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let indices: Vec<(String, Vec<Derivative>)> =
            shared_domain::INDEX_NAMES.iter().flat_map(|i| contracts(i, 2)).collect();
        assert_eq!(lists.get_or_load("NIFTY", day, || async { Ok(indices) }).await.unwrap().len(), 2);

        // A stock's load hangs (a slow download)...
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let loads = Arc::new(AtomicUsize::new(0));
        let slow = {
            let (lists, loads) = (Arc::clone(&lists), Arc::clone(&loads));
            tokio::spawn(async move {
                lists
                    .get_or_load("RELIANCE", day, || async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        let _ = wait.await;
                        Ok(contracts("RELIANCE", 3))
                    })
                    .await
            })
        };
        while loads.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // ...and an index list already loaded is answered at once, the
        // other indices' too (they load together), without loading.
        for index in ["NIFTY", "SENSEX"] {
            let quick = lists.get_or_load(index, day, || async { panic!("{index} is in memory") });
            let found = tokio::time::timeout(Duration::from_secs(1), quick).await.expect("not held up by the stock's load");
            assert_eq!(found.unwrap().len(), 2);
        }
        // A second chart of the same stock waits for that load and finds
        // its list, instead of loading it again.
        let again = {
            let (lists, loads) = (Arc::clone(&lists), Arc::clone(&loads));
            tokio::spawn(async move {
                lists
                    .get_or_load("RELIANCE", day, || async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok(contracts("RELIANCE", 3))
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        assert_eq!(slow.await.unwrap().unwrap().len(), 3);
        assert_eq!(again.await.unwrap().unwrap().len(), 3);
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        // The next instruments day loads afresh, and drops the old day's lists.
        let next = day.succ_opt().unwrap();
        assert_eq!(lists.get_or_load("RELIANCE", next, || async { Ok(contracts("RELIANCE", 4)) }).await.unwrap().len(), 4);
        assert!(matches!(lists.state().cached("NIFTY", next, Instant::now()), Cached::Missing));
    }

    #[tokio::test]
    async fn a_file_that_could_not_be_had_is_not_asked_for_again_for_a_minute() {
        let lists = DerivativeLists::default();
        let day = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let (nse, bse) = (upstox_client::NSE_INSTRUMENTS_URL, upstox_client::BSE_INSTRUMENTS_URL);
        let loads = AtomicUsize::new(0);
        let fails_on = |file: &'static str| {
            let loads = &loads;
            move || async move {
                loads.fetch_add(1, Ordering::SeqCst);
                Err(file)
            }
        };

        // BSE's file fails: the indices' lists (which need it) wait a
        // minute; a stock's, from NSE's file alone, still loads.
        assert!(lists.get_or_load("SENSEX", day, fails_on(bse)).await.is_none());
        assert!(lists.get_or_load("NIFTY", day, fails_on(bse)).await.is_none());
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert_eq!(lists.get_or_load("TCS", day, || async { Ok(contracts("TCS", 1)) }).await.unwrap().len(), 1);

        // NSE's file fails: every stock not yet loaded, and the indices, are
        // answered at once with nothing — no download to queue behind.
        assert!(lists.get_or_load("RELIANCE", day, fails_on(nse)).await.is_none());
        assert!(lists.get_or_load("RELIANCE", day, fails_on(nse)).await.is_none());
        assert!(lists.get_or_load("INFY", day, fails_on(nse)).await.is_none());
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        // A list already in memory is still answered.
        assert_eq!(lists.get_or_load("TCS", day, fails_on(nse)).await.unwrap().len(), 1);
        assert_eq!(loads.load(Ordering::SeqCst), 2);

        // A minute on, each file is asked for again.
        let now = Instant::now();
        let st = lists.state();
        assert!(matches!(st.cached("RELIANCE", day, now), Cached::Failed));
        let later = now + DERIVATIVES_RETRY + Duration::from_secs(1);
        assert!(matches!(st.cached("RELIANCE", day, later), Cached::Missing));
        assert!(matches!(st.cached("SENSEX", day, later), Cached::Missing));
        drop(st);
        lists.state().failed.values_mut().for_each(|at| *at = now - DERIVATIVES_RETRY - Duration::from_secs(1));
        assert_eq!(lists.get_or_load("RELIANCE", day, || async { Ok(contracts("RELIANCE", 2)) }).await.unwrap().len(), 2);
        assert!(!lists.state().failed.contains_key(nse), "a load that worked clears its file's failure");
    }
}
