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
    action: string;
    entry_condition: string;
    entry_price: number;
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
  connected: boolean;
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
 * `AdoptManual` variant as `{ AdoptManual: { stop_loss, target } }`. */
export type ReconcileAction =
  | 'AdoptQty'
  | 'Close'
  | 'Ignore'
  | { AdoptManual: { stop_loss: number; target: number } };

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
  message: string;
  options: ReconcileOption[];
}

export interface ReconcileApplyItem {
  position_id: string | null;
  trading_symbol: string;
  action: ReconcileAction;
}

export type ScreenId = 'dashboard' | 'positions' | 'analytics' | 'portfolio' | 'settings';

export type TgStep = 'idle' | 'code' | 'twofa' | 'chats' | 'running';
