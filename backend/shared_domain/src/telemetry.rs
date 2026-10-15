//! Process-wide Prometheus metrics.
//!
//! Every crate records into the statics below; the server renders them at
//! `GET /metrics` and samples them every 2s for the dashboard's System screen.
//!
//! Recording must never be able to hurt trading, so nothing here blocks,
//! awaits or panics at a call site:
//! - label values are fixed `&'static str`s from the enums and mappers below,
//!   never a free-form string such as an exit reason with a price in it, so
//!   the series count stays bounded;
//! - labelled lookups go through `get_metric_with_label_values` and drop the
//!   sample on error, never `with_label_values`, which panics on a label-count
//!   mismatch.
//!
//! Metric construction can only fail on an invalid name — a programming error
//! the tests at the bottom catch, and which [`init`] surfaces at server start,
//! before any trading begins.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use prometheus::core::Collector;
use prometheus::proto::MetricFamily;
use prometheus::{
    Encoder, Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};

use crate::{DbWriteMessage, OrderRequest, OrderType, TransactionType};

/// Re-exported so readers of this registry need no direct `prometheus` dependency.
pub use prometheus::{HistogramVec as HistogramFamily, IntCounterVec as CounterFamily};

/// The engine tick period; a tick body that runs longer is an overrun.
pub const TICK_BUDGET: Duration = Duration::from_millis(50);

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    Registry::new_custom(Some("autotrader".into()), None).expect("valid registry prefix")
});

fn register<C: Collector + Clone + 'static>(c: C) -> C {
    // Only a duplicate name fails here (covered by the tests). An unregistered
    // metric still records — it just isn't exported — so never panic over it.
    let _ = REGISTRY.register(Box::new(c.clone()));
    c
}

fn counter(name: &str, help: &str) -> IntCounter {
    register(IntCounter::new(name, help).expect("valid metric name"))
}

fn counter_vec(name: &str, help: &str, labels: &[&str]) -> IntCounterVec {
    register(IntCounterVec::new(Opts::new(name, help), labels).expect("valid metric name"))
}

fn int_gauge(name: &str, help: &str) -> IntGauge {
    register(IntGauge::new(name, help).expect("valid metric name"))
}

fn int_gauge_vec(name: &str, help: &str, labels: &[&str]) -> IntGaugeVec {
    register(IntGaugeVec::new(Opts::new(name, help), labels).expect("valid metric name"))
}

fn gauge(name: &str, help: &str) -> Gauge {
    register(Gauge::new(name, help).expect("valid metric name"))
}

fn gauge_vec(name: &str, help: &str, labels: &[&str]) -> GaugeVec {
    register(GaugeVec::new(Opts::new(name, help), labels).expect("valid metric name"))
}

fn histogram(name: &str, help: &str, buckets: &[f64]) -> Histogram {
    register(
        Histogram::with_opts(HistogramOpts::new(name, help).buckets(buckets.to_vec()))
            .expect("valid metric name"),
    )
}

fn histogram_vec(name: &str, help: &str, labels: &[&str], buckets: &[f64]) -> HistogramVec {
    register(
        HistogramVec::new(HistogramOpts::new(name, help).buckets(buckets.to_vec()), labels)
            .expect("valid metric name"),
    )
}

fn inc(vec: &IntCounterVec, labels: &[&str]) {
    if let Ok(c) = vec.get_metric_with_label_values(labels) {
        c.inc();
    }
}

fn observe(vec: &HistogramVec, labels: &[&str], v: f64) {
    if let Ok(h) = vec.get_metric_with_label_values(labels) {
        h.observe(v);
    }
}

/// Set one labelled integer gauge, dropping the sample on a label mismatch.
pub fn set_int(vec: &IntGaugeVec, labels: &[&str], v: i64) {
    if let Ok(g) = vec.get_metric_with_label_values(labels) {
        g.set(v);
    }
}

/// Set one labelled float gauge, dropping the sample on a label mismatch.
pub fn set_float(vec: &GaugeVec, labels: &[&str], v: f64) {
    if let Ok(g) = vec.get_metric_with_label_values(labels) {
        g.set(v);
    }
}

/// Seconds since the Unix epoch, as Prometheus timestamps are written.
pub fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

const FAST_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
];
/// LIVE ticks run broker HTTP inline (30s client timeout), so the tick
/// histogram has to reach that far.
const TICK_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];
const BROKER_BUCKETS: &[f64] = &[0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0];
const MUTEX_BUCKETS: &[f64] = &[
    0.0001, 0.001, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0,
];

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub static ENGINE_TICK_SECONDS: LazyLock<Histogram> = LazyLock::new(|| histogram(
    "engine_tick_duration_seconds", "Time spent in one position-monitor tick body", TICK_BUCKETS,
));
pub static ENGINE_TICKS: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "engine_ticks_total", "Position-monitor ticks completed",
));
pub static ENGINE_TICK_OVERRUNS: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "engine_tick_overruns_total", "Ticks whose body ran longer than the 50ms tick period",
));
pub static ENGINE_LAST_TICK: LazyLock<Gauge> = LazyLock::new(|| gauge(
    "engine_last_tick_timestamp_seconds", "Unix time the last monitor tick completed (heartbeat)",
));
pub static ENGINE_LIVE_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "engine_live_errors_total", "LIVE errors surfaced to the UI (loud_error)",
));
pub static LIVE_HALTS: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "engine_live_halts_total", "LIVE positions halted after repeated failed exits",
));
pub static LIVE_STARTUP_RECONCILED: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "engine_live_startup_reconciled", "1 once LIVE startup reconciliation against the broker succeeded",
));
pub static TRADES: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "engine_trades_total", "Trades booked by the engine", &["mode", "side"],
));
pub static TRADING_MODE: LazyLock<IntGaugeVec> = LazyLock::new(|| int_gauge_vec(
    "trading_mode", "1 for the active trading mode", &["mode"],
));
pub static KILL_SWITCH_ACTIVE: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "kill_switch_active", "1 while the kill switch blocks new entries",
));
pub static POSITIONS: LazyLock<IntGaugeVec> = LazyLock::new(|| int_gauge_vec(
    "positions", "Tracked positions by state", &["state"],
));
pub static UNREALIZED_MTM: LazyLock<Gauge> = LazyLock::new(|| gauge(
    "unrealized_mtm_inr", "Mark-to-market P&L of open quantity at the latest LTP, in INR",
));

/// Times one monitor tick. On drop — whichever `continue` ends the tick — it
/// records the duration, counts an overrun past [`TICK_BUDGET`] and stamps the
/// heartbeat, so a monitor stuck inside a tick shows as a growing heartbeat age.
pub struct TickTimer {
    started: Instant,
}

impl TickTimer {
    pub fn start() -> Self {
        Self { started: Instant::now() }
    }
}

impl Drop for TickTimer {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        ENGINE_TICK_SECONDS.observe(elapsed.as_secs_f64());
        ENGINE_TICKS.inc();
        if elapsed > TICK_BUDGET {
            ENGINE_TICK_OVERRUNS.inc();
        }
        ENGINE_LAST_TICK.set(unix_now());
    }
}

/// Count one trade row the engine books (`mode` "LIVE"/"PAPER", `action`
/// "BUY"/"SELL").
pub fn trade_booked(mode: &str, action: &str) {
    let mode = mode_label(mode);
    let side = if action.eq_ignore_ascii_case("BUY") {
        "buy"
    } else if action.eq_ignore_ascii_case("SELL") {
        "sell"
    } else {
        "other"
    };
    inc(&TRADES, &[mode, side]);
}

/// `"live"` / `"paper"` for a `TradingConfig::mode` string.
pub fn mode_label(mode: &str) -> &'static str {
    if mode.eq_ignore_ascii_case("LIVE") {
        "live"
    } else if mode.eq_ignore_ascii_case("PAPER") {
        "paper"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

pub static SIGNALS_RECEIVED: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "signals_received_total", "Signals the engine received", &["action"],
));
pub static SIGNAL_OUTCOMES: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "signal_outcomes_total", "What the engine did with each received signal", &["outcome"],
));
pub static SIGNALS_LAGGED: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "signals_lagged_total", "Signals dropped because the engine's receiver lagged",
));

/// What the engine did with one received signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalOutcome {
    Accepted,
    EditApplied,
    DuplicateRevised,
    DuplicateSkipped,
    SlUpdated,
    SlUnmatched,
    ExitApplied,
    ExitUnmatched,
    RejectedNoTarget,
    RejectedExpired,
    RejectedKillSwitch,
    RejectedZeroLots,
    RejectedNoScripMaster,
    RejectedUnresolved,
    RejectedAtTarget,
    RejectedUnsupported,
}

impl SignalOutcome {
    pub const ALL: [SignalOutcome; 16] = [
        Self::Accepted, Self::EditApplied, Self::DuplicateRevised, Self::DuplicateSkipped,
        Self::SlUpdated, Self::SlUnmatched, Self::ExitApplied, Self::ExitUnmatched,
        Self::RejectedNoTarget, Self::RejectedExpired, Self::RejectedKillSwitch,
        Self::RejectedZeroLots, Self::RejectedNoScripMaster, Self::RejectedUnresolved,
        Self::RejectedAtTarget, Self::RejectedUnsupported,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::EditApplied => "edit_applied",
            Self::DuplicateRevised => "duplicate_revised",
            Self::DuplicateSkipped => "duplicate_skipped",
            Self::SlUpdated => "sl_updated",
            Self::SlUnmatched => "sl_unmatched",
            Self::ExitApplied => "exit_applied",
            Self::ExitUnmatched => "exit_unmatched",
            Self::RejectedNoTarget => "rejected_no_target",
            Self::RejectedExpired => "rejected_expired",
            Self::RejectedKillSwitch => "rejected_kill_switch",
            Self::RejectedZeroLots => "rejected_zero_lots",
            Self::RejectedNoScripMaster => "rejected_no_scrip_master",
            Self::RejectedUnresolved => "rejected_unresolved",
            Self::RejectedAtTarget => "rejected_at_target",
            Self::RejectedUnsupported => "rejected_unsupported",
        }
    }
}

/// Count one signal arriving at the engine.
pub fn signal_received(action: &str) {
    let action = match action {
        a if a.eq_ignore_ascii_case("BUY") => "buy",
        a if a.eq_ignore_ascii_case("UPDATE_SL") => "update_sl",
        a if a.eq_ignore_ascii_case("EXIT_AT") => "exit_at",
        _ => "other",
    };
    inc(&SIGNALS_RECEIVED, &[action]);
}

/// Count what the engine did with a signal.
pub fn signal(outcome: SignalOutcome) {
    inc(&SIGNAL_OUTCOMES, &[outcome.as_str()]);
}

// ---------------------------------------------------------------------------
// Orders (LIVE)
// ---------------------------------------------------------------------------

pub static ORDERS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "orders_total", "LIVE orders sent to the broker, by outcome", &["side", "type", "result"],
));
pub static ORDER_CANCELS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "order_cancels_total", "LIVE order cancels sent to the broker", &["result"],
));
pub static ORDER_BOOK_POLL_FAILURES: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "order_book_poll_failures_total", "LIVE fill-reconciliation order-book polls that failed",
));
pub static KOTAK_MUTEX_WAIT: LazyLock<Histogram> = LazyLock::new(|| histogram(
    "kotak_mutex_wait_seconds", "Time the engine waited for the shared Kotak client lock", MUTEX_BUCKETS,
));

/// Order outcome labels, from `KotakError::metric_label` or `"ok"`.
pub const ORDER_RESULTS: [&str; 5] = ["ok", "rejected", "ambiguous", "session_expired", "error"];

/// Count one order sent to the broker. `result` is one of [`ORDER_RESULTS`].
pub fn order_sent(order: &OrderRequest, result: &'static str) {
    let side = match order.transaction_type {
        TransactionType::Buy => "buy",
        TransactionType::Sell => "sell",
    };
    let kind = match order.order_type {
        OrderType::Market => "market",
        OrderType::Limit => "limit",
        OrderType::StopLoss => "sl",
        OrderType::StopLossMarket => "sl_m",
    };
    inc(&ORDERS, &[side, kind, result]);
}

pub fn order_cancel(ok: bool) {
    inc(&ORDER_CANCELS, &[if ok { "ok" } else { "error" }]);
}

// ---------------------------------------------------------------------------
// Kotak REST + session
// ---------------------------------------------------------------------------

pub static KOTAK_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "kotak_requests_total", "Kotak REST calls by endpoint and HTTP outcome", &["endpoint", "result"],
));
pub static KOTAK_REQUEST_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| histogram_vec(
    "kotak_request_duration_seconds", "Kotak REST latency to response headers", &["endpoint"], BROKER_BUCKETS,
));
pub static KOTAK_SESSION_HEALTHY: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "kotak_session_healthy", "1 while the Kotak session watchdog considers the session usable",
));
pub static KOTAK_CONNECTED: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "kotak_connected", "1 while a Kotak client session object exists",
));
pub static KOTAK_RELOGINS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "kotak_relogins_total", "Mid-day Kotak re-logins by the session watchdog", &["result"],
));

/// `2xx`/`3xx`/`4xx`/`5xx` for an HTTP status code.
pub fn status_class(code: u16) -> &'static str {
    match code {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

/// Record one Kotak REST call. `endpoint` is a fixed name from the client;
/// `result` is a [`status_class`] or `"timeout"` / `"transport_error"`.
pub fn kotak_request(endpoint: &'static str, result: &'static str, secs: f64) {
    inc(&KOTAK_REQUESTS, &[endpoint, result]);
    observe(&KOTAK_REQUEST_SECONDS, &[endpoint], secs);
}

pub fn kotak_mutex_wait(waited: Duration) {
    KOTAK_MUTEX_WAIT.observe(waited.as_secs_f64());
}

pub fn kotak_relogin(ok: bool) {
    inc(&KOTAK_RELOGINS, &[if ok { "ok" } else { "error" }]);
}

// ---------------------------------------------------------------------------
// Price feed
// ---------------------------------------------------------------------------

pub static FEED_TICKS: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "feed_ticks_total", "LTP ticks received from the Kotak market feed",
));
pub static FEED_LAST_TICK: LazyLock<Gauge> = LazyLock::new(|| gauge(
    "feed_last_tick_timestamp_seconds", "Unix time of the last LTP tick",
));
pub static FEED_CONNECTED: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "feed_connected", "1 while the Node bridge reports the market feed connected",
));
pub static FEED_SUBSCRIPTIONS: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "feed_subscriptions", "Scrips the market feed is subscribed to",
));
pub static FEED_BRIDGE_RESTARTS: LazyLock<IntCounter> = LazyLock::new(|| counter(
    "feed_bridge_restarts_total", "Node bridge spawn failures and exits",
));

/// Per-scrip last-tick time, for the positions table's "LTP age". Kept out of
/// the exported labels on purpose — scrips change daily.
static SCRIP_LAST_TICK: LazyLock<Mutex<HashMap<String, f64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record one LTP tick for `scrip` (`segment|token`, the `ltp_map` key).
pub fn feed_tick(scrip: &str) {
    let now = unix_now();
    FEED_TICKS.inc();
    FEED_LAST_TICK.set(now);
    if let Ok(mut m) = SCRIP_LAST_TICK.lock() {
        match m.get_mut(scrip) {
            Some(t) => *t = now,
            None => {
                m.insert(scrip.to_owned(), now);
            }
        }
    }
}

/// Unix time of the last tick for `scrip`, if one has arrived.
pub fn scrip_last_tick(scrip: &str) -> Option<f64> {
    SCRIP_LAST_TICK.lock().ok().and_then(|m| m.get(scrip).copied())
}

/// Forget scrips that haven't ticked for `max_age` — tokens change daily, so
/// without this the map grows for the life of the process.
pub fn prune_scrip_ticks(max_age: Duration) {
    prune_scrip_ticks_at(unix_now(), max_age);
}

fn prune_scrip_ticks_at(now: f64, max_age: Duration) {
    if let Ok(mut m) = SCRIP_LAST_TICK.lock() {
        m.retain(|_, t| now - *t <= max_age.as_secs_f64());
    }
}

// ---------------------------------------------------------------------------
// Telegram
// ---------------------------------------------------------------------------

pub static TELEGRAM_CONNECTED: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "telegram_connected", "Telegram update loops currently running (0 = not listening)",
));
pub static TELEGRAM_MESSAGES: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "telegram_messages_total", "Messages from watched chats", &["kind"],
));
pub static TELEGRAM_PARSE: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "telegram_parse_total", "What each watched message parsed as", &["result"],
));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseResult {
    Signal,
    SlUpdate,
    Exit,
    Unparsed,
}

impl ParseResult {
    pub const ALL: [ParseResult; 4] = [Self::Signal, Self::SlUpdate, Self::Exit, Self::Unparsed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Signal => "signal",
            Self::SlUpdate => "sl_update",
            Self::Exit => "exit",
            Self::Unparsed => "unparsed",
        }
    }
}

/// Held for the life of one Telegram update loop. A count rather than a 0/1
/// flag, since the env ingester and a dashboard login can each run a loop;
/// the decrement is in `Drop` so a panic or an aborted task still clears it.
pub struct TelegramLoopGuard;

impl TelegramLoopGuard {
    pub fn enter() -> Self {
        TELEGRAM_CONNECTED.inc();
        TelegramLoopGuard
    }
}

impl Drop for TelegramLoopGuard {
    fn drop(&mut self) {
        TELEGRAM_CONNECTED.dec();
    }
}

pub fn telegram_message(edited: bool) {
    inc(&TELEGRAM_MESSAGES, &[if edited { "edited" } else { "new" }]);
}

pub fn telegram_parsed(result: ParseResult) {
    inc(&TELEGRAM_PARSE, &[result.as_str()]);
}

// ---------------------------------------------------------------------------
// SQLite writer
// ---------------------------------------------------------------------------

pub static DB_QUEUE_DEPTH: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "db_queue_depth", "Messages waiting for the single SQLite writer",
));
pub static DB_QUEUE_CAPACITY: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "db_queue_capacity", "Capacity of the SQLite writer queue",
));
pub static DB_WRITE_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| histogram_vec(
    "db_write_duration_seconds", "SQLite writer time per message", &["kind"], FAST_BUCKETS,
));
pub static DB_WRITE_ERRORS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "db_write_errors_total", "SQLite writer failures", &["kind"],
));
pub static DB_POOL_CONNECTIONS: LazyLock<IntGaugeVec> = LazyLock::new(|| int_gauge_vec(
    "db_pool_connections", "SQLite pool connections", &["state"],
));

pub const DB_WRITE_KINDS: [&str; 4] = ["trade", "log", "positions_snapshot", "oi_snapshot"];

impl DbWriteMessage {
    /// Fixed metric label for this message's kind.
    pub fn metric_kind(&self) -> &'static str {
        match self {
            DbWriteMessage::Trade { .. } => "trade",
            DbWriteMessage::Log { .. } => "log",
            DbWriteMessage::PositionsSnapshot { .. } => "positions_snapshot",
            DbWriteMessage::OiSnapshot { .. } => "oi_snapshot",
        }
    }
}

/// Times one SQLite writer message; records on drop, so every `continue` in
/// the writer is still counted.
pub struct DbWriteTimer {
    kind: &'static str,
    started: Instant,
}

impl DbWriteTimer {
    pub fn start(kind: &'static str) -> Self {
        Self { kind, started: Instant::now() }
    }
}

impl Drop for DbWriteTimer {
    fn drop(&mut self) {
        observe(&DB_WRITE_SECONDS, &[self.kind], self.started.elapsed().as_secs_f64());
    }
}

pub fn db_write_error(kind: &'static str) {
    inc(&DB_WRITE_ERRORS, &[kind]);
}

// ---------------------------------------------------------------------------
// Upstox (market data, read-only)
// ---------------------------------------------------------------------------

pub static UPSTOX_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "upstox_requests_total", "Upstox analytics API calls", &["endpoint", "result"],
));
pub static UPSTOX_REQUEST_SECONDS: LazyLock<Histogram> = LazyLock::new(|| histogram(
    "upstox_request_duration_seconds", "Upstox analytics API latency", BROKER_BUCKETS,
));
pub static UPSTOX_LAST_SUCCESS: LazyLock<Gauge> = LazyLock::new(|| gauge(
    "upstox_last_success_timestamp_seconds", "Unix time of the last successful Upstox call",
));

/// Record one Upstox call. `result` is `"ok"`, a [`status_class`], or
/// `"transport_error"` / `"decode_error"`.
pub fn upstox_request(endpoint: &'static str, result: &'static str, secs: f64) {
    inc(&UPSTOX_REQUESTS, &[endpoint, result]);
    UPSTOX_REQUEST_SECONDS.observe(secs);
    if result == "ok" {
        UPSTOX_LAST_SUCCESS.set(unix_now());
    }
}

// ---------------------------------------------------------------------------
// HTTP API
// ---------------------------------------------------------------------------

pub static HTTP_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| counter_vec(
    "http_requests_total", "Dashboard API requests", &["route", "method", "status"],
));
pub static HTTP_REQUEST_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| histogram_vec(
    "http_request_duration_seconds", "Dashboard API latency to response headers", &["route"], FAST_BUCKETS,
));
pub static HTTP_IN_FLIGHT: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "http_in_flight_requests", "Dashboard API requests being handled",
));
pub static SSE_CLIENTS: LazyLock<IntGaugeVec> = LazyLock::new(|| int_gauge_vec(
    "sse_clients", "Connected server-sent-event streams", &["stream"],
));

/// Record one HTTP request. `route` must be a router pattern (bounded), never
/// the raw request path.
pub fn http_request(route: &str, method: &str, status: u16, secs: f64) {
    let method = match method {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    inc(&HTTP_REQUESTS, &[route, method, status_class(status)]);
    observe(&HTTP_REQUEST_SECONDS, &[route], secs);
}

// ---------------------------------------------------------------------------
// Process / host / runtime (set by the server's sampler)
// ---------------------------------------------------------------------------

pub static PROCESS_CPU_PERCENT: LazyLock<Gauge> = LazyLock::new(|| gauge(
    "process_cpu_percent", "Server process CPU use, percent of one core",
));
pub static PROCESS_RSS_BYTES: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "process_resident_memory_bytes", "Server process resident memory",
));
pub static PROCESS_VIRTUAL_BYTES: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "process_virtual_memory_bytes", "Server process virtual memory",
));
pub static PROCESS_UPTIME: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "process_uptime_seconds", "Server process run time",
));
pub static HOST_CPU_PERCENT: LazyLock<Gauge> = LazyLock::new(|| gauge(
    "host_cpu_percent", "Host CPU use across all cores, percent",
));
pub static HOST_MEMORY_BYTES: LazyLock<IntGaugeVec> = LazyLock::new(|| int_gauge_vec(
    "host_memory_bytes", "Host memory", &["kind"],
));
pub static HOST_LOAD: LazyLock<GaugeVec> = LazyLock::new(|| gauge_vec(
    "host_load_average", "Host load average", &["window"],
));
pub static TOKIO_WORKERS: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "tokio_workers", "Tokio runtime worker threads",
));
pub static TOKIO_ALIVE_TASKS: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "tokio_alive_tasks", "Tokio tasks currently alive",
));
pub static TOKIO_GLOBAL_QUEUE: LazyLock<IntGauge> = LazyLock::new(|| int_gauge(
    "tokio_global_queue_depth", "Tasks waiting in the Tokio global run queue",
));

// ---------------------------------------------------------------------------
// Startup, export and reading back
// ---------------------------------------------------------------------------

/// Register every metric and pre-create the fixed label sets, so `/metrics`
/// lists every series from the first scrape (a counter that is absent rather
/// than 0 breaks `rate()`), and so an invalid metric name fails at server
/// start instead of on the first trade.
pub fn init() {
    let _ = &*ENGINE_TICK_SECONDS;
    let _ = &*ENGINE_TICKS;
    let _ = &*ENGINE_TICK_OVERRUNS;
    let _ = &*ENGINE_LAST_TICK;
    let _ = &*ENGINE_LIVE_ERRORS;
    let _ = &*LIVE_HALTS;
    let _ = &*LIVE_STARTUP_RECONCILED;
    let _ = &*KILL_SWITCH_ACTIVE;
    let _ = &*UNREALIZED_MTM;
    let _ = &*SIGNALS_LAGGED;
    let _ = &*ORDER_BOOK_POLL_FAILURES;
    let _ = &*KOTAK_MUTEX_WAIT;
    let _ = &*KOTAK_SESSION_HEALTHY;
    let _ = &*KOTAK_CONNECTED;
    let _ = &*FEED_TICKS;
    let _ = &*FEED_LAST_TICK;
    let _ = &*FEED_CONNECTED;
    let _ = &*FEED_SUBSCRIPTIONS;
    let _ = &*FEED_BRIDGE_RESTARTS;
    let _ = &*TELEGRAM_CONNECTED;
    let _ = &*DB_QUEUE_DEPTH;
    let _ = &*DB_QUEUE_CAPACITY;
    let _ = &*UPSTOX_REQUEST_SECONDS;
    let _ = &*UPSTOX_LAST_SUCCESS;
    let _ = &*HTTP_REQUEST_SECONDS;
    let _ = &*HTTP_IN_FLIGHT;
    let _ = &*PROCESS_CPU_PERCENT;
    let _ = &*PROCESS_RSS_BYTES;
    let _ = &*PROCESS_VIRTUAL_BYTES;
    let _ = &*PROCESS_UPTIME;
    let _ = &*HOST_CPU_PERCENT;
    let _ = &*TOKIO_WORKERS;
    let _ = &*TOKIO_ALIVE_TASKS;
    let _ = &*TOKIO_GLOBAL_QUEUE;
    let _ = &*KOTAK_REQUEST_SECONDS;

    for mode in ["live", "paper"] {
        let _ = TRADING_MODE.get_metric_with_label_values(&[mode]);
        for side in ["buy", "sell"] {
            let _ = TRADES.get_metric_with_label_values(&[mode, side]);
        }
    }
    for state in ["waiting", "active", "target1", "closed"] {
        let _ = POSITIONS.get_metric_with_label_values(&[state]);
    }
    for action in ["buy", "update_sl", "exit_at", "other"] {
        let _ = SIGNALS_RECEIVED.get_metric_with_label_values(&[action]);
    }
    for o in SignalOutcome::ALL {
        let _ = SIGNAL_OUTCOMES.get_metric_with_label_values(&[o.as_str()]);
    }
    for side in ["buy", "sell"] {
        for kind in ["market", "limit"] {
            for result in ORDER_RESULTS {
                let _ = ORDERS.get_metric_with_label_values(&[side, kind, result]);
            }
        }
    }
    for result in ["ok", "error"] {
        let _ = ORDER_CANCELS.get_metric_with_label_values(&[result]);
        let _ = KOTAK_RELOGINS.get_metric_with_label_values(&[result]);
    }
    for kind in ["new", "edited"] {
        let _ = TELEGRAM_MESSAGES.get_metric_with_label_values(&[kind]);
    }
    for r in ParseResult::ALL {
        let _ = TELEGRAM_PARSE.get_metric_with_label_values(&[r.as_str()]);
    }
    for kind in DB_WRITE_KINDS {
        let _ = DB_WRITE_SECONDS.get_metric_with_label_values(&[kind]);
        let _ = DB_WRITE_ERRORS.get_metric_with_label_values(&[kind]);
    }
    for state in ["idle", "in_use"] {
        let _ = DB_POOL_CONNECTIONS.get_metric_with_label_values(&[state]);
    }
    for kind in ["total", "available", "swap_total", "swap_used"] {
        let _ = HOST_MEMORY_BYTES.get_metric_with_label_values(&[kind]);
    }
    for window in ["1m", "5m", "15m"] {
        let _ = HOST_LOAD.get_metric_with_label_values(&[window]);
    }
    for stream in ["logs", "telemetry"] {
        let _ = SSE_CLIENTS.get_metric_with_label_values(&[stream]);
    }
    // Labelled families with no fixed label set (Kotak / Upstox endpoints,
    // HTTP routes) appear on first use.
    let _ = &*KOTAK_REQUESTS;
    let _ = &*UPSTOX_REQUESTS;
    let _ = &*HTTP_REQUESTS;
}

/// Prometheus text exposition of every registered metric.
pub fn render_text() -> String {
    let mut buf = Vec::new();
    // Encoding into a Vec only fails on a malformed family, which the
    // registry never produces; render what we have either way.
    let _ = TextEncoder::new().encode(&REGISTRY.gather(), &mut buf);
    String::from_utf8(buf).unwrap_or_default()
}

fn families(c: &impl Collector) -> Vec<MetricFamily> {
    c.collect()
}

fn label_value<'a>(m: &'a prometheus::proto::Metric, name: &str) -> &'a str {
    m.get_label().iter().find(|l| l.name() == name).map(|l| l.value()).unwrap_or("")
}

/// Sum of a counter family across every label set.
pub fn counter_total(c: &IntCounterVec) -> u64 {
    families(c)
        .iter()
        .flat_map(|f| f.get_metric())
        .map(|m| m.get_counter().get_value() as u64)
        .sum()
}

/// A counter family summed per value of one label.
pub fn counter_by(c: &IntCounterVec, label: &str) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    for f in families(c) {
        for m in f.get_metric() {
            let key = label_value(m, label);
            let v = m.get_counter().get_value() as u64;
            match out.iter_mut().find(|(k, _)| k == key) {
                Some((_, total)) => *total += v,
                None => out.push((key.to_owned(), v)),
            }
        }
    }
    out
}

/// A counter family summed per (`a`, `b`) label-value pair.
pub fn counter_by_pair(c: &IntCounterVec, a: &str, b: &str) -> Vec<((String, String), u64)> {
    let mut out: Vec<((String, String), u64)> = Vec::new();
    for f in families(c) {
        for m in f.get_metric() {
            let key = (label_value(m, a).to_owned(), label_value(m, b).to_owned());
            let v = m.get_counter().get_value() as u64;
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some((_, total)) => *total += v,
                None => out.push((key, v)),
            }
        }
    }
    out
}

/// Cumulative histogram state, summed across label sets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Buckets {
    /// Upper bounds, ascending, excluding `+Inf`.
    pub bounds: Vec<f64>,
    /// Cumulative counts per bound.
    pub cumulative: Vec<u64>,
    pub count: u64,
    pub sum: f64,
}

impl Buckets {
    fn add(&mut self, h: &prometheus::proto::Histogram) {
        let buckets = h.get_bucket();
        if self.bounds.is_empty() {
            self.bounds = buckets.iter().map(|b| b.upper_bound()).collect();
            self.cumulative = vec![0; self.bounds.len()];
        }
        for (i, b) in buckets.iter().enumerate().take(self.cumulative.len()) {
            self.cumulative[i] += b.cumulative_count();
        }
        self.count += h.get_sample_count();
        self.sum += h.get_sample_sum();
    }
}

/// Buckets of a histogram (or every label set of a histogram vec, summed).
pub fn buckets(c: &impl Collector) -> Buckets {
    let mut out = Buckets::default();
    for f in families(c) {
        for m in f.get_metric() {
            out.add(m.get_histogram());
        }
    }
    out
}

/// Buckets of a histogram vec, one entry per value of `label`.
pub fn buckets_by(c: &HistogramVec, label: &str) -> Vec<(String, Buckets)> {
    let mut out: Vec<(String, Buckets)> = Vec::new();
    for f in families(c) {
        for m in f.get_metric() {
            let key = label_value(m, label);
            match out.iter_mut().find(|(k, _)| k == key) {
                Some((_, b)) => b.add(m.get_histogram()),
                None => {
                    let mut b = Buckets::default();
                    b.add(m.get_histogram());
                    out.push((key.to_owned(), b));
                }
            }
        }
    }
    out
}

/// The `q` quantile of the observations made between `earlier` and `later`
/// (two readings of the same histogram), interpolated linearly inside the
/// bucket it falls in — the same estimate PromQL's `histogram_quantile` makes.
/// `None` when nothing was observed in between. A quantile that lands past
/// the last finite bound reports that bound.
pub fn quantile_between(earlier: &Buckets, later: &Buckets, q: f64) -> Option<f64> {
    let observed = later.count.checked_sub(earlier.count)?;
    if observed == 0 || later.bounds.is_empty() {
        return None;
    }
    let delta = |i: usize| -> u64 {
        let prev = earlier.cumulative.get(i).copied().unwrap_or(0);
        later.cumulative[i].saturating_sub(prev)
    };
    let rank = q.clamp(0.0, 1.0) * observed as f64;
    let mut lower_bound = 0.0;
    let mut lower_count = 0u64;
    for (i, &bound) in later.bounds.iter().enumerate() {
        let cum = delta(i);
        if cum as f64 >= rank {
            let in_bucket = cum.saturating_sub(lower_count);
            if in_bucket == 0 {
                return Some(bound);
            }
            let frac = (rank - lower_count as f64) / in_bucket as f64;
            return Some(lower_bound + (bound - lower_bound) * frac.clamp(0.0, 1.0));
        }
        lower_bound = bound;
        lower_count = cum;
    }
    later.bounds.last().copied()
}

/// Mean of the observations made between two readings, or `None`.
pub fn mean_between(earlier: &Buckets, later: &Buckets) -> Option<f64> {
    let n = later.count.checked_sub(earlier.count)?;
    (n > 0).then(|| (later.sum - earlier.sum) / n as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touch every metric with every fixed label value. A label-count mistake
    /// would silently drop samples at runtime (by design), so catch it here:
    /// every series these calls create must show up in the export.
    #[test]
    fn every_metric_records_and_exports() {
        init();
        { let _t = TickTimer::start(); }
        trade_booked("LIVE", "BUY");
        trade_booked("PAPER", "SELL");
        signal_received("BUY");
        for o in SignalOutcome::ALL {
            signal(o);
        }
        let order = OrderRequest {
            after_market_order: crate::AmoFlag::No,
            disclosed_quantity: "0".into(),
            exchange_segment: crate::ExchangeSegment::NseFo,
            market_protection: "0".into(),
            product_code: crate::ProductCode::Nrml,
            portfolio_flag: "N".into(),
            price: "0".into(),
            order_type: OrderType::Market,
            quantity: "75".into(),
            validity: crate::Validity::Day,
            trigger_price: "0".into(),
            trading_symbol: "NIFTY".into(),
            transaction_type: TransactionType::Buy,
        };
        for r in ORDER_RESULTS {
            order_sent(&order, r);
        }
        order_cancel(true);
        kotak_request("order_book", status_class(200), 0.12);
        kotak_mutex_wait(Duration::from_millis(3));
        kotak_relogin(false);
        feed_tick("nse_fo|12345");
        assert!(scrip_last_tick("nse_fo|12345").is_some());
        telegram_message(true);
        for r in ParseResult::ALL {
            telegram_parsed(r);
        }
        { let _t = DbWriteTimer::start("trade"); }
        db_write_error("log");
        upstox_request("quotes", "ok", 0.3);
        http_request("/api/positions", "GET", 200, 0.004);
        http_request("/api/positions", "BREW", 418, 0.004);

        let text = render_text();
        for name in [
            "autotrader_engine_tick_duration_seconds_bucket",
            "autotrader_engine_ticks_total 1",
            "autotrader_engine_last_tick_timestamp_seconds",
            "autotrader_engine_trades_total{mode=\"live\",side=\"buy\"} 1",
            "autotrader_signal_outcomes_total{outcome=\"rejected_kill_switch\"} 1",
            "autotrader_orders_total{result=\"ambiguous\",side=\"buy\",type=\"market\"} 1",
            "autotrader_order_cancels_total{result=\"ok\"} 1",
            "autotrader_kotak_requests_total{endpoint=\"order_book\",result=\"2xx\"} 1",
            "autotrader_kotak_mutex_wait_seconds_count 1",
            "autotrader_kotak_relogins_total{result=\"error\"} 1",
            "autotrader_feed_ticks_total 1",
            "autotrader_telegram_messages_total{kind=\"edited\"} 1",
            "autotrader_telegram_parse_total{result=\"unparsed\"} 1",
            "autotrader_db_write_duration_seconds_count{kind=\"trade\"} 1",
            "autotrader_db_write_errors_total{kind=\"log\"} 1",
            "autotrader_upstox_requests_total{endpoint=\"quotes\",result=\"ok\"} 1",
            "autotrader_http_requests_total{method=\"OTHER\",route=\"/api/positions\",status=\"4xx\"} 1",
            "autotrader_positions{state=\"waiting\"} 0",
            "autotrader_host_memory_bytes{kind=\"available\"} 0",
            "autotrader_sse_clients{stream=\"telemetry\"} 0",
            "autotrader_process_resident_memory_bytes",
            "autotrader_tokio_alive_tasks",
        ] {
            assert!(text.contains(name), "missing `{name}` in:\n{text}");
        }
    }

    fn reading(bounds: &[f64], cumulative: &[u64], sum: f64) -> Buckets {
        Buckets {
            bounds: bounds.to_vec(),
            cumulative: cumulative.to_vec(),
            count: *cumulative.last().unwrap(),
            sum,
        }
    }

    #[test]
    fn quantile_interpolates_inside_the_bucket() {
        let bounds = [0.01, 0.1, 1.0];
        let earlier = reading(&bounds, &[0, 0, 0], 0.0);
        // 10 observations: 5 at or under 10ms, 4 in (10ms, 100ms], 1 in (100ms, 1s].
        let later = reading(&bounds, &[5, 9, 10], 1.0);
        let p50 = quantile_between(&earlier, &later, 0.5).unwrap();
        assert!((p50 - 0.01).abs() < 1e-9, "p50 {p50}");
        let p90 = quantile_between(&earlier, &later, 0.9).unwrap();
        assert!((p90 - 0.1).abs() < 1e-9, "p90 {p90}");
        let p95 = quantile_between(&earlier, &later, 0.95).unwrap();
        assert!((p95 - 0.55).abs() < 1e-9, "p95 {p95}");
        assert!((mean_between(&earlier, &later).unwrap() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn quantile_uses_only_the_window() {
        let bounds = [0.01, 0.1, 1.0];
        // Old slow observations must not leak into the window.
        let earlier = reading(&bounds, &[0, 0, 100], 100.0);
        let later = reading(&bounds, &[10, 10, 110], 100.05);
        let p95 = quantile_between(&earlier, &later, 0.95).unwrap();
        assert!(p95 <= 0.01, "p95 {p95}");
    }

    #[test]
    fn quantile_is_none_without_observations() {
        let bounds = [0.01, 0.1];
        let r = reading(&bounds, &[3, 4], 0.2);
        assert_eq!(quantile_between(&r, &r, 0.95), None);
        assert_eq!(quantile_between(&Buckets::default(), &Buckets::default(), 0.5), None);
    }

    #[test]
    fn prune_drops_only_stale_scrips() {
        // Written directly, not via `feed_tick`, so the shared tick counter
        // the export test asserts on stays untouched.
        let now = unix_now();
        if let Ok(mut m) = SCRIP_LAST_TICK.lock() {
            m.insert("prune|fresh".into(), now - 60.0);
            m.insert("prune|stale".into(), now - 7200.0);
        }
        prune_scrip_ticks_at(now, Duration::from_secs(3600));
        assert!(scrip_last_tick("prune|fresh").is_some());
        assert!(scrip_last_tick("prune|stale").is_none());
    }

    #[test]
    fn telegram_guard_counts_running_loops() {
        let before = TELEGRAM_CONNECTED.get();
        {
            let _a = TelegramLoopGuard::enter();
            let _b = TelegramLoopGuard::enter();
            assert_eq!(TELEGRAM_CONNECTED.get(), before + 2);
        }
        assert_eq!(TELEGRAM_CONNECTED.get(), before);
    }

    #[test]
    fn quantile_past_last_bound_reports_last_bound() {
        let bounds = [0.01, 0.1];
        let earlier = reading(&bounds, &[0, 0], 0.0);
        // 4 observations, 2 of them above 0.1 (only in +Inf).
        let later = Buckets { bounds: bounds.to_vec(), cumulative: vec![1, 2], count: 4, sum: 9.0 };
        assert_eq!(quantile_between(&earlier, &later, 0.95), Some(0.1));
    }
}
