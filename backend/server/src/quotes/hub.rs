//! The hub: one quote per key from the best source there is, and the
//! bookkeeping the poller and the close sweep work from.
//!
//! `get_at(key, now)` answers for a price-map key (`nse_cm|2885`,
//! `nse_fo|51386`, `idx|NIFTY 50`). The candidates are the feed's tick for
//! a key the feed carries (read from the shared price map, which only the
//! feed task writes), the latest Kotak REST read, and Upstox's quote while
//! Kotak is down during hours; the freshest of them gives the price, a feed
//! tick of the last ten seconds ahead of any REST read, and the others fill
//! in the day's figures it lacks. After the close, the 15:35 sweep's row is
//! served as the day's close instead, until the next day's quotes arrive.
//!
//! What a quote tells a signed-in user is the price and the day's figures,
//! never where the price came from in a way that names the feed: the feed
//! carries exactly the contracts the owner's engine, the ladder and the
//! paper desks subscribed, so a contract answered from the feed before the
//! REST poller has read it would say the owner holds it. Hence a contract
//! key's feed tick is used only once the key has a REST read too (so the
//! first call for any contract is `missing` alike), and `source` is
//! serialised as `live` for the feed and REST both.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, NaiveDate};
use dashmap::DashMap;
use kotak_client::{KotakError, KotakQuote};
use serde::{Serialize, Serializer};
use shared_domain::{index_close_key, telemetry};
use upstox_client::UpstoxClient;

use super::eod::{self, Snapshot};
use super::rest::{self, Chunk, Outcome, Scheduler};

/// A feed tick this recent gives the price over a REST read of the same
/// key, whichever was read later.
pub const FEED_FRESH: Duration = Duration::from_secs(10);
/// Keys asked for within this are watched: the close sweep includes them.
pub const WATCH_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Keys the hub keeps interest in at once; past it the least recently
/// asked for are forgotten. Caps what one caller can set polling (and what
/// the close sweep takes on beyond the catalog's cash keys) at twelve
/// seconds of calls — a screen shows a few dozen keys, not hundreds.
pub const MAX_WANTED: usize = 600;
/// A REST or Upstox quote not refreshed for this long is dropped.
const EVICT_AFTER: Duration = Duration::from_secs(30 * 60);
const EVICT_EVERY: Duration = Duration::from_secs(60);
/// The Upstox fallback: keys a call, and its pause after an error.
const UPSTOX_BATCH: usize = 100;
const UPSTOX_PAUSE: Duration = Duration::from_secs(5);
const UPSTOX_PAUSE_MAX: Duration = Duration::from_secs(60);

/// Where a quote's price came from. Serialised by [`Source::label`]: the
/// feed and REST both as `live`, so an answer never says which keys the
/// feed carries (the owner's and the paper desks' contracts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The Kotak feed's tick.
    Feed,
    /// Kotak's REST quotes API.
    Rest,
    /// Upstox's quotes, standing in while Kotak is down.
    Upstox,
    /// The close sweep: the day's close, served after hours.
    Close,
}

impl Source {
    /// What a client sees: `live` (Kotak's feed or REST, during hours),
    /// `upstox` (the fallback while Kotak is down; equities and indices
    /// only, so it names no contract) or `close` (the 15:35 snapshot).
    pub fn label(self) -> &'static str {
        match self {
            Source::Feed | Source::Rest => "live",
            Source::Upstox => "upstox",
            Source::Close => "close",
        }
    }
}

impl Serialize for Source {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

/// One instrument's quote. Every figure is serialised, `null` when absent,
/// like the rest of the v2 answers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Quote {
    /// The price-map key.
    pub key: String,
    pub ltp: f64,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    /// The previous close.
    pub close: Option<f64>,
    /// Against the previous close, for this quote's own `ltp`.
    pub change: Option<f64>,
    pub per_change: Option<f64>,
    pub volume: Option<f64>,
    pub oi: Option<f64>,
    pub year_high: Option<f64>,
    pub year_low: Option<f64>,
    /// Unix ms the price was read: the tick's, the REST read's, the sweep's.
    pub at: i64,
    pub source: Source,
    /// For a close: the IST day and time it stands for, `YYYY-MM-DD 15:30`.
    pub as_of: Option<String>,
}

impl Quote {
    /// A quote with the price alone.
    pub fn bare(key: &str, ltp: f64, at: i64, source: Source) -> Self {
        Quote {
            key: key.to_string(),
            ltp,
            open: None,
            high: None,
            low: None,
            close: None,
            change: None,
            per_change: None,
            volume: None,
            oi: None,
            year_high: None,
            year_low: None,
            at,
            source,
            as_of: None,
        }
    }

    /// Fills in every figure this quote lacks from `other`.
    fn fill_from(&mut self, other: &Quote) {
        self.open = self.open.or(other.open);
        self.high = self.high.or(other.high);
        self.low = self.low.or(other.low);
        self.close = self.close.or(other.close);
        self.change = self.change.or(other.change);
        self.per_change = self.per_change.or(other.per_change);
        self.volume = self.volume.or(other.volume);
        self.oi = self.oi.or(other.oi);
        self.year_high = self.year_high.or(other.year_high);
        self.year_low = self.year_low.or(other.year_low);
    }
}

/// A positive price, else nothing: Kotak sends `0` for a figure it has none
/// of (an index's 52-week range, the open before the first trade).
fn positive(v: Option<f64>) -> Option<f64> {
    v.filter(|p| p.is_finite() && *p > 0.0)
}

/// Change and percent change against a previous close.
fn change_against(ltp: f64, close: Option<f64>) -> (Option<f64>, Option<f64>) {
    match close {
        Some(c) if c > 0.0 => (Some(ltp - c), Some((ltp - c) / c * 100.0)),
        _ => (None, None),
    }
}

/// The hub's quote from a Kotak REST one, read at `at`; `None` without a
/// price.
pub fn from_kotak(key: &str, q: &KotakQuote, at: i64) -> Option<Quote> {
    let ltp = positive(q.ltp)?;
    let close = positive(q.ohlc.close);
    let (change, per_change) = change_against(ltp, close);
    Some(Quote {
        open: positive(q.ohlc.open),
        high: positive(q.ohlc.high),
        low: positive(q.ohlc.low),
        close,
        change: q.change.or(change),
        per_change: q.per_change.or(per_change),
        volume: q.last_volume.filter(|v| *v >= 0.0),
        oi: q.oi.filter(|v| *v >= 0.0),
        year_high: positive(q.year_high),
        year_low: positive(q.year_low),
        ..Quote::bare(key, ltp, at, Source::Rest)
    })
}

/// The hub's quote from an Upstox one, read at `at`; `None` without a price.
pub fn from_upstox(key: &str, q: &upstox_client::Quote, at: i64) -> Option<Quote> {
    let ltp = positive(Some(q.last_price))?;
    let ohlc = q.ohlc.clone().unwrap_or_default();
    let close = positive(ohlc.close);
    let (change, per_change) = change_against(ltp, close);
    Some(Quote {
        open: positive(ohlc.open),
        high: positive(ohlc.high),
        low: positive(ohlc.low),
        close,
        change: q.net_change.or(change),
        per_change,
        volume: Some(q.volume).filter(|v| *v > 0.0),
        oi: Some(q.oi).filter(|v| *v > 0.0),
        ..Quote::bare(key, ltp, at, Source::Upstox)
    })
}

/// The live quote for a key from its candidates: the freshest gives the
/// price, except that a feed tick of the last [`FEED_FRESH`] always does;
/// the rest fill in what it lacks. The change is always the winner's own
/// LTP against the close: a feed tick that borrows REST's previous close
/// gets its change worked out from it, never REST's change for REST's
/// (older) price.
pub fn merge_live(feed: Option<Quote>, rest: Option<Quote>, upstox: Option<Quote>, now_ms: i64) -> Option<Quote> {
    let mut candidates: Vec<Quote> = [feed, rest, upstox].into_iter().flatten().collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by_key(|q| std::cmp::Reverse(q.at));
    let fresh_feed = candidates
        .iter()
        .position(|q| q.source == Source::Feed && now_ms.saturating_sub(q.at) <= FEED_FRESH.as_millis() as i64);
    if let Some(i) = fresh_feed {
        let feed = candidates.remove(i);
        candidates.insert(0, feed);
    }
    let mut winner = candidates[0].clone();
    let own_change = winner.change.is_some();
    for other in &candidates[1..] {
        winner.fill_from(other);
    }
    if !own_change {
        let (change, per_change) = change_against(winner.ltp, winner.close);
        winner.change = change;
        winner.per_change = per_change;
    }
    Some(winner)
}

/// Whether `key` is a derivative's (`nse_fo|…`, `bse_fo|…`): the keys the
/// feed carries only because someone holds or watches the contract.
fn is_contract_key(key: &str) -> bool {
    key.split_once('|').is_some_and(|(segment, _)| segment.ends_with("_fo"))
}

/// What `get` answers from a live quote and the close on file: the close
/// once the market is shut, for a live quote from that day or earlier (a
/// REST read at 15:39 says the same as the sweep did, as the day's close);
/// the live quote whenever the market is open, or when it is from a later
/// day than the close (the morning's quotes after the login).
pub fn serve(live: Option<Quote>, close: Option<(NaiveDate, Quote)>, now: DateTime<FixedOffset>) -> Option<Quote> {
    match (live, close) {
        (Some(live), Some((day, close))) => {
            if !eod::open_at(now) && day_of(live.at) <= day {
                Some(close)
            } else {
                Some(live)
            }
        }
        (Some(live), None) => Some(live),
        (None, close) => close.map(|(_, q)| q),
    }
}

/// The IST day of a Unix-ms instant.
pub fn day_of(at_ms: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(at_ms)
        .map(|t| t.with_timezone(&shared_domain::ist_offset()).date_naive())
        .unwrap_or(NaiveDate::MIN)
}

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

/// Upstox's instrument key for a price-map key, when the catalog knows one.
pub type UpstoxKeyFn = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Unix seconds of the feed's last tick for a price-map key.
pub type FeedTickFn = Box<dyn Fn(&str) -> Option<f64> + Send + Sync>;

/// A quote with when the hub last refreshed it, for eviction.
struct Held {
    quote: Quote,
    seen: Instant,
}

/// The close sweep in progress.
pub struct Sweep {
    pub day: NaiveDate,
    pub started: DateTime<FixedOffset>,
    deadline: Instant,
    pending: VecDeque<String>,
    in_flight: usize,
    pub total: usize,
    pub rows: HashMap<String, eod::Row>,
}

impl Sweep {
    fn done(&self, now: Instant) -> bool {
        (self.pending.is_empty() && self.in_flight == 0) || now >= self.deadline
    }

    /// Whether the deadline passed with keys still to ask for (or asked
    /// and unanswered).
    pub fn cut_short(&self) -> bool {
        !self.pending.is_empty() || self.in_flight > 0
    }
}

struct UpstoxState {
    last_sent: HashMap<String, Instant>,
    backoff: rest::Backoff,
}

/// How far back the request rate counts.
const RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Stats {
    calls: u64,
    refused: u64,
    rate_limited: u64,
    errors: u64,
    /// Quotes a call answered with no key the hub asked for.
    unmatched: u64,
    upstox_calls: u64,
    upstox_errors: u64,
    /// Failed calls since the last good one.
    error_streak: u32,
    last_call_at: Option<String>,
    last_error: Option<String>,
    /// When each Kotak call of the last [`RATE_WINDOW`] was made, oldest
    /// first: the request rate the System screen shows.
    recent_calls: VecDeque<Instant>,
    recent_upstox_calls: VecDeque<Instant>,
}

impl Stats {
    /// Notes a call at `now` and drops those older than the window.
    fn note(recent: &mut VecDeque<Instant>, now: Instant) {
        recent.push_back(now);
        Self::prune(recent, now);
    }

    fn prune(recent: &mut VecDeque<Instant>, now: Instant) {
        while recent.front().is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW) {
            recent.pop_front();
        }
    }
}

/// The close sweep's progress, for the System screen.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SweepStatus {
    pub day: String,
    pub total: usize,
    pub done: usize,
}

/// The close on file, for the System screen.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CloseStatus {
    pub day: String,
    pub taken_at: String,
    /// Keys with a close.
    pub rows: usize,
    /// Keys the sweep asked for.
    pub asked: usize,
    /// Some key got no close: the sweep was cut short by the 15:40
    /// deadline, or Kotak left keys out.
    pub partial: bool,
}

/// What the hub is doing, for the System screen and the log.
#[derive(Clone, Debug, Default, Serialize)]
pub struct HubStatus {
    /// Keys asked for in the last 15 s.
    pub wanted: usize,
    /// Kotak REST quotes held.
    pub quotes: usize,
    /// Upstox quotes held.
    pub upstox_quotes: usize,
    pub batch_size: usize,
    pub quarantined: usize,
    pub calls: u64,
    pub refused: u64,
    pub rate_limited: u64,
    pub errors: u64,
    pub unmatched: u64,
    pub upstox_calls: u64,
    pub upstox_errors: u64,
    /// Kotak quotes calls in the last minute, and the same as calls a
    /// second: the hub's share of the 10 requests a second Kotak allows.
    pub calls_last_minute: u64,
    pub calls_per_sec: f64,
    /// Upstox quotes calls in the last minute (the fallback while Kotak is down).
    pub upstox_calls_last_minute: u64,
    /// IST `HH:MM:SS` of the last call that answered.
    pub last_call_at: Option<String>,
    pub last_error: Option<String>,
    /// Why nothing is being polled, when nothing is.
    pub paused: Option<String>,
    pub sweep: Option<SweepStatus>,
    pub close: Option<CloseStatus>,
}

/// `HubStatus.paused` while the `search` switch is off.
pub const PAUSED_SWITCHED_OFF: &str = "the `search` switch is off";

/// The quote hub — see the module doc.
pub struct QuoteHub {
    /// The feed's price map, read only.
    prices: Arc<DashMap<String, f64>>,
    feed_tick: FeedTickFn,
    upstox_key: UpstoxKeyFn,
    rest: DashMap<String, Held>,
    upstox: DashMap<String, Held>,
    /// When each key was last asked for.
    wanted: Mutex<HashMap<String, Instant>>,
    sched: Mutex<Scheduler>,
    sweep: Mutex<Option<Sweep>>,
    snapshot: RwLock<Option<Arc<Snapshot>>>,
    upstox_state: Mutex<UpstoxState>,
    stats: Mutex<Stats>,
    last_evicted: Mutex<Instant>,
    /// Whether the last poll found a healthy Kotak session.
    session_ok: Mutex<bool>,
    /// Whether the poller last found the `search` switch off.
    switched_off: Mutex<bool>,
}

impl QuoteHub {
    /// A hub over the feed's price map; `upstox_key` names what Upstox
    /// calls a key, for the fallback.
    pub fn new(prices: Arc<DashMap<String, f64>>, upstox_key: UpstoxKeyFn) -> Self {
        Self::with_feed(prices, upstox_key, Box::new(telemetry::scrip_last_tick))
    }

    /// With the feed's last-tick times supplied by `feed_tick`.
    pub fn with_feed(prices: Arc<DashMap<String, f64>>, upstox_key: UpstoxKeyFn, feed_tick: FeedTickFn) -> Self {
        let now = Instant::now();
        QuoteHub {
            prices,
            feed_tick,
            upstox_key,
            rest: DashMap::new(),
            upstox: DashMap::new(),
            wanted: Mutex::new(HashMap::new()),
            sched: Mutex::new(Scheduler::new(now)),
            sweep: Mutex::new(None),
            snapshot: RwLock::new(None),
            upstox_state: Mutex::new(UpstoxState {
                last_sent: HashMap::new(),
                backoff: rest::Backoff::new(UPSTOX_PAUSE, UPSTOX_PAUSE_MAX),
            }),
            stats: Mutex::new(Stats::default()),
            last_evicted: Mutex::new(now),
            session_ok: Mutex::new(false),
            switched_off: Mutex::new(false),
        }
    }

    // -----------------------------------------------------------------------
    // What callers ask
    // -----------------------------------------------------------------------

    /// Marks `keys` as wanted now: the poller refreshes them for the next
    /// 15 s, and the close sweep includes them for 15 minutes. A key with
    /// no REST form is still answered from the feed or the close. Called by
    /// the search, quote and instrument routes for every key they answer.
    /// At most [`MAX_WANTED`] keys are kept; past that the least recently
    /// asked for go first.
    pub fn want<I: IntoIterator<Item = String>>(&self, keys: I) {
        self.want_at(keys, Instant::now());
    }

    fn want_at<I: IntoIterator<Item = String>>(&self, keys: I, now: Instant) {
        let mut wanted = self.wanted.lock().unwrap_or_else(|e| e.into_inner());
        for key in keys {
            wanted.insert(key, now);
        }
        if wanted.len() > MAX_WANTED {
            let mut by_age: Vec<(Instant, String)> = wanted.iter().map(|(k, at)| (*at, k.clone())).collect();
            by_age.sort();
            for (_, key) in by_age.into_iter().take(wanted.len() - MAX_WANTED) {
                wanted.remove(&key);
            }
        }
    }

    /// Keys asked for within `window` of `now`, no particular order.
    fn wanted_within(&self, window: Duration, now: Instant) -> Vec<String> {
        let wanted = self.wanted.lock().unwrap_or_else(|e| e.into_inner());
        wanted
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) < window)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Keys asked for in the last [`WATCH_WINDOW`].
    pub fn watched_keys(&self) -> Vec<String> {
        self.wanted_within(WATCH_WINDOW, Instant::now())
    }

    /// The quote for `key` as of `now` (the routes pass the clock through,
    /// so their tests can set it) — see the module doc for which source
    /// answers. A contract's feed tick counts only once the key has a REST
    /// read as well: the feed carries a contract because the owner's
    /// engine, the ladder or a paper desk subscribed it, and a quote
    /// answered before the poller's first read would say so. Equities and
    /// indices are answered from the feed at once (the indices are always
    /// on it; no equity ever is).
    pub fn get_at(&self, key: &str, now: DateTime<FixedOffset>) -> Option<Quote> {
        let rest = self.rest.get(key).map(|h| h.quote.clone());
        let upstox = self.upstox.get(key).map(|h| h.quote.clone());
        let feed = if is_contract_key(key) && rest.is_none() { None } else { self.feed_quote(key) };
        let live = merge_live(feed, rest, upstox, now.timestamp_millis());
        let close = self.snapshot().and_then(|s| s.quote(key).map(|q| (s.day, q)));
        serve(live, close, now)
    }

    /// The feed's price for a key it carries, stamped with its last tick;
    /// an index's with the close the feed sends beside it.
    fn feed_quote(&self, key: &str) -> Option<Quote> {
        let ltp = positive(self.prices.get(key).map(|p| *p))?;
        let tick = (self.feed_tick)(key)?;
        let at = (tick * 1000.0) as i64;
        let close = key
            .strip_prefix("idx|")
            .and_then(|name| self.prices.get(&index_close_key(name)).map(|c| *c))
            .filter(|c| *c > 0.0);
        let (change, per_change) = change_against(ltp, close);
        Some(Quote { close, change, per_change, ..Quote::bare(key, ltp, at, Source::Feed) })
    }

    pub fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.snapshot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(snapshot));
    }

    pub fn status(&self) -> HubStatus {
        // One lock at a time, so this can never wait on the poller while
        // it waits on this (`plan` holds the sweep while it takes the
        // scheduler).
        let now = Instant::now();
        let wanted = self.wanted_within(rest::WANT_WINDOW, now).len();
        let session_ok = *self.session_ok.lock().unwrap_or_else(|e| e.into_inner());
        let switched_off = *self.switched_off.lock().unwrap_or_else(|e| e.into_inner());
        let (batch_size, quarantined, sched_paused) = {
            let sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
            (sched.batch_size(), sched.quarantined(), sched.paused(now).map(str::to_string))
        };
        let paused = if switched_off {
            Some(PAUSED_SWITCHED_OFF.to_string())
        } else if session_ok {
            sched_paused
        } else {
            Some("no healthy Kotak session".to_string())
        };
        let sweep = self
            .sweep
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| SweepStatus { day: s.day.to_string(), total: s.total, done: s.rows.len() });
        let close = self.snapshot().map(|s| CloseStatus {
            day: s.day.to_string(),
            taken_at: s.taken_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            rows: s.rows.len(),
            asked: s.asked,
            partial: s.partial(),
        });
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        Stats::prune(&mut stats.recent_calls, now);
        Stats::prune(&mut stats.recent_upstox_calls, now);
        let calls_last_minute = stats.recent_calls.len() as u64;
        HubStatus {
            wanted,
            quotes: self.rest.len(),
            upstox_quotes: self.upstox.len(),
            batch_size,
            quarantined,
            calls: stats.calls,
            refused: stats.refused,
            rate_limited: stats.rate_limited,
            errors: stats.errors,
            unmatched: stats.unmatched,
            upstox_calls: stats.upstox_calls,
            upstox_errors: stats.upstox_errors,
            calls_last_minute,
            calls_per_sec: calls_last_minute as f64 / RATE_WINDOW.as_secs_f64(),
            upstox_calls_last_minute: stats.recent_upstox_calls.len() as u64,
            last_call_at: stats.last_call_at.clone(),
            last_error: stats.last_error.clone(),
            paused,
            sweep,
            close,
        }
    }

    // -----------------------------------------------------------------------
    // The poller's side
    // -----------------------------------------------------------------------

    /// What the poller found the `search` switch to be, for the status.
    pub(super) fn set_switched_off(&self, off: bool) {
        *self.switched_off.lock().unwrap_or_else(|e| e.into_inner()) = off;
    }

    /// Charges the 2-calls-a-second bucket for a call sent at `now` — see
    /// `Scheduler::reserve`.
    pub(super) fn reserve_call(&self, now: Instant) -> Result<(), Duration> {
        self.sched.lock().unwrap_or_else(|e| e.into_inner()).reserve(now)
    }

    /// The calls to make now: the wanted keys with a REST form, those the
    /// feed carries last (their price is live anyway), and the sweep's.
    pub(super) fn plan(&self, session_ok: bool, now: Instant) -> Vec<Chunk> {
        *self.session_ok.lock().unwrap_or_else(|e| e.into_inner()) = session_ok;
        let mut wanted: Vec<String> = self
            .wanted_within(rest::WANT_WINDOW, now)
            .into_iter()
            .filter(|k| rest::rest_key(k).is_some())
            .collect();
        wanted.sort();
        let (on_feed, off_feed): (Vec<String>, Vec<String>) = wanted.into_iter().partition(|k| self.feed_quote(k).is_some());
        let wanted: Vec<String> = off_feed.into_iter().chain(on_feed).collect();

        let mut sweep = self.sweep.lock().unwrap_or_else(|e| e.into_inner());
        let mut none = VecDeque::new();
        let queue = match sweep.as_mut() {
            Some(s) if now < s.deadline => &mut s.pending,
            _ => &mut none,
        };
        let chunks = self.sched.lock().unwrap_or_else(|e| e.into_inner()).plan(&wanted, queue, session_ok, now);
        if let Some(s) = sweep.as_mut() {
            s.in_flight += chunks.iter().filter(|c| c.sweep).count();
        }
        drop(sweep);
        self.evict(now);
        chunks
    }

    /// `(REST key, price-map key)` for each of a chunk's keys.
    pub(super) fn rest_keys(chunk: &Chunk) -> Vec<(String, String)> {
        chunk.keys.iter().filter_map(|k| rest::rest_key(k).map(|r| (r, k.clone()))).collect()
    }

    /// What `chunk`'s call came to: its quotes stored (and kept for the
    /// close when the sweep sent it), its outcome fed to the scheduler.
    pub(super) fn settle_kotak(&self, chunk: Chunk, keys: &[(String, String)], result: Result<Vec<KotakQuote>, KotakError>, now: Instant) {
        let outcome = match &result {
            Ok(_) => Outcome::Ok,
            Err(e) => rest::classify(e),
        };
        let at = now_ms();
        let mut unmatched = 0u64;
        let mut rows: Vec<eod::Row> = Vec::new();
        if let Ok(quotes) = &result {
            let by_rest: HashMap<String, &String> = keys.iter().map(|(r, w)| (r.to_lowercase(), w)).collect();
            for q in quotes {
                let Some(ws_key) = by_rest.get(&q.key().to_lowercase()) else {
                    unmatched += 1;
                    continue;
                };
                if let Some(quote) = from_kotak(ws_key, q, at) {
                    if chunk.sweep {
                        rows.push(eod::Row::of(&quote));
                    }
                    self.rest.insert((*ws_key).clone(), Held { quote, seen: now });
                }
            }
        }

        let quarantined = {
            let mut sweep = self.sweep.lock().unwrap_or_else(|e| e.into_inner());
            let mut none = VecDeque::new();
            let queue = match sweep.as_mut() {
                Some(s) => {
                    if chunk.sweep {
                        s.in_flight = s.in_flight.saturating_sub(1);
                        for row in rows {
                            s.rows.insert(row.key.clone(), row);
                        }
                    }
                    &mut s.pending
                }
                None => &mut none,
            };
            self.sched.lock().unwrap_or_else(|e| e.into_inner()).settle(&chunk, outcome, queue, now)
        };

        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.calls += 1;
        Stats::note(&mut stats.recent_calls, now);
        stats.unmatched += unmatched;
        if unmatched > 0 {
            tracing::debug!(unmatched, asked = keys.len(), "Kotak quotes: answers for keys that were not asked");
        }
        match (&result, outcome) {
            (Ok(_), _) => {
                stats.error_streak = 0;
                stats.last_call_at = Some(shared_domain::now_ist().format("%H:%M:%S").to_string());
            }
            (Err(e), outcome) => {
                match outcome {
                    Outcome::RateLimited => stats.rate_limited += 1,
                    Outcome::Refused => stats.refused += 1,
                    _ => stats.errors += 1,
                }
                stats.error_streak += 1;
                let message = format!("{} ({})", e, shared_domain::now_ist().format("%H:%M:%S"));
                stats.last_error = Some(message);
                let keys_sent = chunk.keys.len();
                if stats.error_streak == 1 {
                    tracing::warn!(error = %e, ?outcome, keys = keys_sent, sweep = chunk.sweep, "Kotak quotes call failed");
                } else {
                    tracing::debug!(error = %e, ?outcome, keys = keys_sent, streak = stats.error_streak, "Kotak quotes call failed");
                }
                if let Some(key) = quarantined {
                    tracing::warn!(key, error = %e, "Kotak quotes: key refused on its own — left out for ten minutes");
                }
            }
        }
    }

    /// One Upstox quotes call for the wanted keys the catalog has an Upstox
    /// key for, during hours: the fallback while Kotak is down. Never
    /// called while the Kotak session is healthy.
    pub(super) async fn poll_upstox(&self, client: &UpstoxClient) {
        if !eod::open_at(shared_domain::now_ist()) {
            return;
        }
        let now = Instant::now();
        let keys: Vec<(String, String)> = {
            let mut state = self.upstox_state.lock().unwrap_or_else(|e| e.into_inner());
            if state.backoff.active(now) {
                return;
            }
            let mut wanted = self.wanted_within(rest::WANT_WINDOW, now);
            wanted.sort();
            let keys: Vec<(String, String)> = wanted
                .into_iter()
                .filter(|k| state.last_sent.get(k).is_none_or(|at| now.saturating_duration_since(*at) >= rest::POLL_EVERY))
                .filter_map(|k| (self.upstox_key)(&k).map(|u| (u, k)))
                .take(UPSTOX_BATCH)
                .collect();
            for (_, k) in &keys {
                state.last_sent.insert(k.clone(), now);
            }
            if state.last_sent.len() > 2_000 {
                state.last_sent.retain(|_, at| now.saturating_duration_since(*at) < WATCH_WINDOW);
            }
            keys
        };
        if keys.is_empty() {
            return;
        }
        let asked: Vec<String> = keys.iter().map(|(u, _)| u.clone()).collect();
        let result = client.quotes(&asked).await;
        let mut state = self.upstox_state.lock().unwrap_or_else(|e| e.into_inner());
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        Stats::note(&mut stats.recent_upstox_calls, Instant::now());
        match result {
            Ok(quotes) => {
                let at = now_ms();
                let seen = Instant::now();
                for (upstox_key, ws_key) in &keys {
                    if let Some(quote) = quotes.get(upstox_key).and_then(|q| from_upstox(ws_key, q, at)) {
                        self.upstox.insert(ws_key.clone(), Held { quote, seen });
                    }
                }
                state.backoff.clear();
                stats.upstox_calls += 1;
            }
            Err(e) => {
                let pause = state.backoff.hit(Instant::now());
                stats.upstox_errors += 1;
                if state.backoff.hits == 1 {
                    tracing::warn!(error = %e, keys = keys.len(), pause_secs = pause.as_secs(), "Upstox quotes (the fallback while Kotak is down) failed");
                } else {
                    tracing::debug!(error = %e, keys = keys.len(), pause_secs = pause.as_secs(), "Upstox quotes failed");
                }
            }
        }
    }

    /// Drops quotes nobody refreshed for [`EVICT_AFTER`], once a minute.
    fn evict(&self, now: Instant) {
        let mut last = self.last_evicted.lock().unwrap_or_else(|e| e.into_inner());
        if now.saturating_duration_since(*last) < EVICT_EVERY {
            return;
        }
        *last = now;
        self.rest.retain(|_, h| now.saturating_duration_since(h.seen) < EVICT_AFTER);
        self.upstox.retain(|_, h| now.saturating_duration_since(h.seen) < EVICT_AFTER);
    }

    // -----------------------------------------------------------------------
    // The close sweep's side
    // -----------------------------------------------------------------------

    /// Starts the close sweep over `keys` (those with a REST form), to end
    /// by `deadline`; a sweep already running is replaced. Returns how many
    /// keys it will ask for.
    pub fn start_sweep(&self, keys: BTreeSet<String>, day: NaiveDate, started: DateTime<FixedOffset>, deadline: Instant) -> usize {
        let pending: VecDeque<String> = keys.into_iter().filter(|k| rest::rest_key(k).is_some()).collect();
        let total = pending.len();
        *self.sweep.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Sweep { day, started, deadline, pending, in_flight: 0, total, rows: HashMap::new() });
        total
    }

    /// The sweep, once every key was asked for or its deadline passed.
    pub fn take_sweep_if_done(&self, now: Instant) -> Option<Sweep> {
        let mut sweep = self.sweep.lock().unwrap_or_else(|e| e.into_inner());
        if sweep.as_ref().is_some_and(|s| s.done(now)) {
            sweep.take()
        } else {
            None
        }
    }

    /// For tests: a REST quote as the poller would store it.
    #[cfg(test)]
    pub(crate) fn put_rest(&self, quote: Quote) {
        self.rest.insert(quote.key.clone(), Held { quote, seen: Instant::now() });
    }

    #[cfg(test)]
    pub(crate) fn put_upstox(&self, quote: Quote) {
        self.upstox.insert(quote.key.clone(), Held { quote, seen: Instant::now() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn ist(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .single()
            .unwrap()
    }

    fn ms(t: DateTime<FixedOffset>) -> i64 {
        t.timestamp_millis()
    }

    /// A hub whose feed tick time for every key is the shared `tick_ms`.
    fn hub(prices: Arc<DashMap<String, f64>>, tick_ms: Arc<AtomicI64>) -> QuoteHub {
        let tick = Arc::clone(&tick_ms);
        QuoteHub::with_feed(
            prices,
            Box::new(|key: &str| (key == "nse_cm|2885").then(|| "NSE_EQ|INE002A01018".to_string())),
            Box::new(move |_key: &str| {
                let t = tick.load(Ordering::Relaxed);
                (t > 0).then(|| t as f64 / 1000.0)
            }),
        )
    }

    fn rest_quote(key: &str, ltp: f64, at: DateTime<FixedOffset>) -> Quote {
        Quote {
            open: Some(100.0),
            high: Some(110.0),
            low: Some(95.0),
            close: Some(98.0),
            change: Some(ltp - 98.0),
            per_change: Some((ltp - 98.0) / 98.0 * 100.0),
            volume: Some(1_000.0),
            ..Quote::bare(key, ltp, ms(at), Source::Rest)
        }
    }

    #[test]
    fn a_fresh_feed_tick_wins_over_rest_and_rest_fills_in_the_day() {
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        let tick_ms = Arc::new(AtomicI64::new(0));
        let hub = hub(Arc::clone(&prices), Arc::clone(&tick_ms));
        let noon = ist(2026, 11, 4, 12, 0);

        // REST alone.
        hub.put_rest(rest_quote("nse_fo|1", 104.0, noon - chrono::Duration::seconds(1)));
        let q = hub.get_at("nse_fo|1", noon).unwrap();
        assert_eq!((q.ltp, q.source), (104.0, Source::Rest));

        // The feed ticks 3 s after that read: its price, REST's day, and the
        // change worked out for the feed's price, not copied for REST's.
        prices.insert("nse_fo|1".into(), 105.5);
        tick_ms.store(ms(noon) + 2_000, Ordering::Relaxed);
        let q = hub.get_at("nse_fo|1", noon + chrono::Duration::seconds(3)).unwrap();
        assert_eq!((q.ltp, q.source), (105.5, Source::Feed));
        assert_eq!((q.open, q.high, q.low, q.close), (Some(100.0), Some(110.0), Some(95.0), Some(98.0)));
        assert_eq!(q.volume, Some(1_000.0));
        assert_eq!(q.at, ms(noon) + 2_000);
        assert_eq!(q.change, Some(7.5));
        assert!((q.per_change.unwrap() - 7.5 / 98.0 * 100.0).abs() < 1e-9);
        // Serialised, the feed and REST both say `live`, and every figure is
        // present, `null` when absent.
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["source"], "live");
        assert!(json["oi"].is_null() && json["as_of"].is_null() && json.get("year_high").is_some());
        assert_eq!(serde_json::to_value(Source::Rest).unwrap(), "live");
        assert_eq!(serde_json::to_value(Source::Upstox).unwrap(), "upstox");
        assert_eq!(serde_json::to_value(Source::Close).unwrap(), "close");

        // A REST read later than the tick, with the tick still within 10 s:
        // the feed still gives the price.
        hub.put_rest(rest_quote("nse_fo|1", 104.5, noon + chrono::Duration::seconds(5)));
        let q = hub.get_at("nse_fo|1", noon + chrono::Duration::seconds(6)).unwrap();
        assert_eq!((q.ltp, q.source), (105.5, Source::Feed));
        // The tick 30 s old and REST newer: REST's price.
        let q = hub.get_at("nse_fo|1", noon + chrono::Duration::seconds(32)).unwrap();
        assert_eq!((q.ltp, q.source), (104.5, Source::Rest));
        // The feed's 0.0 placeholder is no price.
        prices.insert("nse_fo|2".into(), 0.0);
        assert_eq!(hub.get_at("nse_fo|2", noon), None);
        // A contract the feed carries but REST has not read yet is not
        // answered: that it is on the feed is the owner's business.
        prices.insert("nse_fo|3".into(), 42.0);
        assert_eq!(hub.get_at("nse_fo|3", noon), None, "a held contract looks like any other until polled");
        hub.put_rest(Quote::bare("nse_fo|3", 41.5, ms(noon) - 500, Source::Rest));
        let q = hub.get_at("nse_fo|3", noon).unwrap();
        assert_eq!((q.ltp, q.source), (42.0, Source::Feed), "once polled, the fresher tick gives the price");
        // Indices are on the feed for everyone: answered at once.
        prices.insert("idx|SENSEX".into(), 80_000.0);
        assert_eq!(hub.get_at("idx|SENSEX", noon).map(|q| q.ltp), Some(80_000.0));
        // An index carries its close from the feed.
        prices.insert("idx|NIFTY 50".into(), 25_100.0);
        prices.insert("idx_close|NIFTY 50".into(), 25_000.0);
        let q = hub.get_at("idx|NIFTY 50", noon).unwrap();
        assert_eq!((q.close, q.change), (Some(25_000.0), Some(100.0)));
        assert!((q.per_change.unwrap() - 0.4).abs() < 1e-9);

        // Upstox stands in only when it is the freshest there is.
        hub.put_upstox(Quote::bare("nse_cm|2885", 1_400.0, ms(noon), Source::Upstox));
        let q = hub.get_at("nse_cm|2885", noon).unwrap();
        assert_eq!((q.ltp, q.source), (1_400.0, Source::Upstox));
        hub.put_rest(rest_quote("nse_cm|2885", 1_401.0, noon + chrono::Duration::seconds(1)));
        let q = hub.get_at("nse_cm|2885", noon + chrono::Duration::seconds(1)).unwrap();
        assert_eq!((q.ltp, q.source), (1_401.0, Source::Rest));
    }

    #[test]
    fn the_close_is_served_after_hours_and_live_quotes_during_them() {
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        let hub = hub(Arc::clone(&prices), Arc::new(AtomicI64::new(0)));
        let day = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let sweep_at = ist(2026, 11, 4, 15, 35);
        let mut rows = HashMap::new();
        rows.insert(
            "nse_cm|2885".to_string(),
            eod::Row { key: "nse_cm|2885".into(), ltp: 1_410.0, open: Some(1_400.0), high: Some(1_415.0), low: Some(1_395.0), close: Some(1_405.0), volume: Some(5e6) },
        );
        hub.set_snapshot(Snapshot { day, taken_at: sweep_at, rows, asked: 1 });

        // After 15:40 the close answers, even over a REST read from 15:39.
        hub.put_rest(rest_quote("nse_cm|2885", 1_411.0, ist(2026, 11, 4, 15, 39)));
        let q = hub.get_at("nse_cm|2885", ist(2026, 11, 4, 16, 0)).unwrap();
        assert_eq!((q.ltp, q.source), (1_410.0, Source::Close));
        assert_eq!(q.as_of.as_deref(), Some("2026-11-04 15:30"));
        assert_eq!((q.close, q.change), (Some(1_405.0), Some(5.0)));
        assert_eq!(q.at, ms(sweep_at));
        // The next morning before the login, and over the weekend: still the close.
        assert_eq!(hub.get_at("nse_cm|2885", ist(2026, 11, 5, 9, 0)).unwrap().source, Source::Close);
        assert_eq!(hub.get_at("nse_cm|2885", ist(2026, 11, 7, 12, 0)).unwrap().source, Source::Close);
        // During hours a live quote of the day wins.
        let q = hub.get_at("nse_cm|2885", ist(2026, 11, 4, 15, 39)).unwrap();
        assert_eq!((q.ltp, q.source), (1_411.0, Source::Rest));
        // The next day's REST read is newer than the close, open or not.
        hub.put_rest(rest_quote("nse_cm|2885", 1_420.0, ist(2026, 11, 5, 9, 10)));
        let q = hub.get_at("nse_cm|2885", ist(2026, 11, 5, 9, 12)).unwrap();
        assert_eq!((q.ltp, q.source), (1_420.0, Source::Rest));
        // A key the sweep never saw has no close; a key only the close knows is answered.
        assert_eq!(hub.get_at("nse_cm|9999", ist(2026, 11, 4, 16, 0)), None);
        hub.put_rest(rest_quote("nse_cm|9999", 5.0, ist(2026, 11, 4, 15, 0)));
        assert_eq!(hub.get_at("nse_cm|9999", ist(2026, 11, 4, 16, 0)).unwrap().source, Source::Rest);
        let status = hub.status();
        assert_eq!(
            status.close,
            Some(CloseStatus { day: "2026-11-04".into(), taken_at: "2026-11-04 15:35:00".into(), rows: 1, asked: 1, partial: false })
        );
        assert_eq!(status.paused.as_deref(), Some("no healthy Kotak session"));
        // While the switch is off the status says so first.
        hub.set_switched_off(true);
        assert_eq!(hub.status().paused.as_deref(), Some(PAUSED_SWITCHED_OFF));
        hub.set_switched_off(false);
    }

    #[test]
    fn interest_is_capped_at_600_keys_oldest_first() {
        let hub = hub(Arc::new(DashMap::new()), Arc::new(AtomicI64::new(0)));
        let t0 = Instant::now();
        hub.want_at((0..600).map(|i| format!("nse_cm|{i}")), t0);
        hub.want_at(["nse_fo|new".to_string()], t0 + Duration::from_secs(1));
        let watched: std::collections::HashSet<String> = hub.wanted_within(WATCH_WINDOW, t0 + Duration::from_secs(2)).into_iter().collect();
        assert_eq!(watched.len(), MAX_WANTED);
        assert!(watched.contains("nse_fo|new"));
        assert!(!watched.contains("nse_cm|0") && watched.contains("nse_cm|1"), "one of the oldest went, not the newest");
        // Asking again refreshes a key's place.
        hub.want_at(["nse_cm|1".to_string()], t0 + Duration::from_secs(3));
        hub.want_at((0..60).map(|i| format!("bse_fo|{i}")), t0 + Duration::from_secs(4));
        let watched: std::collections::HashSet<String> = hub.wanted_within(WATCH_WINDOW, t0 + Duration::from_secs(5)).into_iter().collect();
        assert_eq!(watched.len(), MAX_WANTED);
        assert!(watched.contains("nse_cm|1") && watched.contains("nse_fo|new") && watched.contains("bse_fo|59"));
    }

    #[test]
    fn kotak_and_upstox_quotes_become_hub_quotes() {
        let k = KotakQuote {
            exchange_token: "2885".into(),
            exchange: "nse_cm".into(),
            ltp: Some(1_410.5),
            change: Some(5.5),
            per_change: Some(0.39),
            last_volume: Some(12.0),
            year_high: Some(0.0),
            ohlc: kotak_client::KotakOhlc { open: Some(1_400.0), high: Some(1_415.0), low: Some(0.0), close: Some(1_405.0) },
            ..Default::default()
        };
        let q = from_kotak("nse_cm|2885", &k, 1).unwrap();
        assert_eq!((q.ltp, q.open, q.high, q.low, q.close), (1_410.5, Some(1_400.0), Some(1_415.0), None, Some(1_405.0)));
        assert_eq!((q.change, q.per_change, q.volume, q.year_high), (Some(5.5), Some(0.39), Some(12.0), None));
        assert_eq!(q.source, Source::Rest);
        // Change is worked out from the close when Kotak sends none; no price, no quote.
        let k2 = KotakQuote { ltp: Some(100.0), ohlc: kotak_client::KotakOhlc { close: Some(80.0), ..Default::default() }, ..Default::default() };
        let q = from_kotak("x", &k2, 1).unwrap();
        assert_eq!((q.change, q.per_change), (Some(20.0), Some(25.0)));
        assert_eq!(from_kotak("x", &KotakQuote { ltp: Some(0.0), ..Default::default() }, 1), None);
        assert_eq!(from_kotak("x", &KotakQuote::default(), 1), None);

        let u: upstox_client::Quote = serde_json::from_str(
            r#"{"instrument_token": "NSE_EQ|INE002A01018", "last_price": 1410.5, "volume": 500, "oi": 0,
                "ohlc": {"open": 1400, "high": 1415, "low": 1395, "close": 1405}, "net_change": 5.5}"#,
        )
        .unwrap();
        let q = from_upstox("nse_cm|2885", &u, 2).unwrap();
        assert_eq!((q.ltp, q.open, q.close, q.change, q.volume, q.oi), (1_410.5, Some(1_400.0), Some(1_405.0), Some(5.5), Some(500.0), None));
        assert_eq!(q.source, Source::Upstox);
        assert!((q.per_change.unwrap() - 5.5 / 1_405.0 * 100.0).abs() < 1e-9);
    }

    #[test]
    fn the_plan_takes_wanted_keys_with_a_rest_form_feed_ones_last() {
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        let tick_ms = Arc::new(AtomicI64::new(1));
        let hub = hub(Arc::clone(&prices), tick_ms);
        prices.insert("nse_fo|1".into(), 100.0);
        hub.want(["nse_fo|1".to_string(), "nse_cm|2885".to_string(), "idx|NIFTY 50".to_string(), "idx_close|NIFTY 50".to_string(), "junk".to_string()]);
        let now = Instant::now();
        let chunks = hub.plan(true, now);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].keys, vec!["idx|NIFTY 50", "nse_cm|2885", "nse_fo|1"]);
        assert_eq!(
            QuoteHub::rest_keys(&chunks[0]),
            vec![
                ("nse_cm|Nifty 50".to_string(), "idx|NIFTY 50".to_string()),
                ("nse_cm|2885".to_string(), "nse_cm|2885".to_string()),
                ("nse_fo|1".to_string(), "nse_fo|1".to_string()),
            ]
        );
        // A good answer is stored under the price-map key, matched by the REST key, case aside.
        let answer = vec![
            KotakQuote { exchange_token: "NIFTY 50".into(), exchange: "NSE_CM".into(), ltp: Some(25_000.0), ..Default::default() },
            KotakQuote { exchange_token: "2885".into(), exchange: "nse_cm".into(), ltp: Some(1_400.0), ..Default::default() },
            KotakQuote { exchange_token: "7".into(), exchange: "nse_cm".into(), ltp: Some(1.0), ..Default::default() },
        ];
        let keys = QuoteHub::rest_keys(&chunks[0]);
        hub.settle_kotak(chunks[0].clone(), &keys, Ok(answer), now);
        let status = hub.status();
        assert_eq!((status.calls, status.quotes, status.unmatched, status.wanted), (1, 2, 1, 5));
        assert!(status.paused.is_none());
        // The rate counts the last minute's calls, whatever came of them.
        assert_eq!((status.calls_last_minute, status.upstox_calls_last_minute), (1, 0));
        assert!((status.calls_per_sec - 1.0 / 60.0).abs() < 1e-9);
        let noon = ist(2026, 11, 4, 12, 0);
        assert_eq!(hub.get_at("nse_cm|2885", noon).unwrap().ltp, 1_400.0);
        // The poller charges the bucket as it sends: the second's two calls.
        assert!(hub.reserve_call(now).is_ok());
        assert!(hub.reserve_call(now).is_ok());
        assert!(hub.reserve_call(now + Duration::from_millis(100)).is_err());
        // Nothing is sent while the session is down, and the status says so.
        assert!(hub.plan(false, now + Duration::from_secs(3)).is_empty());
        assert_eq!(hub.status().paused.as_deref(), Some("no healthy Kotak session"));

        // The sweep: its keys go out, its answers are kept as rows, and it is done when all were asked.
        let day = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let keys: BTreeSet<String> = ["nse_cm|2885", "nse_cm|11536", "idx_close|NIFTY 50"].iter().map(|s| s.to_string()).collect();
        let later = now + Duration::from_secs(10);
        assert_eq!(hub.start_sweep(keys, day, noon, later + Duration::from_secs(60)), 2);
        assert!(hub.take_sweep_if_done(later).is_none());
        let chunks = hub.plan(true, later);
        let sweep_chunk = chunks.iter().find(|c| c.sweep).unwrap().clone();
        assert_eq!(sweep_chunk.keys, vec!["nse_cm|11536", "nse_cm|2885"]);
        assert!(hub.take_sweep_if_done(later).is_none(), "a chunk is in flight");
        let keys = QuoteHub::rest_keys(&sweep_chunk);
        hub.settle_kotak(
            sweep_chunk,
            &keys,
            Ok(vec![KotakQuote { exchange_token: "11536".into(), exchange: "nse_cm".into(), ltp: Some(3_000.0), ..Default::default() }]),
            later,
        );
        let sweep = hub.take_sweep_if_done(later).unwrap();
        assert_eq!((sweep.total, sweep.rows.len(), sweep.cut_short()), (2, 1, false), "every key was asked; one got no answer");
        assert_eq!(sweep.rows["nse_cm|11536"].ltp, 3_000.0);
        // A sweep the deadline cuts short says so.
        let keys: BTreeSet<String> = (1..=60).map(|i| format!("nse_cm|{i}")).collect();
        let t = later + Duration::from_secs(5);
        hub.start_sweep(keys, day, noon, t + Duration::from_secs(1));
        let chunks = hub.plan(true, t);
        assert_eq!(chunks.iter().filter(|c| c.sweep).count(), 2, "two calls of 25, ten keys still pending");
        assert!(hub.take_sweep_if_done(t).is_none());
        let cut = hub.take_sweep_if_done(t + Duration::from_secs(2)).unwrap();
        assert!(cut.cut_short());
        assert_eq!((cut.total, cut.rows.len()), (60, 0));
        // A refused sweep chunk with one key is dropped from the sweep.
        let keys: BTreeSet<String> = ["nse_cm|1".to_string()].into_iter().collect();
        let t = t + Duration::from_secs(5);
        hub.start_sweep(keys, day, noon, t + Duration::from_secs(60));
        let chunk = hub.plan(true, t).into_iter().find(|c| c.sweep).unwrap();
        let keys = QuoteHub::rest_keys(&chunk);
        hub.settle_kotak(chunk, &keys, Err(KotakError::ApiError { status_code: 1009, message: "Invalid instrument/code".into() }), t);
        assert!(hub.take_sweep_if_done(t).unwrap().rows.is_empty());
        let status = hub.status();
        assert_eq!((status.refused, status.quarantined), (1, 1));
        assert!(status.last_error.as_deref().unwrap().contains("Invalid instrument/code"));
        assert_eq!(status.calls_last_minute, 3);
    }

    #[test]
    fn only_derivative_keys_wait_for_a_rest_read() {
        assert!(is_contract_key("nse_fo|51386"));
        assert!(is_contract_key("bse_fo|874001"));
        assert!(!is_contract_key("nse_cm|2885"));
        assert!(!is_contract_key("idx|NIFTY 50"));
        assert!(!is_contract_key("junk"));
    }

    #[test]
    fn the_request_rate_forgets_calls_older_than_a_minute() {
        let mut stats = Stats::default();
        let start = Instant::now();
        Stats::note(&mut stats.recent_calls, start);
        Stats::note(&mut stats.recent_calls, start + Duration::from_secs(30));
        Stats::note(&mut stats.recent_calls, start + Duration::from_secs(59));
        assert_eq!(stats.recent_calls.len(), 3);
        Stats::prune(&mut stats.recent_calls, start + Duration::from_secs(60));
        assert_eq!(stats.recent_calls.len(), 2, "the call a minute ago is out");
        Stats::note(&mut stats.recent_calls, start + Duration::from_secs(120));
        assert_eq!(stats.recent_calls.len(), 1);
    }
}
