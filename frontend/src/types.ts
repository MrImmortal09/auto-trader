// ---------------------------------------------------------------------------
// Types (mirror shared_domain structs)
// ---------------------------------------------------------------------------

export interface TradingConfig {
  max_trade_amount_inr: number;
  index_lots: number;
  other_lots: number;
  /** Per-index default lot count (key: index symbol, e.g. "NIFTY"). Falls back to `index_lots` when absent. */
  index_lots_by_symbol: Record<string, number>;
  mode: string;
  brokerage_per_order: number;
  target_1_exit_pct: number;
  target_2_exit_pct: number;
  entry_market_protection: number;
  /** Sell one lot at target 1, then trail an ever-extending target ladder for
   * the runner instead of exiting at the signal's fixed target 2. */
  dynamic_targeting: boolean;
  /** Multiplier applied to `diff = target1 - entry` when trailing the stop
   * under `dynamic_targeting`: stop = rung - diff * factor. 0 = tightest
   * (locks the rung itself), 1 = loosest (breakeven at entry on the first
   * rung). Unused unless `dynamic_targeting` is on. */
  dynamic_targeting_trail_factor: number;
  /** Multiplier applied to `diff = target1 - entry` when extending the next
   * rung under `dynamic_targeting`: next_rung = rung + diff * factor. 1.0
   * reproduces the original fixed `diff` spacing. Unused unless
   * `dynamic_targeting` is on. */
  dynamic_targeting_extension_factor: number;
  /** Extends `dynamic_targeting` to one-lot positions: instead of selling the
   * only lot at target 1 (a full exit), sell nothing and let the whole lot run
   * the dynamic ladder. Multi-lot positions are unaffected. Unused unless
   * `dynamic_targeting` is on. Applies in PAPER and LIVE. */
  dynamic_targeting_single_lot: boolean;
  /** Protect gains before target 1: once the peak LTP since entry covers
   * `pre_t1_trail_arm_pct` % of the entry→target-1 distance, the stop is
   * ratcheted up to `peak - diff * pre_t1_trail_factor` (never below the
   * signal's SL, never moved back down). Trades that reach target 1 are
   * untouched. Applies in both PAPER and LIVE. */
  pre_t1_trailing: boolean;
  /** Percent of the entry→target-1 distance the peak must cover before the
   * pre-T1 trail arms. Unused unless `pre_t1_trailing` is on. */
  pre_t1_trail_arm_pct: number;
  /** Trail distance once armed, as a multiplier on `diff`:
   * stop = peak - diff * factor. 0 = tightest, 1 = loosest. Unused unless
   * `pre_t1_trailing` is on. */
  pre_t1_trail_factor: number;
  /** When true, trading is halted for the day and open positions have been liquidated. */
  kill_switch_active?: boolean;
  /** Entry buy window as a percent of the entry→target-1 distance. 0 (default)
   * buys the moment the trigger is crossed; >0 refuses to chase past
   * `entry ± pct% * (target1 - entry)` and waits for a pull-back into that
   * window. Clamped to [0, 100] on save. */
  entry_window_pct: number;
  /** Size option entries on the VWAP + PCR indicator at the moment of buying
   * (parent index's last closed check; BANKNIFTY and SENSEX use their own,
   * everything else follows NIFTY): strongly agrees (score ±2, PCR and VWAP
   * both) → × `indicator_strong_agree_multiplier`, agrees (score ±1) →
   * × `indicator_agree_multiplier`, neutral / no fresh reading →
   * × `indicator_neutral_multiplier`, disagrees → the call is skipped. Off by
   * default. */
  indicator_sizing: boolean;
  /** Lot multiplier when the indicator agrees with the call on a ±1 score
   * (one of PCR / VWAP, the other neutral). [0.1, 5]. */
  indicator_agree_multiplier: number;
  /** Lot multiplier when the indicator strongly agrees with the call: a ±2
   * score, PCR and VWAP both on its side. [0.1, 5]. */
  indicator_strong_agree_multiplier: number;
  /** Lot multiplier when the indicator is neutral or has no fresh reading. [0.1, 5]. */
  indicator_neutral_multiplier: number;
  /** Minutes per indicator check: 5, 15, 30 or 60. */
  indicator_interval_min: number;
  /** "chg" (change in OI since yesterday's close) or "total" (total OI). */
  indicator_basis: 'chg' | 'total';
  /** Strikes each side of ATM counted: 5, 10, 20, or 0 = all. */
  indicator_strike_window: number;
  /** Option Signal = BUY when PCR is above this. */
  indicator_pcr_buy_above: number;
  /** Option Signal = SELL when PCR is below this. */
  indicator_pcr_sell_below: number;
  /** VWAP Signal is neutral within ± this % of VWAP. [0, 5]. */
  indicator_vwap_band_pct: number;
  /** A call not bought within this many minutes of arriving is dropped. 0 = off. [0, 390]. */
  call_validity_minutes: number;
  /** 09:16 check always NEUTRAL, then 5-min checks until the first regular check. */
  indicator_early_checks: boolean;
  /** Disagreeing indicator: keep the call waiting (true) or drop it (false). */
  indicator_wait_on_disagree: boolean;
  /** Top a NEUTRAL-sized position up once the indicator agrees — which
   * reading does it, and to what size, is `indicator_topup_mode`. */
  indicator_topup: boolean;
  /** The top-up rule while `indicator_topup` is on. */
  indicator_topup_mode: TopupMode;
  /** A Telegram instruction that lowers a call's stop is applied only on a
   * strong indicator agree (+2 CE / −2 PE); raising is always applied. */
  indicator_sl_lower_gate: boolean;
}

/** `strong_to_agree` (default): only a strong agree tops up, to the Agrees
 * size. `strong_to_strong`: only a strong agree, to the Strong size. `tier`:
 * any agreement, to that reading's own size. `step`: as `tier`, then once more
 * to the Strong size when the reading turns strong. */
export type TopupMode = 'strong_to_agree' | 'strong_to_strong' | 'tier' | 'step';

/** `GET /api/market/indicator` — the engine's own verdicts, right now. */
export interface IndicatorScore {
  call: number;
  put: number;
  ratio: number | null;
  option: number;
  vwap: number;
  overall: number;
}
export type IndicatorBias = 'BULLISH' | 'BEARISH' | 'NEUTRAL';
export interface IndicatorReading {
  index: string;
  check_time: string;
  ts: string;
  score: IndicatorScore;
  bias: IndicatorBias;
  /** The 09:16 opening check under the early-check rule: bias is forced NEUTRAL. */
  opening?: boolean;
}
export interface CallVerdict {
  action: 'SCALE' | 'SKIP' | 'WAIT';
  multiplier: number | null;
  base_lots: number;
  lots: number | null;
  note: string;
}
export interface IndicatorIndex {
  index: string;
  reading: IndicatorReading | null;
  bias: IndicatorBias;
  ce: CallVerdict;
  pe: CallVerdict;
}
export interface IndicatorPreview {
  configured: boolean;
  enabled: boolean;
  now: string;
  indices: IndicatorIndex[];
}

export interface PaperTrade {
  id: number;
  ticker: string;
  action: string;
  qty: number;
  executed_price: number;
  gross_value: number;
  brokerage: number;
  stt_charge: number;
  sebi_fee: number;
  stamp_duty: number;
  transaction_charge: number;
  gst: number;
  net_value: number;
  timestamp: string;
  exit_reason?: string;
  mode?: string;
  signal_id?: string;
  raw_message?: string;
}

export interface Portfolio {
  balance: number;
  /** "LIVE" | "PAPER" | "LIVE_UNAVAILABLE" — see backend Portfolio struct. */
  balance_source: string;
  /** IST time of the Kotak read behind a "LIVE" balance; absent otherwise. */
  balance_updated_at?: string | null;
  trades: PaperTrade[];
}

export interface HealthSnapshot {
  generated_at_ist: string;
  hostname: string | null;
  os_name: string | null;
  os_version: string | null;
  kernel_version: string | null;
  uptime_secs: number;
  cpu_cores: number;
  cpu_usage_pct: number;
  load_average: {
    one: number;
    five: number;
    fifteen: number;
  };
  memory: {
    total_mib: number;
    used_mib: number;
    free_mib: number;
  };
  swap: {
    total_mib: number;
    used_mib: number;
    free_mib: number;
  };
  current_process: {
    pid: string;
    name: string;
    cpu_usage_pct: number;
    memory_mib: number;
    virtual_memory_mib: number;
    run_time_secs: number;
  } | null;
  /** Whether the backend has AUTH_SECRET set. If false, every authenticated
   * API route fails with 500 — see auth_middleware in server/src/main.rs. */
  auth_secret_configured: boolean;
}

export interface MonitoredPosition {
  id: string;
  signal: {
    instrument_name: string;
    /** Option strike, e.g. 58200 — absent for equity / futures signals. */
    strike?: number | null;
    /** "CE" | "PE" for options; absent otherwise. */
    option_type?: string | null;
    action: string;
    entry_condition: string;
    entry_price: number;
    /** Top of an entry range (`140-145`); the engine buys only inside it. */
    entry_price_max?: number | null;
    stop_loss: number;
    targets: number[];
  };
  state: string;
  current_sl: number;
  /** Dynamic-targeting runner: the next rung to watch for once target 1 has
   * been hit, if `TradingConfig.dynamic_targeting` was on when it hit. `null`
   * means the position exits at the signal's fixed target 2 as usual. */
  next_dynamic_target?: number | null;
  /** Dynamic-targeting runner: how many rungs have been hit (1 after target
   * 1, 2 after the next extension, …). 0/absent for non-dynamic positions —
   * `state` stays `"Target1Hit"` regardless, this only drives the display
   * label ("Target2Hit", "Target3Hit", …). */
  dynamic_rung_number?: number;
  executed_qty: number;
  avg_buy_price: number;
  override_qty: number | null;
  resolved_order?: {
    quantity?: string;
    trading_symbol?: string;
    exchange_segment?: string;
    order_type?: string;
    product_code?: string;
    validity?: string;
    transaction_type?: string;
    trigger_price?: string;
    price?: string;
  };
  ltp?: number;
  ws_scrip_key?: string | null;
  /** `[low, high]` premium window the engine will buy a WaitingForEntry row in,
   * from the live `entry_window_pct` config. Absent when the feature is off. */
  entry_zone?: [number, number] | null;
  /** IST `HH:MM` this still-unbought call stops being valid (call validity window). */
  call_expires_at?: string | null;
  /** Opened by hand from the Execute screen: market entry, no stop, no target. */
  manual?: boolean;
  /** A manual add still waiting to be sent. */
  manual_add?: { lots: number; requested_at: string } | null;
  /** A scalp's stop and target set from a v2 chart; absent while none is set. */
  chart?: ChartLevels;
}

/** A chart stop or target: on the option's own price, or on its index. */
export interface ChartLevel {
  price: number;
  trigger: 'premium' | 'index';
}

/** A scalp's chart stop and target. Reaching either sells the whole holding. */
export interface ChartLevels {
  stop: ChartLevel | null;
  target: ChartLevel | null;
}

export interface KotakForm {
  server_base: string;
  access_token: string;
  mobile_number: string;
  ucc: string;
  totp: string;
  mpin: string;
}

export interface KotakStatus {
  /** A session exists AND the watchdog's last probe of it succeeded. */
  connected: boolean;
  /** A session object is held in memory (may or may not still work). */
  session_present?: boolean;
  /** The watchdog's last probe of the held session succeeded. */
  session_healthy?: boolean;
  /** All five KOTAK_* env vars are set, so "Auto Connect" needs no form input. */
  has_env_credentials: boolean;
  /** KOTAK_TOTP_SECRET / KOTAK_TOTP_HASH is set, so the TOTP field can be left blank. */
  has_totp_secret: boolean;
  auto_login_enabled: boolean;
  /** `has_env_credentials && auto_login_enabled` — whether unattended
   * auto-login will actually run at the next scheduled trigger. */
  auto_login_ready: boolean;
  /** Set when `auto_login_ready` is false — why (missing env var(s), or
   * KOTAK_AUTO_LOGIN=false). */
  auto_login_reason?: string | null;
  masked_ucc?: string | null;
}

export interface TelegramChat {
  id: number;
  name: string;
  kind: string;
}

// ---------------------------------------------------------------------------
// On-demand broker reconciliation ("Sync with Kotak")
// ---------------------------------------------------------------------------

export type ReconcileCategory =
  | 'Matches'
  | 'QtyReduced'
  | 'QtyZero'
  | 'QtyIncreased'
  | 'UnexplainedExposure'
  | 'DuplicateAmbiguous';

export type ReconcileActionKind = 'AdoptQty' | 'Close' | 'Ignore' | 'AdoptManual';

/** Mirrors the Rust `ReconcileAction` enum's default (externally-tagged) serde
 * shape: the three unit variants serialize as bare strings, the data-carrying
 * `AdoptManual` variant as `{ AdoptManual: { stop_loss, target, avg_buy_price } }`.
 * `avg_buy_price` of 0 tells the backend to fall back to the broker's figure. */
export type ReconcileAction =
  | 'AdoptQty'
  | 'Close'
  | 'Ignore'
  | { AdoptManual: { stop_loss: number; target: number; avg_buy_price: number } };

export interface ReconcileOption {
  action: ReconcileAction;
  label: string;
  recommended: boolean;
}

export interface ReconcileFinding {
  position_id: string | null;
  trading_symbol: string;
  instrument: string;
  category: ReconcileCategory;
  engine_qty: number;
  broker_qty: number;
  /** Broker's same-day average buy price for the symbol; 0 when unavailable. */
  broker_avg_price: number;
  message: string;
  options: ReconcileOption[];
}

export interface ReconcileApplyItem {
  position_id: string | null;
  trading_symbol: string;
  action: ReconcileAction;
}

export type ScreenId = 'dashboard' | 'execute' | 'positions' | 'analytics' | 'market' | 'portfolio' | 'system' | 'settings';

export type TgStep = 'idle' | 'resuming' | 'code' | 'twofa' | 'chats' | 'running';

// ---------------------------------------------------------------------------
// System pulse (GET /api/telemetry/snapshot, SSE /api/telemetry/stream) —
// mirrors `backend/server/src/pulse.rs`.
// ---------------------------------------------------------------------------

/** The charted subset of a sample; the backend keeps 15 minutes of these. */
export interface TelemetryPoint {
  /** Unix seconds. */
  ts: number;
  process_cpu_pct: number;
  host_cpu_pct: number;
  process_rss_mb: number;
  host_available_mb: number;
  tick_p95_ms: number | null;
  ticks_per_sec: number;
  feed_ticks_per_sec: number;
  http_req_per_sec: number;
  http_p95_ms: number | null;
  kotak_p95_ms: number | null;
  db_queue_depth: number;
  signals_per_min: number;
}

export interface TelemetryEndpointStat {
  endpoint: string;
  total: number;
  errors: number;
  p95_ms: number | null;
}

export interface TelemetryPositionRow {
  id: string;
  instrument: string;
  strike: number | null;
  option_type: string | null;
  state: 'waiting' | 'active' | 'target1' | 'closed';
  qty: number;
  avg_price: number;
  entry_price: number;
  current_sl: number;
  ltp: number | null;
  mtm: number | null;
  ltp_age_secs: number | null;
  halted: boolean;
}

export interface TelemetrySample extends TelemetryPoint {
  mode: 'live' | 'paper' | 'other';
  kill_switch: boolean;

  process_uptime_secs: number;
  process_virtual_mb: number;
  host_memory_total_mb: number;
  host_swap_total_mb: number;
  host_swap_used_mb: number;
  load_average: [number, number, number];
  cpu_cores: number;
  tokio_workers: number;
  tokio_alive_tasks: number;
  tokio_global_queue: number;

  engine_heartbeat_age_secs: number | null;
  tick_p50_ms: number | null;
  tick_overruns_total: number;
  tick_overruns_window: number;
  live_startup_reconciled: boolean;
  live_errors_total: number;
  live_halts_total: number;
  /** null when the engine held the positions lock at sample time. */
  positions: { waiting: number; active: number; target1: number; closed: number } | null;
  unrealized_mtm: number | null;
  positions_table: TelemetryPositionRow[] | null;
  trades: Record<string, number>;

  signals_received_total: number;
  signal_outcomes: Record<string, number>;
  signals_lagged_total: number;

  orders_by_result: Record<string, number>;
  orders_by_side: Record<string, number>;
  order_cancels: Record<string, number>;
  order_book_poll_failures_total: number;
  kotak_mutex_wait_p95_ms: number | null;
  kotak_connected: boolean;
  kotak_session_healthy: boolean;
  kotak_requests_total: number;
  kotak_errors_total: number;
  kotak_endpoints: TelemetryEndpointStat[];
  kotak_relogins: Record<string, number>;

  feed_connected: boolean;
  feed_last_tick_age_secs: number | null;
  feed_subscriptions: number;
  feed_ticks_total: number;
  feed_bridge_restarts_total: number;

  telegram_connected: boolean;
  telegram_messages_total: number;
  telegram_parse: Record<string, number>;

  db_queue_capacity: number;
  db_write_p95_ms: number | null;
  db_write_errors_total: number;
  db_pool_size: number;
  db_pool_idle: number;

  upstox_configured: boolean;
  upstox_last_success_age_secs: number | null;
  upstox_errors_total: number;

  http_requests_total: number;
  http_4xx_pct: number | null;
  http_5xx_pct: number | null;
  http_in_flight: number;
  http_routes: TelemetryEndpointStat[];
  sse_clients: number;
}

export interface TelemetrySnapshot {
  latest: TelemetrySample | null;
  history: TelemetryPoint[];
  sample_every_secs: number;
}

// ---------------------------------------------------------------------------
// Execute screen (SSE /api/execute/stream) — mirrors `backend/server/src/execute.rs`.
// ---------------------------------------------------------------------------

export interface ExecuteQuote {
  /** Broker trading symbol — what a BUY tap sends. */
  sym: string;
  ltp: number | null;
  /** Milliseconds since this contract's own last tick. */
  age_ms: number | null;
}

export interface ExecuteRow {
  strike: number;
  ce: ExecuteQuote | null;
  pe: ExecuteQuote | null;
}

export interface ExecuteBoard {
  index: string;
  /** `YYYY-MM-DD`: the expiry the rows are from. */
  expiry: string;
  days_to_expiry: number;
  /** Every expiry this index's options are listed for, earliest first. */
  expiries: string[];
  lot_size: number;
  spot: number | null;
  /** Live Kotak index tick, Upstox's 60 s poll, or put-call parity. */
  spot_source: 'kotak' | 'upstox' | 'parity' | null;
  atm: number | null;
  /** The strikes `rows` run from and to. */
  window: { from: number; to: number };
  /** The subscribed window: 7 strikes each side of ATM, or of the strike the
   * stream was asked to centre on. v1 shows 11 of them. */
  rows: ExecuteRow[];
}

export interface ExecutePosition {
  id: string;
  manual: boolean;
  index: string;
  strike: number | null;
  option_type: string | null;
  /** `YYYY-MM-DD`, once the contract is resolved. */
  expiry: string | null;
  symbol: string;
  lot_size: number;
  qty: number;
  avg: number;
  ltp: number | null;
  pnl: number | null;
  placing: boolean;
  adding: boolean;
  exiting: boolean;
  halted: boolean;
  /** Chart stop and target, when set from a v2 chart. */
  chart?: ChartLevels;
}

export interface ExecuteFrame {
  /** Unix ms. */
  ts: number;
  mode: string;
  kill_switch: boolean;
  /** Manual buys are accepted (09:15–15:30 IST, weekday). */
  market_open: boolean;
  feed: { connected: boolean; last_tick_age_ms: number | null };
  kotak_ready: boolean;
  indices: { index: string; spot: number | null; change_pct: number | null }[];
  board: ExecuteBoard | null;
  board_error: string | null;
  positions: ExecutePosition[];
  /** Engine errors from the last minute (`ts` on the frame's clock): the
   * underlying, and the position when the engine said which. */
  errors: { instrument: string; position_id: string | null; message: string; ts: number }[];
}
