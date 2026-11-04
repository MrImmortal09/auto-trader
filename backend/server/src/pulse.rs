//! System pulse — the live stats behind the dashboard's System screen.
//!
//! A sampler task wakes every [`SAMPLE_EVERY`], sets the gauges nothing else
//! owns (process, host, runtime, queue depths, position counts), reads the
//! shared Prometheus registry back (`shared_domain::telemetry`), and turns the
//! cumulative counters and histograms into rates and windowed percentiles. The
//! result is one [`Sample`]: broadcast to SSE clients and kept, as a compact
//! [`Point`], in a fixed 15-minute ring so a page reload can redraw its charts.
//!
//! This task only *reads* trading state, and never waits on it: positions and
//! the Kotak client are taken with `try_*` and skipped for that sample if the
//! engine holds them, so the sampler can never delay a tick or an order.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use shared_domain::telemetry::{self, Buckets};
use shared_domain::{MonitoredPosition, TradeState};
use sysinfo::{get_current_pid, ProcessesToUpdate, System};
use tokio::sync::watch;

use crate::AppState;

/// Sampling period: 0.5 Hz, like the Inox pulse.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// History kept for page reloads: 15 minutes of samples.
const HISTORY_LEN: usize = 450;
/// Window for `*_per_sec` / `*_per_min` rates.
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Window for p50 / p95 latencies.
const QUANTILE_WINDOW: Duration = Duration::from_secs(30);
/// Per-scrip last-tick entries older than this are dropped (clears overnight).
const SCRIP_TICK_MAX_AGE: Duration = Duration::from_secs(12 * 3600);
/// `MEMORY_WARN_MB` unset: of the VM's 954 MB, leaving the engine's tick,
/// the Node bridge and the OS the rest. An OOM kill stops the engine with
/// nothing restarting it, so the warning comes well before.
pub const DEFAULT_MEMORY_WARN_MB: f64 = 600.0;
/// The alarm clears only once RSS is this far back under the threshold, so
/// a process hovering at it warns once, not on every sample.
const MEMORY_WARN_CLEAR_RATIO: f64 = 0.95;

/// Resident memory, in MB, above which the sampler warns and `memory_warn`
/// is set: `MEMORY_WARN_MB`, or [`DEFAULT_MEMORY_WARN_MB`].
pub fn memory_warn_mb() -> f64 {
    parse_memory_warn_mb(std::env::var("MEMORY_WARN_MB").ok().as_deref())
}

/// [`memory_warn_mb`] for a `MEMORY_WARN_MB` value: anything that isn't a
/// positive number is the default.
fn parse_memory_warn_mb(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_MEMORY_WARN_MB)
}

/// Whether the process RSS is over the threshold, with one warning per
/// crossing — see [`MemoryAlarm::observe`].
struct MemoryAlarm {
    threshold_mb: f64,
    over: bool,
}

impl MemoryAlarm {
    fn new(threshold_mb: f64) -> Self {
        Self { threshold_mb, over: false }
    }

    /// Takes a reading: `Some(true)` on the sample that crosses the
    /// threshold, `Some(false)` on the one that comes back under it (by
    /// [`MEMORY_WARN_CLEAR_RATIO`]), `None` while nothing changed.
    fn observe(&mut self, rss_mb: f64) -> Option<bool> {
        let over = if self.over {
            rss_mb >= self.threshold_mb * MEMORY_WARN_CLEAR_RATIO
        } else {
            rss_mb >= self.threshold_mb
        };
        if over == self.over {
            return None;
        }
        self.over = over;
        Some(over)
    }
}

/// The charted subset of a sample — what the history ring keeps.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Point {
    /// Unix seconds.
    pub ts: f64,
    pub process_cpu_pct: f32,
    pub host_cpu_pct: f32,
    pub process_rss_mb: f64,
    pub host_available_mb: f64,
    pub tick_p95_ms: Option<f64>,
    pub ticks_per_sec: f64,
    pub feed_ticks_per_sec: f64,
    pub http_req_per_sec: f64,
    pub http_p95_ms: Option<f64>,
    pub kotak_p95_ms: Option<f64>,
    pub db_queue_depth: usize,
    pub signals_per_min: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PositionCounts {
    pub waiting: usize,
    pub active: usize,
    pub target1: usize,
    pub closed: usize,
}

/// One row of the live positions table.
#[derive(Clone, Debug, Serialize)]
pub struct PositionRow {
    pub id: String,
    pub instrument: String,
    pub strike: Option<f64>,
    pub option_type: Option<String>,
    pub state: &'static str,
    pub qty: i32,
    pub avg_price: f64,
    pub entry_price: f64,
    pub current_sl: f64,
    pub ltp: Option<f64>,
    pub mtm: Option<f64>,
    pub ltp_age_secs: Option<f64>,
    pub halted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct EndpointStat {
    pub endpoint: String,
    pub total: u64,
    pub errors: u64,
    pub p95_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Sample {
    #[serde(flatten)]
    pub point: Point,

    // Context
    pub mode: &'static str,
    pub kill_switch: bool,

    // Process / host / runtime
    pub process_uptime_secs: u64,
    pub process_virtual_mb: f64,
    /// `MEMORY_WARN_MB` — see [`memory_warn_mb`].
    pub memory_warn_mb: f64,
    /// Resident memory over `memory_warn_mb` (until it falls 5% under).
    pub memory_warn: bool,
    pub host_memory_total_mb: f64,
    pub host_swap_total_mb: f64,
    pub host_swap_used_mb: f64,
    pub load_average: [f64; 3],
    pub cpu_cores: usize,
    pub tokio_workers: usize,
    pub tokio_alive_tasks: usize,
    pub tokio_global_queue: usize,

    // Engine
    pub engine_heartbeat_age_secs: Option<f64>,
    pub tick_p50_ms: Option<f64>,
    pub tick_overruns_total: u64,
    pub tick_overruns_window: u64,
    pub live_startup_reconciled: bool,
    pub live_errors_total: u64,
    pub live_halts_total: u64,
    /// `None` when the engine held the positions lock at sample time.
    pub positions: Option<PositionCounts>,
    pub unrealized_mtm: Option<f64>,
    pub positions_table: Option<Vec<PositionRow>>,
    pub trades: BTreeMap<String, u64>,

    // Signals
    pub signals_received_total: u64,
    pub signal_outcomes: BTreeMap<String, u64>,
    pub signals_lagged_total: u64,

    // Orders / broker
    pub orders_by_result: BTreeMap<String, u64>,
    pub orders_by_side: BTreeMap<String, u64>,
    pub order_cancels: BTreeMap<String, u64>,
    pub order_book_poll_failures_total: u64,
    pub kotak_mutex_wait_p95_ms: Option<f64>,
    pub kotak_connected: bool,
    pub kotak_session_healthy: bool,
    pub kotak_requests_total: u64,
    pub kotak_errors_total: u64,
    pub kotak_endpoints: Vec<EndpointStat>,
    pub kotak_relogins: BTreeMap<String, u64>,

    // Price feed
    pub feed_connected: bool,
    pub feed_last_tick_age_secs: Option<f64>,
    pub feed_subscriptions: i64,
    pub feed_ticks_total: u64,
    pub feed_bridge_restarts_total: u64,

    // Telegram
    pub telegram_connected: bool,
    pub telegram_messages_total: u64,
    pub telegram_parse: BTreeMap<String, u64>,

    // SQLite
    pub db_queue_capacity: usize,
    pub db_write_p95_ms: Option<f64>,
    pub db_write_errors_total: u64,
    pub db_pool_size: u32,
    pub db_pool_idle: usize,

    // Upstox
    pub upstox_configured: bool,
    pub upstox_last_success_age_secs: Option<f64>,
    pub upstox_errors_total: u64,

    // HTTP
    pub http_requests_total: u64,
    pub http_4xx_pct: Option<f64>,
    pub http_5xx_pct: Option<f64>,
    pub http_in_flight: i64,
    pub http_routes: Vec<EndpointStat>,
    pub sse_clients: i64,
}

/// Where today's counts start from: the last sample before IST midnight, or
/// none on the day the server started — everything it has counted is today's.
#[derive(Clone)]
pub struct DayStart {
    pub day: chrono::NaiveDate,
    pub baseline: Option<Arc<Sample>>,
}

/// Shared between the sampler (writer) and the routes (readers).
pub struct PulseHub {
    history: Mutex<VecDeque<Point>>,
    latest: watch::Sender<Option<Arc<Sample>>>,
    day_start: Mutex<Option<DayStart>>,
}

impl PulseHub {
    pub fn new() -> Self {
        Self {
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LEN)),
            latest: watch::channel(None).0,
            day_start: Mutex::new(None),
        }
    }

    /// Today's starting point for the counters, once a sample exists.
    pub fn day_start(&self) -> Option<DayStart> {
        self.day_start.lock().ok().and_then(|d| d.clone())
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Sample>>> {
        self.latest.subscribe()
    }

    pub fn latest(&self) -> Option<Arc<Sample>> {
        self.latest.borrow().clone()
    }

    pub fn history(&self) -> Vec<Point> {
        self.history.lock().map(|h| h.iter().cloned().collect()).unwrap_or_default()
    }

    fn publish(&self, sample: Sample) {
        let today = shared_domain::today_ist();
        if let Ok(mut start) = self.day_start.lock() {
            match start.as_ref() {
                None => *start = Some(DayStart { day: today, baseline: None }),
                // A new day: count on from the last sample before midnight.
                Some(s) if s.day != today => {
                    *start = Some(DayStart { day: today, baseline: self.latest.borrow().clone() });
                }
                Some(_) => {}
            }
        }
        if let Ok(mut h) = self.history.lock() {
            if h.len() == HISTORY_LEN {
                h.pop_front();
            }
            h.push_back(sample.point.clone());
        }
        // `send_replace` stores the value even with no subscribers connected.
        self.latest.send_replace(Some(Arc::new(sample)));
    }
}

/// Cumulative registry readings at one instant, kept long enough to diff.
struct Reading {
    at: Instant,
    ticks: u64,
    overruns: u64,
    tick: Buckets,
    feed_ticks: u64,
    http_total: u64,
    http: Buckets,
    kotak: Buckets,
    kotak_by_endpoint: Vec<(String, Buckets)>,
    http_by_route: Vec<(String, Buckets)>,
    mutex: Buckets,
    db: Buckets,
    signals: u64,
}

impl Reading {
    /// A zero reading — the baseline before any reading exists.
    fn empty(at: Instant) -> Self {
        Self {
            at,
            ticks: 0,
            overruns: 0,
            tick: Buckets::default(),
            feed_ticks: 0,
            http_total: 0,
            http: Buckets::default(),
            kotak: Buckets::default(),
            kotak_by_endpoint: Vec::new(),
            http_by_route: Vec::new(),
            mutex: Buckets::default(),
            db: Buckets::default(),
            signals: 0,
        }
    }

    fn take() -> Self {
        Self {
            at: Instant::now(),
            ticks: telemetry::ENGINE_TICKS.get(),
            overruns: telemetry::ENGINE_TICK_OVERRUNS.get(),
            tick: telemetry::buckets(&*telemetry::ENGINE_TICK_SECONDS),
            feed_ticks: telemetry::FEED_TICKS.get(),
            http_total: telemetry::counter_total(&telemetry::HTTP_REQUESTS),
            http: telemetry::buckets(&*telemetry::HTTP_REQUEST_SECONDS),
            kotak: telemetry::buckets(&*telemetry::KOTAK_REQUEST_SECONDS),
            kotak_by_endpoint: telemetry::buckets_by(&telemetry::KOTAK_REQUEST_SECONDS, "endpoint"),
            http_by_route: telemetry::buckets_by(&telemetry::HTTP_REQUEST_SECONDS, "route"),
            mutex: telemetry::buckets(&*telemetry::KOTAK_MUTEX_WAIT),
            db: telemetry::buckets(&*telemetry::DB_WRITE_SECONDS),
            signals: telemetry::counter_total(&telemetry::SIGNALS_RECEIVED),
        }
    }
}

/// The oldest reading still inside `window` of `now` (the best baseline for a
/// rate over that window), or the oldest one kept.
fn baseline(readings: &VecDeque<Reading>, now: Instant, window: Duration) -> Option<&Reading> {
    readings
        .iter()
        .find(|r| now.duration_since(r.at) <= window)
        .or_else(|| readings.front())
}

fn per_sec(later: u64, earlier: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return 0.0;
    }
    later.saturating_sub(earlier) as f64 / secs
}

fn ms(v: Option<f64>) -> Option<f64> {
    v.map(|s| (s * 1000.0 * 100.0).round() / 100.0)
}

fn mb(bytes: u64) -> f64 {
    (bytes as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0
}

fn age(ts: f64, now: f64) -> Option<f64> {
    (ts > 0.0).then(|| (now - ts).max(0.0))
}

fn by(c: &telemetry::CounterFamily, label: &str) -> BTreeMap<String, u64> {
    telemetry::counter_by(c, label).into_iter().collect()
}

fn state_label(s: &TradeState) -> &'static str {
    match s {
        TradeState::WaitingForEntry => "waiting",
        TradeState::Active => "active",
        TradeState::Target1Hit => "target1",
        TradeState::Closed => "closed",
    }
}

fn position_row(p: &MonitoredPosition, prices: &dashmap::DashMap<String, f64>, now: f64) -> PositionRow {
    // Same LTP precedence the engine uses for display: the live feed for the
    // resolved scrip, else the last LTP the engine recorded on the position.
    let ltp = p
        .ws_scrip_key
        .as_deref()
        .and_then(|k| prices.get(k).map(|v| *v))
        .filter(|v| *v > 0.0)
        .or(p.ltp);
    let open = matches!(p.state, TradeState::Active | TradeState::Target1Hit) && p.executed_qty > 0;
    PositionRow {
        id: p.id.clone(),
        instrument: p.signal.instrument_name.clone(),
        strike: p.signal.strike,
        option_type: p.signal.option_type.clone(),
        state: state_label(&p.state),
        qty: p.executed_qty,
        avg_price: p.avg_buy_price,
        entry_price: p.signal.entry_price,
        current_sl: p.current_sl,
        ltp,
        mtm: if open { ltp.map(|l| (l - p.avg_buy_price) * p.executed_qty as f64) } else { None },
        ltp_age_secs: p
            .ws_scrip_key
            .as_deref()
            .and_then(telemetry::scrip_last_tick)
            .and_then(|t| age(t, now)),
        halted: p.live_halt.is_some(),
    }
}

/// Per-key totals, error counts and windowed p95 for a counter family whose
/// second label (`result` / `status`) is a status class. Busiest first.
fn endpoint_stats(
    requests: &telemetry::CounterFamily,
    key_label: &str,
    is_error: impl Fn(&str) -> bool,
    earlier: &[(String, Buckets)],
    later: &[(String, Buckets)],
) -> Vec<EndpointStat> {
    let status_label = if key_label == "route" { "status" } else { "result" };
    let mut out: Vec<EndpointStat> = Vec::new();
    for ((key, status), n) in telemetry::counter_by_pair(requests, key_label, status_label) {
        let errors = if is_error(&status) { n } else { 0 };
        match out.iter_mut().find(|e| e.endpoint == key) {
            Some(e) => {
                e.total += n;
                e.errors += errors;
            }
            None => out.push(EndpointStat { endpoint: key, total: n, errors, p95_ms: None }),
        }
    }
    for e in &mut out {
        let find = |set: &[(String, Buckets)]| {
            set.iter().find(|(k, _)| *k == e.endpoint).map(|(_, b)| b.clone()).unwrap_or_default()
        };
        e.p95_ms = ms(telemetry::quantile_between(&find(earlier), &find(later), 0.95));
    }
    out.sort_by(|a, b| b.total.cmp(&a.total));
    out
}

/// Spawn the sampler. Runs for the life of the process.
pub fn spawn_sampler(state: AppState) {
    tokio::spawn(async move {
        let mut sys = System::new();
        let pid = get_current_pid().ok();
        let mut readings: VecDeque<Reading> = VecDeque::new();
        let mut mode: &'static str = "paper";
        let mut kill_switch = false;
        let mut kotak_connected = false;
        let mut memory_alarm = MemoryAlarm::new(memory_warn_mb());

        let mut every = tokio::time::interval(SAMPLE_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let now_unix = telemetry::unix_now();

            // --- Process / host -------------------------------------------
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            if let Some(pid) = pid {
                sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            }
            let process = pid.and_then(|p| sys.process(p));
            let process_cpu = process.map(|p| p.cpu_usage()).unwrap_or(0.0);
            let rss = process.map(|p| p.memory()).unwrap_or(0);
            let virt = process.map(|p| p.virtual_memory()).unwrap_or(0);
            let uptime = process.map(|p| p.run_time()).unwrap_or(0);
            let host_cpu = sys.global_cpu_usage();
            // sysinfo reports 0 available memory on some macOS versions;
            // fall back to total − used there (Linux reads MemAvailable).
            let host_available = match sys.available_memory() {
                0 => sys.total_memory().saturating_sub(sys.used_memory()),
                v => v,
            };
            let load = System::load_average();
            // Once per crossing, both ways: the log, and the dashboard's log
            // (which outlives a restart) — not on every sample it stays over.
            let rss_mb = mb(rss);
            match memory_alarm.observe(rss_mb) {
                Some(true) => {
                    tracing::warn!(
                        rss_mb, threshold_mb = memory_alarm.threshold_mb,
                        "Server memory above MEMORY_WARN_MB — an OOM kill would stop the engine with nothing restarting it"
                    );
                    crate::routes::auth_kotak::send_log(&state.db_tx, &state.log_tx, "WARN", serde_json::json!({
                        "event": "MEMORY_WARN",
                        "level": "WARN",
                        "rss_mb": rss_mb,
                        "threshold_mb": memory_alarm.threshold_mb,
                        "message": format!(
                            "Server memory {rss_mb} MB, above MEMORY_WARN_MB {} MB — an OOM kill would stop the engine",
                            memory_alarm.threshold_mb
                        ),
                    })).await;
                }
                Some(false) => {
                    tracing::info!(rss_mb, threshold_mb = memory_alarm.threshold_mb, "Server memory back under MEMORY_WARN_MB");
                    crate::routes::auth_kotak::send_log(&state.db_tx, &state.log_tx, "INFO", serde_json::json!({
                        "event": "MEMORY_WARN_CLEARED",
                        "level": "INFO",
                        "rss_mb": rss_mb,
                        "threshold_mb": memory_alarm.threshold_mb,
                        "message": format!("Server memory {rss_mb} MB, back under MEMORY_WARN_MB {} MB", memory_alarm.threshold_mb),
                    })).await;
                }
                None => {}
            }
            telemetry::PROCESS_CPU_PERCENT.set(process_cpu as f64);
            telemetry::PROCESS_RSS_BYTES.set(rss as i64);
            telemetry::PROCESS_VIRTUAL_BYTES.set(virt as i64);
            telemetry::PROCESS_UPTIME.set(uptime as i64);
            telemetry::HOST_CPU_PERCENT.set(host_cpu as f64);
            let mem = &telemetry::HOST_MEMORY_BYTES;
            telemetry::set_int(mem, &["total"], sys.total_memory() as i64);
            telemetry::set_int(mem, &["available"], host_available as i64);
            telemetry::set_int(mem, &["swap_total"], sys.total_swap() as i64);
            telemetry::set_int(mem, &["swap_used"], sys.used_swap() as i64);
            telemetry::set_float(&telemetry::HOST_LOAD, &["1m"], load.one);
            telemetry::set_float(&telemetry::HOST_LOAD, &["5m"], load.five);
            telemetry::set_float(&telemetry::HOST_LOAD, &["15m"], load.fifteen);

            let rt = tokio::runtime::Handle::current().metrics();
            telemetry::TOKIO_WORKERS.set(rt.num_workers() as i64);
            telemetry::TOKIO_ALIVE_TASKS.set(rt.num_alive_tasks() as i64);
            telemetry::TOKIO_GLOBAL_QUEUE.set(rt.global_queue_depth() as i64);

            // --- Queues, pools, streams ------------------------------------
            let db_capacity = state.db_tx.max_capacity();
            let db_depth = db_capacity.saturating_sub(state.db_tx.capacity());
            telemetry::DB_QUEUE_DEPTH.set(db_depth as i64);
            telemetry::DB_QUEUE_CAPACITY.set(db_capacity as i64);
            let pool_size = state.db_pool.size();
            let pool_idle = state.db_pool.num_idle();
            telemetry::set_int(&telemetry::DB_POOL_CONNECTIONS, &["idle"], pool_idle as i64);
            telemetry::set_int(
                &telemetry::DB_POOL_CONNECTIONS, &["in_use"],
                (pool_size as i64 - pool_idle as i64).max(0),
            );
            let log_clients = state.log_tx.receiver_count() as i64;
            let pulse_clients = state.pulse.latest.receiver_count() as i64;
            telemetry::set_int(&telemetry::SSE_CLIENTS, &["logs"], log_clients);
            telemetry::set_int(&telemetry::SSE_CLIENTS, &["telemetry"], pulse_clients);

            // --- Trading state (never waited on) ---------------------------
            if let Ok(cfg) = state.trading_cfg.try_read() {
                mode = telemetry::mode_label(&cfg.mode);
                kill_switch = cfg.kill_switch_active;
            }
            telemetry::set_int(&telemetry::TRADING_MODE, &["live"], (mode == "live") as i64);
            telemetry::set_int(&telemetry::TRADING_MODE, &["paper"], (mode == "paper") as i64);
            telemetry::KILL_SWITCH_ACTIVE.set(kill_switch as i64);

            // Held across every broker call; keep the last answer when busy.
            if let Ok(k) = state.kotak.try_lock() {
                kotak_connected = k.is_some();
            }
            let kotak_healthy = kotak_connected && state.kotak_session_healthy.load(Ordering::Relaxed);
            telemetry::KOTAK_CONNECTED.set(kotak_connected as i64);
            telemetry::KOTAK_SESSION_HEALTHY.set(kotak_healthy as i64);

            let (positions, unrealized_mtm, positions_table) = match state.positions.try_read() {
                Ok(g) => {
                    let mut counts = PositionCounts::default();
                    let rows: Vec<PositionRow> =
                        g.iter().map(|p| position_row(p, &state.prices, now_unix)).collect();
                    for r in &rows {
                        match r.state {
                            "waiting" => counts.waiting += 1,
                            "active" => counts.active += 1,
                            "target1" => counts.target1 += 1,
                            _ => counts.closed += 1,
                        }
                    }
                    drop(g);
                    // `+ 0.0` turns the empty sum's -0.0 into 0.0.
                    let mtm: f64 = rows.iter().filter_map(|r| r.mtm).sum::<f64>() + 0.0;
                    telemetry::set_int(&telemetry::POSITIONS, &["waiting"], counts.waiting as i64);
                    telemetry::set_int(&telemetry::POSITIONS, &["active"], counts.active as i64);
                    telemetry::set_int(&telemetry::POSITIONS, &["target1"], counts.target1 as i64);
                    telemetry::set_int(&telemetry::POSITIONS, &["closed"], counts.closed as i64);
                    telemetry::UNREALIZED_MTM.set(mtm);
                    (Some(counts), Some((mtm * 100.0).round() / 100.0), Some(rows))
                }
                Err(_) => (None, None, None),
            };

            // --- Rates and windowed percentiles ----------------------------
            let cur = Reading::take();
            let rate_base = baseline(&readings, cur.at, RATE_WINDOW);
            let q_base = baseline(&readings, cur.at, QUANTILE_WINDOW);
            let empty = Reading::empty(cur.at);
            let rb = rate_base.unwrap_or(&empty);
            let qb = q_base.unwrap_or(&empty);
            let rate_elapsed = cur.at.duration_since(rb.at);

            let p95 = |a: &Buckets, b: &Buckets| ms(telemetry::quantile_between(a, b, 0.95));

            let kotak_endpoints = endpoint_stats(
                &telemetry::KOTAK_REQUESTS, "endpoint", |result| result != "2xx",
                &qb.kotak_by_endpoint, &cur.kotak_by_endpoint,
            );
            let kotak_requests_total: u64 = kotak_endpoints.iter().map(|e| e.total).sum();
            let kotak_errors_total: u64 = kotak_endpoints.iter().map(|e| e.errors).sum();

            let http_by_status = telemetry::counter_by(&telemetry::HTTP_REQUESTS, "status");
            let http_total: u64 = http_by_status.iter().map(|(_, v)| v).sum();
            let share = |class: &str| -> Option<f64> {
                (http_total > 0).then(|| {
                    let n: u64 = http_by_status.iter().filter(|(k, _)| k == class).map(|(_, v)| v).sum();
                    (n as f64 / http_total as f64 * 10000.0).round() / 100.0
                })
            };
            let mut http_routes = endpoint_stats(
                &telemetry::HTTP_REQUESTS, "route", |status| status == "5xx",
                &qb.http_by_route, &cur.http_by_route,
            );
            http_routes.truncate(10);

            let upstox_errors_total = telemetry::counter_by(&telemetry::UPSTOX_REQUESTS, "result")
                .into_iter()
                .filter(|(k, _)| k != "ok")
                .map(|(_, v)| v)
                .sum();

            let orders_by_result = by(&telemetry::ORDERS, "result");

            let sample = Sample {
                point: Point {
                    ts: now_unix,
                    process_cpu_pct: process_cpu,
                    host_cpu_pct: host_cpu,
                    process_rss_mb: mb(rss),
                    host_available_mb: mb(host_available),
                    tick_p95_ms: p95(&qb.tick, &cur.tick),
                    ticks_per_sec: per_sec(cur.ticks, rb.ticks, rate_elapsed),
                    feed_ticks_per_sec: per_sec(cur.feed_ticks, rb.feed_ticks, rate_elapsed),
                    http_req_per_sec: per_sec(cur.http_total, rb.http_total, rate_elapsed),
                    http_p95_ms: p95(&qb.http, &cur.http),
                    kotak_p95_ms: p95(&qb.kotak, &cur.kotak),
                    db_queue_depth: db_depth,
                    signals_per_min: per_sec(cur.signals, rb.signals, rate_elapsed) * 60.0,
                },
                mode,
                kill_switch,
                process_uptime_secs: uptime,
                process_virtual_mb: mb(virt),
                memory_warn_mb: memory_alarm.threshold_mb,
                memory_warn: memory_alarm.over,
                host_memory_total_mb: mb(sys.total_memory()),
                host_swap_total_mb: mb(sys.total_swap()),
                host_swap_used_mb: mb(sys.used_swap()),
                load_average: [load.one, load.five, load.fifteen],
                cpu_cores: sys.cpus().len(),
                tokio_workers: rt.num_workers(),
                tokio_alive_tasks: rt.num_alive_tasks(),
                tokio_global_queue: rt.global_queue_depth(),
                engine_heartbeat_age_secs: age(telemetry::ENGINE_LAST_TICK.get(), now_unix),
                tick_p50_ms: ms(telemetry::quantile_between(&qb.tick, &cur.tick, 0.5)),
                tick_overruns_total: cur.overruns,
                tick_overruns_window: cur.overruns.saturating_sub(qb.overruns),
                live_startup_reconciled: telemetry::LIVE_STARTUP_RECONCILED.get() == 1,
                live_errors_total: telemetry::ENGINE_LIVE_ERRORS.get(),
                live_halts_total: telemetry::LIVE_HALTS.get(),
                positions,
                unrealized_mtm,
                positions_table,
                trades: telemetry::counter_by(&telemetry::TRADES, "mode")
                    .into_iter()
                    .collect(),
                signals_received_total: cur.signals,
                signal_outcomes: by(&telemetry::SIGNAL_OUTCOMES, "outcome"),
                signals_lagged_total: telemetry::SIGNALS_LAGGED.get(),
                orders_by_result,
                orders_by_side: by(&telemetry::ORDERS, "side"),
                order_cancels: by(&telemetry::ORDER_CANCELS, "result"),
                order_book_poll_failures_total: telemetry::ORDER_BOOK_POLL_FAILURES.get(),
                kotak_mutex_wait_p95_ms: p95(&qb.mutex, &cur.mutex),
                kotak_connected,
                kotak_session_healthy: kotak_healthy,
                kotak_requests_total,
                kotak_errors_total,
                kotak_endpoints,
                kotak_relogins: by(&telemetry::KOTAK_RELOGINS, "result"),
                feed_connected: telemetry::FEED_CONNECTED.get() == 1,
                feed_last_tick_age_secs: age(telemetry::FEED_LAST_TICK.get(), now_unix),
                feed_subscriptions: telemetry::FEED_SUBSCRIPTIONS.get(),
                feed_ticks_total: cur.feed_ticks,
                feed_bridge_restarts_total: telemetry::FEED_BRIDGE_RESTARTS.get(),
                telegram_connected: telemetry::TELEGRAM_CONNECTED.get() > 0,
                telegram_messages_total: telemetry::counter_total(&telemetry::TELEGRAM_MESSAGES),
                telegram_parse: by(&telemetry::TELEGRAM_PARSE, "result"),
                db_queue_capacity: db_capacity,
                db_write_p95_ms: p95(&qb.db, &cur.db),
                db_write_errors_total: telemetry::counter_total(&telemetry::DB_WRITE_ERRORS),
                db_pool_size: pool_size,
                db_pool_idle: pool_idle,
                upstox_configured: state.market.configured,
                upstox_last_success_age_secs: age(telemetry::UPSTOX_LAST_SUCCESS.get(), now_unix),
                upstox_errors_total,
                http_requests_total: http_total,
                http_4xx_pct: share("4xx"),
                http_5xx_pct: share("5xx"),
                http_in_flight: telemetry::HTTP_IN_FLIGHT.get(),
                http_routes,
                sse_clients: log_clients + pulse_clients,
            };
            state.pulse.publish(sample);
            telemetry::prune_scrip_ticks(SCRIP_TICK_MAX_AGE);

            readings.push_back(cur);
            while readings
                .front()
                .is_some_and(|r| r.at.elapsed() > QUANTILE_WINDOW + SAMPLE_EVERY)
            {
                readings.pop_front();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_alarm_fires_once_per_crossing() {
        let mut alarm = MemoryAlarm::new(600.0);
        assert!(!alarm.over);
        assert_eq!(alarm.observe(500.0), None);
        assert_eq!(alarm.observe(599.9), None);
        // Crossing up: once, however long it stays over.
        assert_eq!(alarm.observe(600.0), Some(true));
        assert_eq!(alarm.observe(650.0), None);
        assert_eq!(alarm.observe(900.0), None);
        assert!(alarm.over);
        // Hovering just under the threshold doesn't clear it...
        assert_eq!(alarm.observe(590.0), None);
        assert!(alarm.over);
        // ...5% under does, once; then a new crossing warns again.
        assert_eq!(alarm.observe(569.0), Some(false));
        assert_eq!(alarm.observe(560.0), None);
        assert_eq!(alarm.observe(610.0), Some(true));
    }

    #[test]
    fn the_threshold_is_read_defensively() {
        assert_eq!(parse_memory_warn_mb(None), DEFAULT_MEMORY_WARN_MB);
        assert_eq!(parse_memory_warn_mb(Some(" 750 ")), 750.0);
        assert_eq!(parse_memory_warn_mb(Some("512.5")), 512.5);
        for bad in ["", "abc", "0", "-5", "NaN", "inf"] {
            assert_eq!(parse_memory_warn_mb(Some(bad)), DEFAULT_MEMORY_WARN_MB, "{bad:?}");
        }
    }
}
