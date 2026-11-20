//! Hyperliquid perps: the ones the app trades, and their market data.
//!
//! The perps on the list ([`markets`]) are charted and priced: BTC, ETH and
//! trade.xyz's gold to start with, and whatever else the admin adds. Their
//! charts are `HL:BTC`, `HL:GOLD`, `HL:xyz:NVDA` on the same candle routes
//! as everything else. The live bars, and each perp's mark, mid, oracle
//! price and funding, come from one WebSocket open only while the
//! `hyperliquid` switch is on, which follows only what is in use: a perp's
//! context while the header shows it, a screen has asked for it in the last
//! minute, a chart watches it or a paper book holds, rests or arms
//! something in it; its candles at a timeframe while a chart watches them.
//!
//! Each perp's bars at each timeframe are kept once a chart asks for them
//! (a [`Series`]): Hyperliquid's `candleSnapshot` is read for what isn't kept
//! yet — the older days a chart asks for, and the tail while the feed was
//! down — and one fetch at a time per perp and timeframe, so charts opening
//! together share it. Hyperliquid limits each IP to 1,200 request weight a
//! minute, and the order calls to come draw on the same budget. A series no
//! chart has watched for half an hour is dropped.
//!
//! The feed's `candle` channel can miss a bar's last trades, and sends
//! nothing for a minute without a trade (Hyperliquid's own candles have a
//! flat bar for it) or while it is down. So when a bar closes, and when the
//! feed comes back, the bars since are read again from `candleSnapshot` and
//! sent to the open charts as corrections — for the series a chart still
//! watches only.
//!
//! Market data comes from mainnet whatever the trading network: paper fills
//! are priced on the real market. Nothing here holds a key or can trade.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use hyperliquid_client::feed::{self, FeedMsg, Sub};
use hyperliquid_client::{AssetCtx, HlError, InfoClient, Network};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::task::JoinHandle;

use crate::candles::bars::{Bar, Tf};

pub mod live;
pub mod markets;
pub mod paper;
pub mod perp;

/// Room for a burst of bar changes while a stream catches up.
const CHANGES_CAPACITY: usize = 1024;
/// Most bars kept per perp and timeframe (Hyperliquid serves only its latest
/// 5,000 candles anyway).
const MAX_SERIES_BARS: usize = 12_000;
/// When a bar closes, the bars back to the one before are read again — at
/// most this many.
const CORRECT_MAX_BARS: i64 = 60;
/// A series no chart has asked for (or streamed) in this long is not
/// corrected any more, and its candles are not followed; the next chart to
/// ask reads its tail again.
const SERIES_IDLE_MS: i64 = 10 * 60_000;
/// A series no chart has watched in this long is dropped.
const SERIES_DROP_MS: i64 = 30 * 60_000;
/// A perp a screen asked about is followed for this long after.
const ASKED_FOR_MS: i64 = 60_000;
/// Hyperliquid allows 1,000 subscriptions an IP; the feed keeps below.
const MAX_SUBS: usize = 900;
/// How often what the feed follows is worked out again (and idle series
/// dropped).
const KEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// A perp the app trades, with Hyperliquid's rules for it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Market {
    /// The app's name for it: Hyperliquid's coin (`BTC`, `xyz:NVDA`), but
    /// `GOLD` for trade.xyz's gold, as it has been called from the start.
    pub key: &'static str,
    /// Hyperliquid's name: `BTC`, `xyz:GOLD`.
    pub coin: &'static str,
    /// Its name without the dex: `BTC`, `GOLD`, `NVDA`.
    pub base: &'static str,
    /// The builder dex it trades on (`xyz`); `None` on a core perp.
    pub dex: Option<&'static str>,
    /// What the screens call it: `BTC-PERP`, `NVDA-PERP (io)`.
    pub label: &'static str,
    pub name: &'static str,
    /// Decimals a size may carry: 5 for BTC is 0.00001 BTC.
    pub sz_decimals: u32,
    /// Hyperliquid's own leverage limit, which the app takes up to (the
    /// owner's pick, 2026-11-20: no lower cap of its own).
    pub max_leverage: u32,
    /// Fees as a fraction of notional at Hyperliquid's base tier: 0.045 %
    /// taker and 0.015 % maker on core perps; a builder-deployed one's scale
    /// with its deployer's share (double on trade.xyz's gold), a tenth of
    /// that in growth mode.
    pub taker_fee: f64,
    pub maker_fee: f64,
}

/// What a perp's chart symbol starts with.
pub const SYMBOL_PREFIX: &str = "HL:";

impl Market {
    /// Its chart symbol: `HL:BTC`.
    pub fn symbol(&self) -> String {
        format!("{SYMBOL_PREFIX}{}", self.key)
    }

    /// The market `HL:BTC` (any case) names, on the list (added or
    /// close-only).
    pub fn of_symbol(raw: &str) -> Option<&'static Market> {
        let raw = raw.trim();
        let rest = raw.get(..SYMBOL_PREFIX.len()).filter(|p| p.eq_ignore_ascii_case(SYMBOL_PREFIX)).map(|_| &raw[SYMBOL_PREFIX.len()..])?;
        Market::of_key(rest)
    }

    /// The market Hyperliquid calls `coin`, on the list.
    pub fn of_coin(coin: &str) -> Option<&'static Market> {
        markets::by_coin(coin).map(|e| e.market)
    }

    /// The market the app calls `key` (any case), on the list.
    pub fn of_key(key: &str) -> Option<&'static Market> {
        markets::by_key(key).map(|e| e.market)
    }

    /// Added (not close-only): it takes orders that open or add.
    pub fn tradable(&self) -> bool {
        markets::tradable(self.key)
    }

    /// The most leverage the app takes on it: Hyperliquid's own limit.
    pub fn leverage_cap(&self) -> u32 {
        self.max_leverage
    }

    /// Maintenance margin as a fraction of notional: half the initial
    /// margin at Hyperliquid's max leverage (1.25 % at 40x, 2 % at 25x).
    pub fn maintenance(&self) -> f64 {
        1.0 / (2.0 * f64::from(self.max_leverage))
    }

    /// `size` on the market's size grid, rounded down (never more than asked).
    pub fn round_size(&self, size: f64) -> f64 {
        round_size(size, self.sz_decimals)
    }

    /// `price` as Hyperliquid takes a perp price: at most five significant
    /// figures and `6 - sz_decimals` decimals (an integer always passes),
    /// rounded to the nearest.
    pub fn round_price(&self, price: f64) -> f64 {
        round_price(price, self.sz_decimals)
    }
}

/// `size` on a grid of `sz_decimals` decimals, rounded down (never more
/// than asked).
pub fn round_size(size: f64, sz_decimals: u32) -> f64 {
    let f = 10f64.powi(sz_decimals as i32);
    ((size * f) + 1e-9).floor() / f
}

/// `price` as Hyperliquid takes a perp price on a perp of `sz_decimals`: at
/// most five significant figures and `6 - sz_decimals` decimals (an integer
/// always passes), rounded to the nearest.
pub fn round_price(price: f64, sz_decimals: u32) -> f64 {
    if !(price.is_finite() && price > 0.0) {
        return price;
    }
    let max_decimals = 6 - sz_decimals as i32;
    let magnitude = price.log10().floor() as i32 + 1;
    let decimals = (5 - magnitude).clamp(0, max_decimals.max(0));
    let f = 10f64.powi(decimals);
    let rounded = (price * f).round() / f;
    // An integer price is always valid, however many figures it has.
    if decimals == 0 { price.round() } else { rounded }
}

/// Whether `raw` is a perp's chart symbol (or meant to be one: `HL:` and
/// anything after it), so the candle routes hand it here.
pub fn is_hl_symbol(raw: &str) -> bool {
    raw.trim().get(..SYMBOL_PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(SYMBOL_PREFIX))
}

/// A perp's bar that changed, for the candle stream: the forming bar as the
/// feed moves it, or an older one corrected from Hyperliquid's candles.
#[derive(Debug, Clone)]
pub struct BarChange {
    pub key: &'static str,
    pub tf: Tf,
    pub bar: Bar,
}

/// A perp's live context with when it arrived (Unix ms).
#[derive(Debug, Clone)]
pub struct Quote {
    pub ctx: AssetCtx,
    pub at_ms: i64,
}

#[derive(Default)]
struct Live {
    /// The feed is connected and subscribed.
    up: bool,
    /// It has been, since it was opened: a later `Up` is a reconnect.
    was_up: bool,
    quotes: HashMap<&'static str, Quote>,
    /// Each perp's forming bar at each timeframe, as the feed last sent it.
    /// Emptied when the feed drops: a bar from before the drop is stale.
    last: HashMap<(&'static str, Tf), Bar>,
}

/// One perp's bars at one timeframe, kept since a chart first asked.
struct Series {
    bars: BTreeMap<i64, Bar>,
    /// The bars are complete from here on: what was asked of Hyperliquid.
    from_ms: i64,
    /// The feed has kept the tail current since it was last read; while
    /// not, the next request reads it again.
    live: bool,
    /// When a chart last asked for it, or streamed it.
    touched_ms: i64,
}

impl Series {
    fn watched(&self, now: i64) -> bool {
        now - self.touched_ms < SERIES_IDLE_MS
    }

    fn tail_ms(&self) -> i64 {
        self.bars.last_key_value().map_or(self.from_ms, |(t, _)| *t)
    }
}

/// Merge bars read from Hyperliquid into `s`: what changed, oldest first. A
/// bar at or after `forming` that `s` already has is left alone — the feed's
/// copy of the forming bar is newer than one read a moment ago.
fn merge(s: &mut Series, fetched: Vec<Bar>, forming: Option<i64>) -> Vec<Bar> {
    let mut changed = Vec::new();
    for bar in fetched {
        if forming.is_some_and(|f| bar.t >= f) && s.bars.contains_key(&bar.t) {
            continue;
        }
        if s.bars.get(&bar.t) != Some(&bar) {
            s.bars.insert(bar.t, bar);
            changed.push(bar);
        }
    }
    while s.bars.len() > MAX_SERIES_BARS {
        s.bars.pop_first();
        if let Some((t, _)) = s.bars.first_key_value() {
            s.from_ms = *t;
        }
    }
    changed
}

/// One lock per perp and timeframe, made on first use.
type FetchLocks = HashMap<(&'static str, Tf), Arc<Mutex<()>>>;

struct Running {
    stop: watch::Sender<bool>,
    feed: JoinHandle<()>,
    reader: JoinHandle<()>,
    keeper: JoinHandle<()>,
}

/// Hyperliquid's market data while the `hyperliquid` switch is on.
pub struct HlMarketData {
    info: InfoClient,
    live: StdMutex<Live>,
    series: StdMutex<HashMap<(&'static str, Tf), Series>>,
    /// One Hyperliquid read at a time per perp and timeframe: charts opening
    /// together wait for the one fetch and share it.
    fetching: StdMutex<FetchLocks>,
    changes: broadcast::Sender<BarChange>,
    /// What the feed follows ([`HlMarketData::resubscribe`]).
    subs: watch::Sender<BTreeSet<Sub>>,
    /// One `resubscribe` at a time, from working out what is wanted to
    /// applying it: an older answer never lands over a newer one.
    resubscribing: StdMutex<()>,
    /// When a screen last asked about each perp (Unix ms).
    asked: StdMutex<HashMap<&'static str, i64>>,
    /// The perps a paper book holds, rests or arms something in, as the
    /// desk's last tick found them.
    in_use: StdMutex<HashSet<&'static str>>,
    running: Mutex<Option<Running>>,
}

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

fn bar_of(c: &hyperliquid_client::Candle) -> Bar {
    Bar { t: c.t, o: c.o, h: c.h, l: c.l, c: c.c, v: Some(c.v) }
}

/// The app's key for a coin the feed followed: the list's, or (for one no
/// longer on it) as the list would name it.
fn key_of_coin(coin: &str) -> Option<&'static str> {
    Market::of_coin(coin).map(|m| m.key)
}

impl HlMarketData {
    pub fn new() -> Arc<Self> {
        Arc::new(HlMarketData {
            info: InfoClient::new(Network::Mainnet),
            live: StdMutex::new(Live::default()),
            series: StdMutex::new(HashMap::new()),
            fetching: StdMutex::new(HashMap::new()),
            changes: broadcast::channel(CHANGES_CAPACITY).0,
            subs: watch::channel(BTreeSet::new()).0,
            resubscribing: StdMutex::new(()),
            asked: StdMutex::new(HashMap::new()),
            in_use: StdMutex::new(HashSet::new()),
            running: Mutex::new(None),
        })
    }

    // Lock order: `resubscribing`, then `live`, then `series`; the feed's
    // reader holds `live` while it reads `subs`. None is held across an
    // await; `asked` and `in_use` are never held with another.
    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn series(&self) -> MutexGuard<'_, HashMap<(&'static str, Tf), Series>> {
        self.series.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fetch_lock(&self, key: &'static str, tf: Tf) -> Arc<Mutex<()>> {
        let mut all = self.fetching.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(all.entry((key, tf)).or_default())
    }

    /// What the feed should follow now: each perp's context while the
    /// header shows it, a screen asked for it lately, a paper book uses it
    /// or a chart watches it; a series' candles while a chart watches it.
    /// Contexts come first, held perps first of all, should the cap bite.
    fn wanted(&self, now: i64) -> BTreeSet<Sub> {
        let in_use: Vec<&'static str> = self.in_use.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
        let asked: Vec<&'static str> = self.asked.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, at)| now - **at < ASKED_FOR_MS).map(|(k, _)| *k).collect();
        let watched: Vec<(&'static str, Tf)> = self.series().iter().filter(|(_, s)| s.watched(now)).map(|(k, _)| *k).collect();
        let header = markets::entries().into_iter().filter(|e| e.added && e.header).map(|e| e.market.key);
        let mut ctx: Vec<&'static str> = Vec::new();
        for key in in_use.into_iter().chain(header).chain(asked).chain(watched.iter().map(|(k, _)| *k)) {
            if !ctx.contains(&key) {
                ctx.push(key);
            }
        }
        let mut out = BTreeSet::new();
        let ctx_subs = ctx.iter().filter_map(|k| Market::of_key(k)).map(|m| Sub::Ctx { coin: m.coin.to_string() });
        let candle_subs = watched.iter().filter_map(|(k, tf)| Market::of_key(k).map(|m| Sub::Candle { coin: m.coin.to_string(), interval: tf.as_str().to_string() }));
        for sub in ctx_subs.chain(candle_subs) {
            if out.len() >= MAX_SUBS {
                tracing::warn!("Hyperliquid feed: more than {MAX_SUBS} subscriptions wanted — the rest wait");
                break;
            }
            out.insert(sub);
        }
        out
    }

    /// Work out what the feed follows again, and forget what it stops
    /// following: a context's quote, a series' forming bar (its tail is read
    /// again on the next ask).
    fn resubscribe(&self) {
        let _one = self.resubscribing.lock().unwrap_or_else(|e| e.into_inner());
        let wanted = self.wanted(now_ms());
        let mut dropped = Vec::new();
        self.subs.send_if_modified(|current| {
            if *current == wanted {
                return false;
            }
            dropped = current.difference(&wanted).cloned().collect();
            *current = wanted;
            true
        });
        if dropped.is_empty() {
            return;
        }
        let mut live = self.live();
        let mut all = self.series();
        for sub in dropped {
            match sub {
                Sub::Ctx { coin } => {
                    if let Some(key) = key_of_coin(&coin) {
                        live.quotes.remove(key);
                    }
                }
                Sub::Candle { coin, interval } => {
                    let (Some(key), Some(tf)) = (key_of_coin(&coin), Tf::parse(&interval)) else { continue };
                    live.last.remove(&(key, tf));
                    if let Some(s) = all.get_mut(&(key, tf)) {
                        s.live = false;
                    }
                }
            }
        }
    }

    /// A screen is about to show `keys`: follow their contexts for the next
    /// minute.
    pub fn ask(&self, keys: &[&'static str]) {
        if keys.is_empty() {
            return;
        }
        let now = now_ms();
        let fresh = {
            let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
            let mut fresh = false;
            for key in keys {
                fresh |= asked.insert(key, now).is_none_or(|at| now - at >= ASKED_FOR_MS);
            }
            fresh
        };
        if fresh {
            self.resubscribe();
        }
    }

    /// The perps the paper books use now (the desk's tick).
    pub fn set_in_use(&self, keys: HashSet<&'static str>) {
        let changed = {
            let mut in_use = self.in_use.lock().unwrap_or_else(|e| e.into_inner());
            let changed = *in_use != keys;
            *in_use = keys;
            changed
        };
        if changed {
            self.resubscribe();
        }
    }

    /// Drop the series no chart has watched in [`SERIES_DROP_MS`], forget
    /// old asks, and follow what is wanted now.
    fn keep(&self) {
        let now = now_ms();
        self.series().retain(|_, s| now - s.touched_ms < SERIES_DROP_MS);
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, at| now - *at < ASKED_FOR_MS);
        self.resubscribe();
    }

    /// Open the feed (`on`) or close it, as the switch now says.
    pub async fn switch(self: &Arc<Self>, on: bool) {
        let mut running = self.running.lock().await;
        if on == running.is_some() {
            return;
        }
        if let Some(r) = running.take() {
            let _ = r.stop.send(true);
            r.feed.abort();
            r.reader.abort();
            r.keeper.abort();
            *self.live() = Live::default();
            self.series().clear();
            self.asked.lock().unwrap_or_else(|e| e.into_inner()).clear();
            tracing::info!("Hyperliquid feed closed");
            return;
        }
        self.resubscribe();
        let (tx, rx) = mpsc::channel(4096);
        let (stop, stop_rx) = watch::channel(false);
        let feed = tokio::spawn(feed::run(Network::Mainnet, self.subs.subscribe(), tx, stop_rx));
        let reader = tokio::spawn(Arc::clone(self).read(rx));
        let me = Arc::clone(self);
        let keeper = tokio::spawn(async move {
            let mut tick = tokio::time::interval(KEEP_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                me.keep();
            }
        });
        *running = Some(Running { stop, feed, reader, keeper });
        tracing::info!("Hyperliquid feed opening ({} subscriptions)", self.subs.borrow().len());
    }

    /// Keep the feed's updates, until it ends.
    async fn read(self: Arc<Self>, mut rx: mpsc::Receiver<FeedMsg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                FeedMsg::Up => {
                    let reconnect = {
                        let mut live = self.live();
                        live.up = true;
                        std::mem::replace(&mut live.was_up, true)
                    };
                    tracing::info!("Hyperliquid feed up");
                    // What closed, or never came, while it was down.
                    if reconnect {
                        tokio::spawn(Arc::clone(&self).catch_up());
                    }
                }
                FeedMsg::Down(_) => {
                    {
                        let mut live = self.live();
                        live.up = false;
                        live.last.clear();
                    }
                    for s in self.series().values_mut() {
                        s.live = false;
                    }
                }
                // A message still in flight when its sub was dropped is not
                // kept: checked under `live`, which `resubscribe` takes only
                // after `subs` changed, so what it forgot stays forgotten.
                FeedMsg::Ctx(ctx) => {
                    if let Some(m) = Market::of_coin(&ctx.coin) {
                        let mut live = self.live();
                        if self.subs.borrow().contains(&Sub::Ctx { coin: ctx.coin.clone() }) {
                            live.quotes.insert(m.key, Quote { ctx, at_ms: now_ms() });
                        }
                    }
                }
                FeedMsg::Candle(c) => {
                    let (Some(m), Some(tf)) = (Market::of_coin(&c.coin), Tf::parse(&c.interval)) else { continue };
                    let bar = bar_of(&c);
                    let before = {
                        let mut live = self.live();
                        if !self.subs.borrow().contains(&Sub::Candle { coin: c.coin.clone(), interval: c.interval.clone() }) {
                            continue;
                        }
                        live.last.insert((m.key, tf), bar).map(|b| b.t)
                    };
                    let tail = {
                        let mut all = self.series();
                        all.get_mut(&(m.key, tf)).map(|s| {
                            let tail = s.tail_ms();
                            s.bars.insert(bar.t, bar);
                            tail
                        })
                    };
                    let _ = self.changes.send(BarChange { key: m.key, tf, bar });
                    // A new bar: the one before closed, maybe without its
                    // last trades, maybe with flat minutes since. The first
                    // bar since the series was subscribed: what closed
                    // between its read and now.
                    let closed = match before {
                        Some(t) => Some(t).filter(|t| *t < bar.t),
                        None => tail.filter(|t| *t < bar.t),
                    };
                    if let Some(closed) = closed {
                        tokio::spawn(Arc::clone(&self).correct(m, tf, closed));
                    }
                }
            }
        }
    }

    /// Read `m`'s bars at `tf` from `from_t` on again and send the closed
    /// ones that changed — if a chart still watches the series.
    async fn correct(self: Arc<Self>, m: &'static Market, tf: Tf, from_t: i64) {
        let now = now_ms();
        if !self.series().get(&(m.key, tf)).is_some_and(|s| s.watched(now)) {
            return;
        }
        let lock = self.fetch_lock(m.key, tf);
        let _one = lock.lock().await;
        let from = from_t.max(now - CORRECT_MAX_BARS * tf.ms());
        let fetched = match self.fetch(m, tf, from, now).await {
            Ok(bars) => bars,
            Err(e) => {
                tracing::debug!("Hyperliquid {} {} correction not read: {e}", m.label, tf.as_str());
                return;
            }
        };
        self.apply(m, tf, fetched, false);
    }

    /// The feed is back: read each watched series' tail again, and send what
    /// changed while it was down.
    async fn catch_up(self: Arc<Self>) {
        let now = now_ms();
        let wanted: Vec<((&'static str, Tf), i64)> =
            self.series().iter().filter(|(_, s)| s.watched(now)).map(|(k, s)| (*k, s.tail_ms())).collect();
        for ((key, tf), tail) in wanted {
            let Some(m) = Market::of_key(key) else { continue };
            let lock = self.fetch_lock(key, tf);
            let _one = lock.lock().await;
            match self.fetch(m, tf, tail, now_ms()).await {
                Ok(fetched) => self.apply(m, tf, fetched, true),
                Err(e) => tracing::warn!("Hyperliquid {} {} not caught up after a reconnect: {e}", m.label, tf.as_str()),
            }
        }
    }

    /// Merge bars just read into the series, and send the changed ones
    /// older than the feed's forming bar (all of them while the feed has
    /// none). `tail_read`: the read reached now, so the tail is current
    /// again while the feed is up.
    fn apply(&self, m: &'static Market, tf: Tf, fetched: Vec<Bar>, tail_read: bool) {
        let (up, forming) = {
            let live = self.live();
            (live.up, live.last.get(&(m.key, tf)).map(|b| b.t))
        };
        let changed = {
            let mut all = self.series();
            let Some(s) = all.get_mut(&(m.key, tf)) else { return };
            if tail_read {
                s.live = up;
            }
            merge(s, fetched, forming)
        };
        for bar in changed.into_iter().filter(|b| forming.is_none_or(|f| b.t < f)) {
            let _ = self.changes.send(BarChange { key: m.key, tf, bar });
        }
    }

    async fn fetch(&self, m: &Market, tf: Tf, from_ms: i64, to_ms: i64) -> Result<Vec<Bar>, HlError> {
        Ok(self.info.candles(m.coin, tf.as_str(), from_ms, to_ms).await?.iter().map(bar_of).collect())
    }

    /// Whether the feed is connected now.
    pub fn is_up(&self) -> bool {
        self.live().up
    }

    /// `key`'s latest context, if one has arrived since the feed opened.
    pub fn quote(&self, key: &str) -> Option<Quote> {
        self.live().quotes.get(key).cloned()
    }

    /// `key`'s forming bar at `tf`, as the feed last sent it.
    pub fn last_bar(&self, key: &str, tf: Tf) -> Option<Bar> {
        self.live().last.iter().find(|((k, t), _)| *k == key && *t == tf).map(|(_, b)| *b)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BarChange> {
        self.changes.subscribe()
    }

    /// An open chart still follows `keys` at `tf`: keep correcting them.
    pub fn touch(&self, keys: &[&'static str], tf: Tf) {
        let now = now_ms();
        {
            let mut all = self.series();
            for key in keys {
                if let Some(s) = all.get_mut(&(*key, tf)) {
                    s.touched_ms = now;
                }
            }
        }
        self.resubscribe();
    }

    /// `m`'s bars at `tf` opening in `[from_ms, to_ms]`, oldest first. Only
    /// what isn't kept is read from Hyperliquid: older bars than the series
    /// holds, and its tail unless the feed has kept it current. A tail that
    /// can't be read gives the kept bars; older ones that can't, an error.
    pub async fn history(&self, m: &'static Market, tf: Tf, from_ms: i64, to_ms: i64) -> Result<Vec<Bar>, HlError> {
        let lock = self.fetch_lock(m.key, tf);
        let _one = lock.lock().await;
        let now = now_ms();
        let up = self.is_up();
        let (head, tail) = {
            let mut all = self.series();
            match all.get_mut(&(m.key, tf)) {
                // Nothing kept: one read up to now is head and tail.
                None => (Some((from_ms, now)), None),
                Some(s) => {
                    s.touched_ms = now;
                    let head = (from_ms < s.from_ms).then(|| (from_ms, s.from_ms - 1));
                    let tail = (!(s.live && up)).then(|| s.tail_ms());
                    (head, tail)
                }
            }
        };
        let fresh = head.is_some_and(|(_, to)| to == now);
        let mut fetched = Vec::new();
        if let Some((a, b)) = head {
            fetched.extend(self.fetch(m, tf, a, b).await?);
        }
        let mut tail_read = fresh;
        if let Some(t) = tail {
            match self.fetch(m, tf, t, now).await {
                Ok(bars) => {
                    fetched.extend(bars);
                    tail_read = true;
                }
                Err(e) => tracing::warn!("Hyperliquid {} {} tail not read, serving the kept bars: {e}", m.label, tf.as_str()),
            }
        }
        let (up, forming) = {
            let live = self.live();
            (live.up, live.last.get(&(m.key, tf)).copied())
        };
        let mut all = self.series();
        let s = all.entry((m.key, tf)).or_insert_with(|| Series { bars: BTreeMap::new(), from_ms, live: false, touched_ms: now });
        s.from_ms = s.from_ms.min(from_ms);
        merge(s, fetched, forming.map(|b| b.t));
        if let Some(bar) = forming {
            // The feed's forming bar is the newest there is.
            s.bars.insert(bar.t, bar);
        }
        if tail_read {
            s.live = up;
        }
        let bars: Vec<Bar> = s.bars.range(from_ms..=to_ms).map(|(_, b)| *b).collect();
        drop(all);
        // A new series: its candles are followed from now.
        self.resubscribe();
        Ok(bars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_prices_follow_hyperliquids_grid() {
        let btc = Market::of_key("BTC").unwrap();
        let eth = Market::of_key("ETH").unwrap();
        let gold = Market::of_key("GOLD").unwrap();
        assert_eq!(btc.round_size(0.123456789), 0.12345);
        assert_eq!(eth.round_size(1.99999), 1.9999);
        assert_eq!(btc.round_price(63_856.37), 63_856.0, "five figures");
        assert_eq!(btc.round_price(63_856.5), 63_857.0);
        assert_eq!(btc.round_price(123_456.7), 123_457.0, "an integer always passes");
        assert_eq!(eth.round_price(1_805.437), 1_805.4);
        assert_eq!(gold.round_price(3_733.456), 3_733.5);
        assert_eq!(eth.round_price(0.0123456), 0.01, "at most 6 - 4 = 2 decimals on ETH");
        assert_eq!(btc.round_price(0.0123456), 0.0, "at most 6 - 5 = 1 decimal on BTC");
        assert_eq!((btc.leverage_cap(), eth.leverage_cap()), (40, 25), "Hyperliquid's own limits");
        assert!((btc.maintenance() - 0.0125).abs() < 1e-12 && (gold.maintenance() - 0.02).abs() < 1e-12);
        assert_eq!(gold.taker_fee, 2.0 * btc.taker_fee, "a builder-deployed perp's fees are double");
    }

    #[test]
    fn a_symbol_names_a_perp_on_the_list() {
        assert_eq!(Market::of_symbol("HL:BTC").map(|m| m.coin), Some("BTC"));
        assert_eq!(Market::of_symbol("hl:gold").map(|m| m.coin), Some("xyz:GOLD"));
        assert_eq!(Market::of_symbol(" HL:ETH ").map(|m| m.label), Some("ETH-PERP"));
        assert_eq!(Market::of_symbol("HL:DOGE-NOT-LISTED"), None, "not on the list");
        assert_eq!(Market::of_symbol("BTC"), None);
        assert_eq!(Market::of_symbol("NIFTY"), None);
        assert!(is_hl_symbol("HL:SOL"), "an unknown perp is still the perps' to refuse");
        assert!(!is_hl_symbol("NIFTY") && !is_hl_symbol("H"));
        assert_eq!(Market::of_coin("xyz:GOLD").map(|m| m.key), Some("GOLD"));
        assert_eq!(Market::of_coin("GOLD"), None, "only trade.xyz's gold");
        assert_eq!(Market::of_key("ETH").map(|m| m.coin), Some("ETH"));
        assert_eq!(markets::BUILTIN.iter().map(Market::symbol).collect::<Vec<_>>(), ["HL:BTC", "HL:ETH", "HL:GOLD"]);
    }

    fn bar(t: i64, c: f64) -> Bar {
        Bar { t, o: c, h: c, l: c, c, v: Some(1.0) }
    }

    fn series(bars: &[Bar]) -> Series {
        Series { bars: bars.iter().map(|b| (b.t, *b)).collect(), from_ms: 0, live: true, touched_ms: 0 }
    }

    #[test]
    fn a_read_corrects_closed_bars_and_fills_quiet_minutes_but_not_the_feeds_forming_bar() {
        // The feed's copy of the 1-minute bar at 60 s missed its last trade,
        // nothing traded at 120 s, and 180 s is forming.
        let mut s = series(&[bar(0, 10.0), bar(60_000, 11.0), bar(180_000, 13.0)]);
        let read = vec![bar(0, 10.0), bar(60_000, 11.5), bar(120_000, 11.5), bar(180_000, 12.5)];
        let changed = merge(&mut s, read, Some(180_000));
        assert_eq!(changed.iter().map(|b| (b.t, b.c)).collect::<Vec<_>>(), [(60_000, 11.5), (120_000, 11.5)]);
        assert_eq!(s.bars[&180_000].c, 13.0, "the feed's forming bar stays");
        // With no forming bar known (the feed is down), the read wins.
        let changed = merge(&mut s, vec![bar(180_000, 12.5)], None);
        assert_eq!(changed.iter().map(|b| b.c).collect::<Vec<_>>(), [12.5]);
        // Nothing new: nothing changed.
        assert!(merge(&mut s, vec![bar(0, 10.0)], None).is_empty());
    }

    #[test]
    fn a_series_keeps_its_newest_bars_and_says_where_it_starts() {
        let mut s = series(&[]);
        let many: Vec<Bar> = (0..(MAX_SERIES_BARS as i64 + 5)).map(|i| bar(i * 60_000, 1.0)).collect();
        merge(&mut s, many, None);
        assert_eq!(s.bars.len(), MAX_SERIES_BARS);
        assert_eq!(s.from_ms, 5 * 60_000);
        assert_eq!(s.tail_ms(), (MAX_SERIES_BARS as i64 + 4) * 60_000);
        assert!(s.watched(SERIES_IDLE_MS - 1) && !s.watched(SERIES_IDLE_MS));
    }
}
