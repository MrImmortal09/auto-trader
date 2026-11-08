//! Feed latency, for the admin: how late prices reach this server on each
//! feed, live (the last minute) and on average over the day.
//!
//! - Upstox: each tick carries the exchange's time of its trade in ms, so its
//!   delay is our receive time less that.
//! - Kotak: its ticks carry whole seconds only, so it is measured against the
//!   same trade on Upstox: the first price change both feeds report, paired
//!   per instrument (`stats::Matcher`) — Kotak's delay against the trade, and
//!   how much sooner (or later) it had it than Upstox.
//!
//! Watched: the six indices, each one's nearest future, and every contract the
//! owner holds. Market hours only (special sessions included).
//!
//! Analytics only. It reads Kotak's tick tap (which never makes the feed
//! wait), asks the bridge to subscribe the indices and futures, and holds one
//! read-only Upstox stream; it never touches orders or positions.

pub mod stats;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use chrono::NaiveDate;
use serde::Serialize;
use shared_domain::{MonitoredPosition, TradeState, INDEX_NAMES};
use tokio::sync::{broadcast, mpsc, watch, Mutex, RwLock};
use upstox_client::{LtpTick, UpstoxClient};

use crate::candles::symbols::upstox_index_key;
use stats::{Matcher, Sample, Series, Summary};

/// How often the watch set and the Kotak subscriptions are brought up to date.
const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// How often a closed market is looked at again.
const CLOSED_CHECK: Duration = Duration::from_secs(15);
/// A failed futures lookup is tried again after this long.
const FUTURES_RETRY: Duration = Duration::from_secs(60);
const STREAM_BACKOFF_START: Duration = Duration::from_secs(5);
const STREAM_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Upstox's instruments file per exchange, and the indices it lists.
const INSTRUMENT_FILES: [(&str, &[&str]); 2] = [
    (upstox_client::NSE_INSTRUMENTS_URL, &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"]),
    (upstox_client::BSE_INSTRUMENTS_URL, &["SENSEX", "BANKEX"]),
];

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Index,
    Future,
    Position,
}

const CLASSES: [Class; 3] = [Class::Index, Class::Future, Class::Position];

impl Class {
    fn label(self) -> &'static str {
        match self {
            Class::Index => "Indices",
            Class::Future => "Near futures",
            Class::Position => "Held contracts",
        }
    }
}

/// One instrument to watch, as both feeds name it.
#[derive(Debug, Clone, PartialEq)]
struct Watch {
    label: String,
    class: Class,
    upstox_key: String,
    kotak_key: String,
    /// What the bridge is sent to stream it; none for a held contract, which
    /// the engine already streams.
    kotak_subscribe: Option<String>,
}

/// Upstox's key for a Kotak F&O key: both use the exchange token
/// (`nse_fo|61471` is `NSE_FO|61471`).
fn upstox_key_of(kotak_key: &str) -> Option<String> {
    let (segment, token) = kotak_key.split_once('|')?;
    let segment = match segment {
        "nse_fo" => "NSE_FO",
        "bse_fo" => "BSE_FO",
        _ => return None,
    };
    (!token.is_empty()).then(|| format!("{segment}|{token}"))
}

fn kotak_key_of(upstox_key: &str) -> Option<String> {
    let (segment, token) = upstox_key.split_once('|')?;
    let segment = match segment {
        "NSE_FO" => "nse_fo",
        "BSE_FO" => "bse_fo",
        _ => return None,
    };
    (!token.is_empty()).then(|| format!("{segment}|{token}"))
}

/// "25000", "12437.5".
fn strike_text(strike: f64) -> String {
    if strike.fract() == 0.0 {
        format!("{strike:.0}")
    } else {
        format!("{strike}")
    }
}

fn index_watches() -> Vec<Watch> {
    INDEX_NAMES
        .iter()
        .filter_map(|index| {
            let upstox_key = upstox_index_key(index)?.to_string();
            let (segment, feed_name) = shared_domain::kotak_index_feed(index)?;
            let msg = serde_json::json!({ "action": "subscribe_indices", "indices": format!("{segment}|{feed_name}") });
            Some(Watch {
                label: index.to_string(),
                class: Class::Index,
                upstox_key,
                kotak_key: shared_domain::index_price_key(feed_name),
                kotak_subscribe: Some(msg.to_string()),
            })
        })
        .collect()
}

fn future_watch(index: &str, expiry: NaiveDate, upstox_key: &str) -> Option<Watch> {
    let kotak_key = kotak_key_of(upstox_key)?;
    // `ladder`: the bridge drops these at the day's close.
    let msg = serde_json::json!({ "action": "subscribe", "scrips": kotak_key, "ladder": true });
    Some(Watch {
        label: format!("{index} {} FUT", expiry.format("%b").to_string().to_uppercase()),
        class: Class::Future,
        upstox_key: upstox_key.to_string(),
        kotak_key,
        kotak_subscribe: Some(msg.to_string()),
    })
}

/// The owner's held contracts: open, with quantity, on F&O.
fn position_watches(positions: &[MonitoredPosition]) -> Vec<Watch> {
    let mut seen = HashSet::new();
    positions
        .iter()
        .filter(|p| matches!(p.state, TradeState::Active | TradeState::Target1Hit) && p.executed_qty > 0)
        .filter_map(|p| {
            let kotak_key = p.ws_scrip_key.clone()?;
            let upstox_key = upstox_key_of(&kotak_key)?;
            if !seen.insert(kotak_key.clone()) {
                return None;
            }
            let s = &p.signal;
            let label = [Some(s.instrument_name.clone()), s.strike.map(strike_text), s.option_type.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            Some(Watch { label, class: Class::Position, upstox_key, kotak_key, kotak_subscribe: None })
        })
        .collect()
}

/// The three measurements, in the order the panel shows them.
#[derive(Debug, Default)]
struct Measures {
    upstox_delay: Series,
    kotak_delay: Series,
    kotak_lead: Series,
}

impl Measures {
    fn add(&mut self, at_ms: i64, sample: Sample) {
        match sample {
            Sample::UpstoxDelay(v) => self.upstox_delay.add(at_ms, v),
            Sample::KotakDelay(v) => self.kotak_delay.add(at_ms, v),
            Sample::KotakLead(v) => self.kotak_lead.add(at_ms, v),
        }
    }

    fn minute(&mut self, now_ms: i64) -> Figures {
        Figures {
            upstox_delay: self.upstox_delay.minute(now_ms),
            kotak_delay: self.kotak_delay.minute(now_ms),
            kotak_lead: self.kotak_lead.minute(now_ms),
        }
    }

    fn day(&self) -> Figures {
        Figures { upstox_delay: self.upstox_delay.day(), kotak_delay: self.kotak_delay.day(), kotak_lead: self.kotak_lead.day() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Figures {
    pub upstox_delay: Option<Summary>,
    pub kotak_delay: Option<Summary>,
    pub kotak_lead: Option<Summary>,
}

struct Instrument {
    watch: Watch,
    matcher: Matcher,
    measures: Measures,
    last_upstox_ms: Option<i64>,
    last_kotak_ms: Option<i64>,
}

#[derive(Default)]
struct State {
    /// The day the figures are for.
    day: Option<NaiveDate>,
    streaming: bool,
    upstox_error: Option<String>,
    /// By Upstox key, in watch order.
    instruments: Vec<Instrument>,
    by_kotak: HashMap<String, usize>,
    by_upstox: HashMap<String, usize>,
    /// Every instrument of a class, the day's included after it is gone.
    classes: HashMap<Class, Measures>,
    kotak_lagged: u64,
}

impl State {
    fn reindex(&mut self) {
        self.by_kotak = self.instruments.iter().enumerate().map(|(i, x)| (x.watch.kotak_key.clone(), i)).collect();
        self.by_upstox = self.instruments.iter().enumerate().map(|(i, x)| (x.watch.upstox_key.clone(), i)).collect();
    }

    fn record(&mut self, i: usize, at_ms: i64, samples: Vec<Sample>) {
        let class = self.instruments[i].watch.class;
        for sample in samples {
            self.instruments[i].measures.add(at_ms, sample);
            self.classes.entry(class).or_default().add(at_ms, sample);
        }
    }
}

/// What `GET /api/admin/latency` answers.
#[derive(Debug, Serialize)]
pub struct LatencyAnswer {
    /// `not_configured` (no Upstox token), `market_closed` or `streaming`.
    pub status: &'static str,
    /// The IST day the figures are for, once there are any.
    pub day: Option<String>,
    /// The Upstox stream's last failure, until it streams again.
    pub upstox_error: Option<String>,
    /// Kotak's feed is connected and ticked in the last 10 s; without it there
    /// are no Kotak figures.
    pub kotak_streaming: bool,
    pub window_s: i64,
    pub classes: Vec<ClassOut>,
    pub instruments: Vec<InstrumentOut>,
    /// Kotak ticks this monitor fell behind on and skipped today.
    pub kotak_lagged: u64,
}

/// Kotak's feed is connected and ticked recently (what the candle service
/// counts as up).
fn kotak_streaming() -> bool {
    use shared_domain::telemetry;
    let last = telemetry::FEED_LAST_TICK.get();
    telemetry::FEED_CONNECTED.get() == 1 && last > 0.0 && telemetry::unix_now() - last <= 10.0
}

#[derive(Debug, Serialize)]
pub struct ClassOut {
    pub class: Class,
    pub label: &'static str,
    pub minute: Figures,
    pub day: Figures,
}

#[derive(Debug, Serialize)]
pub struct InstrumentOut {
    pub label: String,
    pub class: Class,
    pub upstox_key: String,
    pub kotak_key: String,
    /// How long ago each feed last ticked for it.
    pub upstox_age_ms: Option<i64>,
    pub kotak_age_ms: Option<i64>,
    pub minute: Figures,
    pub day: Figures,
}

pub struct FeedLatency {
    configured: bool,
    state: StdMutex<State>,
}

impl FeedLatency {
    pub fn new(configured: bool) -> Self {
        Self { configured, state: StdMutex::new(State::default()) }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new trading day starts from nothing.
    fn open_day(&self, day: NaiveDate) {
        let mut st = self.state();
        if st.day != Some(day) {
            *st = State { day: Some(day), ..State::default() };
        }
        st.streaming = true;
    }

    /// The day's figures stay to be read until the next day opens.
    fn close_day(&self) {
        self.state().streaming = false;
    }

    /// Watch exactly `watches`: new ones start empty, gone ones are dropped
    /// (their day stays in their class).
    fn set_watches(&self, watches: &[Watch]) {
        let mut st = self.state();
        if st.instruments.len() == watches.len() && st.instruments.iter().zip(watches).all(|(a, b)| a.watch == *b) {
            return;
        }
        let mut old: HashMap<String, Instrument> = st.instruments.drain(..).map(|x| (x.watch.upstox_key.clone(), x)).collect();
        st.instruments = watches
            .iter()
            .map(|w| match old.remove(&w.upstox_key) {
                Some(mut x) => {
                    x.watch = w.clone();
                    x
                }
                None => Instrument { watch: w.clone(), matcher: Matcher::default(), measures: Measures::default(), last_upstox_ms: None, last_kotak_ms: None },
            })
            .collect();
        st.reindex();
    }

    fn on_upstox(&self, tick: &LtpTick, recv_ms: i64) {
        let mut st = self.state();
        st.upstox_error = None;
        let Some(&i) = st.by_upstox.get(&tick.key) else { return };
        st.instruments[i].last_upstox_ms = Some(recv_ms);
        let samples = st.instruments[i].matcher.upstox(tick.ltp, tick.ltt_ms, recv_ms);
        st.record(i, recv_ms, samples);
    }

    fn on_kotak(&self, key: &str, ltp: f64, recv_ms: i64) {
        let mut st = self.state();
        let Some(&i) = st.by_kotak.get(key) else { return };
        st.instruments[i].last_kotak_ms = Some(recv_ms);
        let samples = st.instruments[i].matcher.kotak(ltp, recv_ms);
        st.record(i, recv_ms, samples);
    }

    fn upstox_failed(&self, error: String) {
        self.state().upstox_error = Some(error);
    }

    fn kotak_lagged(&self, n: u64) {
        self.state().kotak_lagged += n;
    }

    pub fn snapshot(&self) -> LatencyAnswer {
        let now = now_ms();
        let mut st = self.state();
        let status = if !self.configured {
            "not_configured"
        } else if st.streaming {
            "streaming"
        } else {
            "market_closed"
        };
        let classes = CLASSES
            .iter()
            .map(|&class| {
                let m = st.classes.entry(class).or_default();
                ClassOut { class, label: class.label(), minute: m.minute(now), day: m.day() }
            })
            .collect();
        let instruments = st
            .instruments
            .iter_mut()
            .map(|x| InstrumentOut {
                label: x.watch.label.clone(),
                class: x.watch.class,
                upstox_key: x.watch.upstox_key.clone(),
                kotak_key: x.watch.kotak_key.clone(),
                upstox_age_ms: x.last_upstox_ms.map(|t| now - t),
                kotak_age_ms: x.last_kotak_ms.map(|t| now - t),
                minute: x.measures.minute(now),
                day: x.measures.day(),
            })
            .collect();
        LatencyAnswer {
            status,
            day: st.day.map(|d| d.format("%Y-%m-%d").to_string()),
            upstox_error: st.upstox_error.clone(),
            kotak_streaming: kotak_streaming(),
            window_s: stats::WINDOW_MS / 1000,
            classes,
            instruments,
            kotak_lagged: st.kotak_lagged,
        }
    }
}

/// `GET /api/admin/latency` (admin only).
pub async fn latency_handler(axum::extract::State(state): axum::extract::State<crate::AppState>) -> axum::Json<LatencyAnswer> {
    axum::Json(state.feed_latency.snapshot())
}

/// Each index's nearest future still trading (one expiring today still
/// trades until the close), from Upstox's instruments files.
async fn near_futures(client: &UpstoxClient, today: NaiveDate) -> Result<Vec<Watch>, upstox_client::UpstoxError> {
    let mut out = Vec::new();
    for (url, wanted) in INSTRUMENT_FILES {
        let list = client.index_derivatives(url, wanted).await?;
        out.extend(nearest_futures(&list, wanted, today));
    }
    Ok(out)
}

fn nearest_futures(list: &[upstox_client::Derivative], wanted: &[&str], today: NaiveDate) -> Vec<Watch> {
    wanted
        .iter()
        .filter_map(|index| {
            let f = list
                .iter()
                .filter(|d| d.underlying == *index && d.kind == "FUT" && d.expiry >= today)
                .min_by_key(|d| d.expiry)?;
            future_watch(index, f.expiry, &f.instrument_key)
        })
        .collect()
}

/// Runs forever: in market hours, streams and measures; outside them, waits.
pub async fn run(
    me: Arc<FeedLatency>,
    client: UpstoxClient,
    positions: Arc<RwLock<Vec<MonitoredPosition>>>,
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
) {
    let mut futures: Option<(NaiveDate, Vec<Watch>)> = None;
    let mut futures_retry_at = tokio::time::Instant::now();
    loop {
        while !shared_domain::is_market_open() {
            tokio::time::sleep(CLOSED_CHECK).await;
        }
        let today = shared_domain::today_ist();
        me.open_day(today);
        tracing::info!("Feed latency: measuring");

        let (keys_tx, keys_rx) = watch::channel(BTreeSet::<String>::new());
        let (tick_tx, mut tick_rx) = mpsc::channel::<LtpTick>(4096);
        let stream = tokio::spawn(stream_with_backoff(client.clone(), keys_rx, tick_tx, Arc::clone(&me)));
        let mut kotak_rx = kotak_client::subscribe_ticks();
        let mut kotak_channel: Option<mpsc::UnboundedSender<String>> = None;
        let mut kotak_sent: HashSet<String> = HashSet::new();
        let mut refresh = tokio::time::interval(REFRESH_EVERY);

        loop {
            tokio::select! {
                _ = refresh.tick() => {
                    if !shared_domain::is_market_open() {
                        break;
                    }
                    if futures.as_ref().is_none_or(|(day, _)| *day != today) && tokio::time::Instant::now() >= futures_retry_at {
                        match near_futures(&client, today).await {
                            Ok(list) => futures = Some((today, list)),
                            Err(e) => {
                                tracing::warn!(error = %e, "Feed latency: Upstox futures lookup failed — indices and held contracts only for now");
                                futures_retry_at = tokio::time::Instant::now() + FUTURES_RETRY;
                            }
                        }
                    }
                    let mut watches = index_watches();
                    if let Some((day, list)) = &futures {
                        if *day == today {
                            watches.extend(list.iter().cloned());
                        }
                    }
                    watches.extend(position_watches(&positions.read().await));
                    me.set_watches(&watches);
                    let keys: BTreeSet<String> = watches.iter().map(|w| w.upstox_key.clone()).collect();
                    keys_tx.send_if_modified(|current| {
                        let changed = *current != keys;
                        if changed {
                            *current = keys;
                        }
                        changed
                    });
                    // A fresh bridge (a re-login) knows none of our subscriptions.
                    if let Some(tx) = ws_tx.lock().await.clone() {
                        if !kotak_channel.as_ref().is_some_and(|c| c.same_channel(&tx)) {
                            kotak_channel = Some(tx.clone());
                            kotak_sent.clear();
                        }
                        for msg in watches.iter().filter_map(|w| w.kotak_subscribe.as_ref()) {
                            if kotak_sent.insert(msg.clone()) {
                                let _ = tx.send(msg.clone());
                            }
                        }
                    }
                }
                Some(tick) = tick_rx.recv() => me.on_upstox(&tick, now_ms()),
                tick = kotak_rx.recv() => match tick {
                    Ok(t) => me.on_kotak(&t.key, t.ltp, t.at_ms),
                    Err(broadcast::error::RecvError::Lagged(n)) => me.kotak_lagged(n),
                    Err(broadcast::error::RecvError::Closed) => kotak_rx = kotak_client::subscribe_ticks(),
                },
            }
        }

        drop(keys_tx);
        stream.abort();
        me.close_day();
        tracing::info!("Feed latency: market closed, stopped");
    }
}

/// Upstox's stream for `keys`, reconnecting with backoff until `keys` closes.
async fn stream_with_backoff(
    client: UpstoxClient,
    keys: watch::Receiver<BTreeSet<String>>,
    out: mpsc::Sender<LtpTick>,
    me: Arc<FeedLatency>,
) {
    let mut backoff = STREAM_BACKOFF_START;
    loop {
        match client.stream_ltps(keys.clone(), out.clone()).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(error = %e, retry_in_secs = backoff.as_secs(), "Feed latency: Upstox stream dropped");
                me.upstox_failed(e.to_string());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(STREAM_BACKOFF_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotak_and_upstox_keys_are_the_same_exchange_token() {
        assert_eq!(upstox_key_of("nse_fo|61471").as_deref(), Some("NSE_FO|61471"));
        assert_eq!(upstox_key_of("bse_fo|1100929").as_deref(), Some("BSE_FO|1100929"));
        assert_eq!(kotak_key_of("NSE_FO|61471").as_deref(), Some("nse_fo|61471"));
        assert_eq!(upstox_key_of("nse_cm|11536"), None);
        assert_eq!(upstox_key_of("idx|NIFTY 50"), None);
        assert_eq!(kotak_key_of("NSE_INDEX|Nifty 50"), None);
    }

    #[test]
    fn every_index_is_watched_on_both_feeds() {
        let watches = index_watches();
        assert_eq!(watches.len(), INDEX_NAMES.len());
        let nifty = &watches[0];
        assert_eq!((nifty.upstox_key.as_str(), nifty.kotak_key.as_str()), ("NSE_INDEX|Nifty 50", "idx|NIFTY 50"));
        assert!(nifty.kotak_subscribe.as_deref().unwrap().contains("subscribe_indices"));
    }

    #[test]
    fn the_nearest_future_still_trading_is_watched_by_its_month() {
        let date = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        let fut = |underlying: &str, kind: &str, expiry, key: &str| upstox_client::Derivative {
            underlying: underlying.into(),
            kind: kind.into(),
            expiry,
            strike: 0.0,
            instrument_key: key.into(),
            lot_size: 65,
        };
        let list = vec![
            fut("NIFTY", "FUT", date(12, 29), "NSE_FO|58875"),
            fut("NIFTY", "FUT", date(11, 24), "NSE_FO|61471"),
            fut("NIFTY", "CE", date(11, 10), "NSE_FO|1"),
            fut("BANKNIFTY", "FUT", date(11, 24), "NSE_FO|61466"),
        ];
        let watches = nearest_futures(&list, &["NIFTY", "BANKNIFTY", "FINNIFTY"], date(11, 9));
        let got: Vec<(&str, &str)> = watches.iter().map(|w| (w.label.as_str(), w.kotak_key.as_str())).collect();
        assert_eq!(got, [("NIFTY NOV FUT", "nse_fo|61471"), ("BANKNIFTY NOV FUT", "nse_fo|61466")]);
        assert!(watches[0].kotak_subscribe.as_deref().unwrap().contains("\"ladder\":true"));
        // On its expiry day it still trades; the day after, December's is the nearest.
        assert_eq!(nearest_futures(&list, &["NIFTY"], date(11, 24))[0].upstox_key, "NSE_FO|61471");
        let after = nearest_futures(&list, &["NIFTY"], date(11, 25));
        assert_eq!((after[0].label.as_str(), after[0].upstox_key.as_str()), ("NIFTY DEC FUT", "NSE_FO|58875"));
    }

    #[test]
    fn a_new_day_starts_empty_and_a_dropped_instrument_keeps_its_class_day() {
        let me = FeedLatency::new(true);
        let day = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        me.open_day(day);
        let nifty = index_watches().remove(0);
        me.set_watches(std::slice::from_ref(&nifty));
        let at = now_ms();
        // Each feed's first tick is a snapshot: where prices start.
        me.on_kotak("idx|NIFTY 50", 24_990.0, at - 1_000);
        me.on_upstox(&LtpTick { key: "NSE_INDEX|Nifty 50".into(), ltp: 24_990.0, ltt_ms: at - 1_100 }, at - 1_000);
        me.on_kotak("idx|NIFTY 50", 25_000.0, at - 100);
        me.on_upstox(&LtpTick { key: "NSE_INDEX|Nifty 50".into(), ltp: 25_000.0, ltt_ms: at - 150 }, at);
        let snap = me.snapshot();
        assert_eq!(snap.status, "streaming");
        let figures = snap.instruments[0].minute;
        assert_eq!(figures.upstox_delay.unwrap().p50, 150.0);
        assert_eq!(figures.kotak_delay.unwrap().p50, 50.0);
        assert_eq!(figures.kotak_lead.unwrap().p50, 100.0);

        // At the close the day's figures stay to be read.
        me.close_day();
        let snap = me.snapshot();
        assert_eq!(snap.status, "market_closed");
        assert_eq!(snap.instruments[0].day.upstox_delay.unwrap().n, 1);

        // Gone from the watch set: its row goes, its class keeps the day.
        me.set_watches(&[]);
        let snap = me.snapshot();
        assert!(snap.instruments.is_empty());
        assert_eq!(snap.classes[0].day.upstox_delay.unwrap().n, 1);

        // The next day starts over.
        me.open_day(day.succ_opt().unwrap());
        assert!(me.snapshot().classes[0].day.upstox_delay.is_none());
        assert_eq!(FeedLatency::new(false).snapshot().status, "not_configured");
    }
}
