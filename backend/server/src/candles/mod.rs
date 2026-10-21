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

pub mod bars;
pub mod symbols;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

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
    /// Kotak price-map key (`idx|NIFTY 50`, `nse_fo|51342`), when Kotak's
    /// scrip master knows the contract.
    pub kotak: Option<String>,
    /// Upstox instrument key, when Upstox is configured and knows it.
    pub upstox: Option<String>,
}

#[derive(Debug)]
pub enum TrackError {
    /// Neither broker lists this contract (expired, or no such strike).
    NotFound,
    /// The service stopped while this was being set up; ask again.
    Stopped,
}

struct Tracked {
    tracking: Tracking,
    /// What the bridge is sent to stream it.
    kotak_subscribe: Option<String>,
    day: DaySeries,
    /// The minute ticks are currently building.
    forming: Option<i64>,
    last_used_ms: i64,
}

struct State {
    day_start: i64,
    tracked: HashMap<String, Tracked>,
    /// Kotak or Upstox feed key → symbol name.
    by_key: HashMap<String, String>,
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
        })
        .await;
        *slot = Some(Arc::clone(&running));
        running
    }

    /// Track `symbol` on the running service (starting it if need be).
    pub async fn track(&self, state: &AppState, symbol: Symbol) -> Result<(Arc<Running>, Tracking), TrackError> {
        for _ in 0..2 {
            let running = self.get(state).await;
            match running.track(symbol).await {
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
}

pub struct Running {
    upstox: Option<UpstoxClient>,
    db_tx: mpsc::Sender<DbWriteMessage>,
    pool: SqlitePool,
    scrip_store: Arc<RwLock<Option<ScripStore>>>,
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    state: StdMutex<State>,
    /// Upstox's index futures and options, loaded once per IST day on the
    /// first future or option asked for.
    derivatives: Mutex<Option<(NaiveDate, Arc<Vec<Derivative>>)>>,
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
            state: StdMutex::new(State {
                day_start: bars::ist_day_start(now),
                tracked: HashMap::new(),
                by_key: HashMap::new(),
                kotak_channel: None,
                last_error: HashMap::new(),
            }),
            derivatives: Mutex::new(None),
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
    pub async fn track(self: &Arc<Self>, symbol: Symbol) -> Result<Tracking, TrackError> {
        let name = symbol.name();
        let now = now_ms();
        self.last_used_ms.store(now, Ordering::Release);
        {
            let mut st = self.state();
            if self.stopped.load(Ordering::Acquire) {
                return Err(TrackError::Stopped);
            }
            if let Some(t) = st.tracked.get_mut(&name) {
                t.last_used_ms = now;
                return Ok(t.tracking.clone());
            }
        }

        let today = shared_domain::today_ist();
        let upstox = match symbol {
            _ if self.upstox.is_none() => None,
            Symbol::Index(index) => symbols::upstox_index_key(index).map(str::to_string),
            _ => match self.derivatives(today).await {
                Some(list) => symbols::find_upstox(&symbol, &list, today).map(|d| d.instrument_key.clone()),
                None => None,
            },
        };
        let kotak = match symbol {
            Symbol::Index(index) => shared_domain::kotak_index_feed(index).map(|(segment, feed_name)| {
                let msg = serde_json::json!({ "action": "subscribe_indices", "indices": format!("{segment}|{feed_name}") });
                (shared_domain::index_price_key(feed_name), msg.to_string())
            }),
            _ => {
                let store = self.scrip_store.read().await;
                store.as_ref().and_then(|s| symbols::find_kotak(&symbol, s, today)).map(|record| {
                    let key = record.ws_key();
                    // `ladder`: the bridge drops these at the day's close.
                    let msg = serde_json::json!({ "action": "subscribe", "scrips": key, "ladder": true });
                    (key, msg.to_string())
                })
            }
        };
        if upstox.is_none() && kotak.is_none() {
            return Err(TrackError::NotFound);
        }
        let (kotak_key, kotak_subscribe) = kotak.unzip();
        let tracking = Tracking { name: name.clone(), kotak: kotak_key, upstox };

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
                st.by_key.insert(key.clone(), name.clone());
            }
            st.tracked.entry(name.clone()).or_insert(Tracked {
                tracking: tracking.clone(),
                kotak_subscribe: kotak_subscribe.clone(),
                day,
                forming: None,
                last_used_ms: now,
            });
        }
        if let Some(msg) = &kotak_subscribe {
            self.subscribe_kotak(msg).await;
        }
        if let Some(key) = &tracking.upstox {
            self.upstox_keys.send_modify(|keys| {
                keys.insert(key.clone());
            });
            self.fetch_official(&name, key).await;
        }
        tracing::info!(symbol = %name, kotak = ?tracking.kotak, upstox = ?tracking.upstox, "Candle service tracking");
        Ok(tracking)
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

    /// Fold one price into the symbol `key` belongs to, if it is tracked.
    fn apply_tick(&self, key: &str, at_ms: i64, price: f64) {
        let mut st = self.state();
        let Some(name) = st.by_key.get(key).cloned() else { return };
        let Some(t) = st.tracked.get_mut(&name) else { return };
        let Some(minute) = t.day.tick(at_ms, price) else { return };
        // The minute before just closed: cache it unless Upstox already has it.
        let closed = t.forming.filter(|m| *m != minute).filter(|m| !t.day.has_official(*m));
        t.forming = Some(minute);
        if let Some(bar) = closed.and_then(|m| t.day.ticked_minute(m)) {
            let _ = self.db_tx.try_send(candle_minute(&name, bar, "ticks"));
        }
        drop(st);
        let _ = self.changes.send(Change { symbol: name.into(), minute });
    }

    /// Ask the bridge to stream one symbol. Which channel already has every
    /// subscription is `resubscribe_kotak`'s to track, not this one's.
    async fn subscribe_kotak(&self, msg: &str) {
        if let Some(tx) = self.ws_tx.lock().await.clone() {
            let _ = tx.send(msg.to_string());
        }
    }

    /// A bridge this service hasn't sent everything to yet — the first one,
    /// or a fresh one after a Kotak re-login — gets every subscription.
    async fn resubscribe_kotak(&self) {
        let Some(tx) = self.ws_tx.lock().await.clone() else { return };
        let messages: Vec<String> = {
            let mut st = self.state();
            if st.kotak_channel.as_ref().is_some_and(|c| c.same_channel(&tx)) {
                return;
            }
            st.kotak_channel = Some(tx.clone());
            st.tracked.values().filter_map(|t| t.kotak_subscribe.clone()).collect()
        };
        for msg in messages {
            let _ = tx.send(msg);
        }
    }

    /// Upstox's index derivatives for `today`, downloaded on first use.
    async fn derivatives(&self, today: NaiveDate) -> Option<Arc<Vec<Derivative>>> {
        let client = self.upstox.as_ref()?;
        let mut slot = self.derivatives.lock().await;
        if let Some((day, list)) = slot.as_ref() {
            if *day == today {
                return Some(Arc::clone(list));
            }
        }
        let mut list = Vec::new();
        for (url, wanted) in [
            (upstox_client::NSE_INSTRUMENTS_URL, &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"][..]),
            (upstox_client::BSE_INSTRUMENTS_URL, &["SENSEX", "BANKEX"][..]),
        ] {
            match client.index_derivatives(url, wanted).await {
                Ok(rows) => list.extend(rows),
                Err(e) => {
                    tracing::warn!(error = %e, url, "Upstox instruments download failed — futures and options charts have no Upstox data until it works");
                    return None;
                }
            }
        }
        let list = Arc::new(list);
        tracing::info!(contracts = list.len(), "Upstox index derivatives loaded for the charts");
        *slot = Some((today, Arc::clone(&list)));
        Some(list)
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
        let mut lagged = 0u64;
        loop {
            match rx.recv().await {
                Ok(t) => self.apply_tick(&t.key, t.at_ms, t.ltp),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    lagged += n;
                    if lagged == n {
                        tracing::warn!(missed = n, "Candle service fell behind Kotak's ticks — some ticks skipped");
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
                    let want = weekday
                        && bars::in_session(now.timestamp_millis())
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
                    st.by_key.remove(key);
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
        // The contract map is the biggest thing held; nothing loads it while
        // stopped, so a busy lock just means it goes with the service.
        if let Ok(mut derivatives) = self.derivatives.try_lock() {
            *derivatives = None;
        }
        tracing::info!("Candle service stopped — no chart has asked for anything in 10 minutes");
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for task in tasks {
            // This sweep runs inside one of them; aborting itself is harmless.
            task.abort();
        }
        true
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
        let db = TempDb::new(name);
        let pool = crate::db::init_db(&format!("sqlite://{}", db.0.display())).await;
        let (db_tx, db_rx) = mpsc::channel(64);
        let running = Running::start(Deps {
            upstox: None,
            db_tx,
            pool,
            scrip_store: Arc::new(RwLock::new(None)),
            ws_tx: Arc::new(Mutex::new(None)),
        })
        .await;
        (running, db, db_rx)
    }

    /// Unix ms of `h:m` IST today.
    fn today_at(h: i64, m: i64) -> i64 {
        bars::ist_day_start(now_ms()) + (h * 60 + m) * MINUTE_MS
    }

    #[tokio::test]
    async fn ticks_for_a_tracked_symbol_build_its_bars() {
        let (running, _db, mut db_rx) = service("ticks").await;
        let tracking = running.track(Symbol::Index("NIFTY")).await.unwrap();
        assert_eq!(tracking.kotak.as_deref(), Some("idx|NIFTY 50"));
        let mut changes = running.subscribe_changes();

        let t = today_at(9, 20);
        running.apply_tick("idx|NIFTY 50", t + 1_000, 22_800.0);
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
        running.track(Symbol::Index("NIFTY")).await.unwrap();
        let later = |ms: i64| now_ms() + ms;

        assert!(!running.sweep(later(MINUTE_MS)), "just asked for");
        // A chart left open keeps the service up, even once its symbol lapses.
        let stream = running.open_stream();
        assert!(!running.sweep(later(IDLE_MS + MINUTE_MS)));
        assert_eq!(running.touch(&["NIFTY".to_string()]), ["NIFTY"], "the idle symbol was dropped");
        drop(stream);
        assert!(!running.sweep(later(MINUTE_MS)), "the stream only just closed");
        assert!(running.sweep(later(IDLE_MS + MINUTE_MS)));
        assert!(matches!(running.track(Symbol::Index("NIFTY")).await, Err(TrackError::Stopped)));
    }
}
