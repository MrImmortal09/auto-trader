// ---------------------------------------------------------------------------
// Types (mirror shared_domain structs)
// ---------------------------------------------------------------------------

export interface TradingConfig {
  max_trade_amount_inr: number;
  index_lots: number;
  other_lots: number;
  mode: string;
  brokerage_per_order: number;
  target_1_exit_pct: number;
  target_2_exit_pct: number;
  entry_market_protection: number;
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

export interface TelegramChat {
  id: number;
  name: string;
  kind: string;
}

export type ScreenId = 'dashboard' | 'positions' | 'analytics' | 'portfolio' | 'settings';

export type TgStep = 'idle' | 'code' | 'twofa' | 'chats' | 'running';
