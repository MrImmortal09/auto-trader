// What the v2 endpoints answer — mirrors backend/server/src/desk.rs and
// history.rs. v1's own types stay in src/types.ts.

export type Mode = 'PAPER' | 'LIVE';
export type Tone = 'profit' | 'loss' | 'neutral';

/** How a closed call ended, as its chip reads. */
export interface Outcome {
  label: string;
  tone: Tone;
}

export interface ChartLevel {
  price: number;
  trigger: 'premium' | 'index';
}

export interface ChartLevels {
  stop: ChartLevel | null;
  target: ChartLevel | null;
}

export interface Source {
  kind: 'telegram' | 'scalp' | 'other';
  title: string;
}

/** An open or waiting position, as a Desk row shows it. */
export interface DeskPosition {
  id: string;
  /** "NIFTY 25000 CE". */
  contract: string;
  index: string;
  strike: number | null;
  side: string | null;
  source: Source;
  state: 'waiting' | 'active' | 'target1';
  qty: number;
  lot_size: number;
  /** Lots a waiting call will buy. */
  lots: number;
  lots_set: boolean;
  avg: number;
  /** IST `HH:MM`: the first buy, or when a waiting call arrived. */
  since: string | null;
  ltp: number | null;
  pnl: number | null;
  pnl_pct: number | null;
  entry: number;
  entry_max: number | null;
  entry_condition: string;
  zone: [number, number] | null;
  valid_till: string | null;
  /** The live stop. */
  stop: number;
  /** The stop the call gave. */
  signal_stop: number;
  targets: number[];
  next_target: number | null;
  chart: ChartLevels;
  index_ltp: number | null;
  busy: 'halted' | 'exiting' | 'selling' | 'buying' | null;
}

export interface LegRow {
  id: number;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  t: string;
  action: string;
  qty: number;
  price: number;
  net: number;
  charges: number;
  reason: string | null;
}

/** One Telegram call or scalp, with its legs. */
export interface Call {
  key: string;
  channel: string;
  chat_id: number | null;
  signal_id: string | null;
  index: string;
  side: string | null;
  strike: number | null;
  opened_at: string;
  closed_at: string | null;
  bought_qty: number;
  sold_qty: number;
  pnl: number | null;
  charges: number;
  exit_reason: string | null;
  message: string | null;
  legs: LegRow[];
  /** How it ended, once sold. */
  outcome: Outcome | null;
}

export interface ClosedCall extends Call {
  /** When its Telegram message was sent, if the feed has it. */
  message_at: string | null;
}

export interface Wallet {
  balance: number | null;
  source: 'PAPER' | 'LIVE' | 'LIVE_UNAVAILABLE';
  updated_at: string | null;
}

export interface DeskKpis {
  net: number;
  booked: number;
  open: number;
  calls: number;
}

/** `GET /api/v2/desk`. */
export interface DeskAnswer {
  mode: Mode;
  kill_switch: boolean;
  wallet: Wallet;
  kpis: DeskKpis;
  open: DeskPosition[];
  waiting: DeskPosition[];
  /** Newest first. */
  closed: ClosedCall[];
}

export type FeedStatus =
  | { kind: 'waiting'; valid_till: string | null }
  | { kind: 'bought'; qty: number; price: number }
  | { kind: 'closed'; outcome: Outcome | null; pnl: number | null }
  | { kind: 'not_taken' }
  | { kind: 'skipped' }
  | { kind: 'not_a_call'; no_instrument: boolean }
  | { kind: 'stop_update'; price: number | null }
  | { kind: 'exit'; price: number | null };

export interface FeedItem {
  id: number;
  chat_id: number;
  channel: string;
  msg_id: number;
  sent_at: string;
  text: string;
  edited: boolean;
  reply_to: number | null;
  status: FeedStatus;
}

/** `GET /api/v2/feed`. */
export interface FeedAnswer {
  day: string;
  channels: { chat_id: number; title: string; messages: number }[];
  /** Newest first. */
  items: FeedItem[];
}

export type Level = 'ok' | 'down' | 'off';

export interface Health {
  level: Level;
  label: string;
}

export interface HealthSet {
  engine: Health;
  feed: Health;
  kotak: Health;
  telegram: Health;
  upstox: Health;
}

export interface Stopped {
  /** IST `HH:MM`. */
  at: string | null;
  positions_exited: number;
  entries_cancelled: number;
}

/** One trade row, for a toast. */
export interface Fill {
  id: number;
  t: string;
  action: string;
  qty: number;
  price: number;
  contract: string;
  channel: string;
  reason: string | null;
  pnl: number | null;
}

/** An index whose options are held open on their expiry day. */
export interface Expiring {
  index: string;
  series: 'weekly' | 'monthly';
  positions: number;
}

/** `GET /api/v2/status`. */
export interface StatusAnswer {
  now: string;
  mode: Mode;
  kill_switch: boolean;
  stopped: Stopped | null;
  market_open: boolean;
  market: string;
  health: HealthSet | null;
  /** Seconds since the price feed's last tick, while it's connected. */
  feed_age_secs: number | null;
  expiring: Expiring[];
  day_pnl: number;
  fills: Fill[];
  last_fill_id: number;
}

/** `GET /api/v2/flags`. */
export interface Flags {
  index_options_only: boolean;
  chart_stops: boolean;
}

export interface ChainSide {
  /** The broker's trading symbol — what a Buy sends. */
  symbol: string;
  /** Signed (a put's is negative); `assumed` when Upstox had none. */
  delta: { value: number; assumed: boolean };
}

/** `GET /api/positions`: the engine's own rows, the few fields the Trade panel's Orders tab reads. */
export interface RawPosition {
  id: string;
  manual?: boolean;
  state: string;
  executed_qty: number;
  signal: { instrument_name: string; strike: number | null; option_type: string | null };
  /** LIVE: the resting SL-M order at Kotak, when one rests. */
  sl_order_id?: string | null;
  sl_order_qty?: number;
  sl_order_trigger?: number;
}

/** `GET /api/v2/expiries?index=`: every expiry the index's options are listed for, earliest first. */
export interface ExpiriesAnswer {
  index: string;
  /** `YYYY-MM-DD`, IST. */
  today: string;
  expiries: { date: string; days_to_expiry: number; lot_size: number }[];
}

/** `GET /api/v2/chain?index=&expiry=`: one expiry's strikes (the nearest by default), as the Execute screen trades them. */
export interface ChainAnswer {
  index: string;
  expiry: string;
  lot_size: number;
  tick_size: number;
  delta_at: string | null;
  strikes: { strike: number; ce: ChainSide | null; pe: ChainSide | null }[];
}

export interface FillMark {
  id: number;
  t: string;
  action: string;
  qty: number;
  price: number;
  index: string;
  strike: number | null;
  side: string | null;
  scalp: boolean;
  reason: string | null;
}

export interface CallMark {
  id: number;
  t: string;
  channel: string | null;
  index: string;
  strike: number | null;
  side: string | null;
  text: string;
}

export interface TelegramLine {
  id: string;
  index: string;
  strike: number | null;
  side: string | null;
  expiry: string | null;
  state: 'waiting' | 'active' | 'target1';
  channel: string;
  qty: number;
  avg: number;
  entry: number;
  ltp: number | null;
  pnl: number | null;
  stop: number;
  targets: number[];
}

/** `GET /api/v2/marks`: today's fills, Telegram calls and Telegram positions. */
export interface MarksAnswer {
  day: string;
  fills: FillMark[];
  calls: CallMark[];
  telegram: TelegramLine[];
}

export type RangeId = 'today' | '7d' | '30d' | 'all';
export type GroupId = 'channel' | 'index' | 'cepe';

export interface Kpis {
  net: number;
  calls: number;
  wins: number;
  losses: number;
  win_rate: number | null;
  profit_factor: number | null;
  avg_per_call: number | null;
  max_drawdown: number;
  gross_profit: number;
  gross_loss: number;
  charges: number;
}

export interface CurvePoint {
  t: string;
  pnl: number;
  peak: number;
  drawdown: number;
}

export interface GroupRow {
  label: string;
  calls: number;
  wins: number;
  net: number;
  gross_profit: number;
  gross_loss: number;
  profit_factor: number | null;
}

export interface DayRow {
  date: string;
  calls: number;
  wins: number;
  net: number;
  expiries: string[];
}

/** `GET /api/v2/history`. */
export interface HistoryAnswer {
  mode: Mode;
  range: RangeId;
  from: string | null;
  to: string;
  kpis: Kpis;
  curve: CurvePoint[];
  group: GroupId;
  groups: GroupRow[];
  /** Newest first. */
  days: DayRow[];
  /** Newest first. */
  calls: Call[];
  more_calls: number;
}

// ── System (`GET /api/v2/system`) ───────────────────────────────────────── //

/** What became of today's calls; the parts add up to `calls`. */
export interface SignalCounts {
  calls: number;
  traded: number;
  waiting: number;
  expired: number;
  indicator: number;
  not_index: number;
  other: number;
}

export interface OrderCounts {
  /** LIVE: orders sent to Kotak. Paper: its fills. */
  sent: number;
  filled: number;
  rejected: number;
  cancelled: number;
  poll_failures: number;
}

export interface SystemAnswer {
  day: string;
  mode: Mode;
  /** `HH:MM` the server started, when that was today. */
  started_today: string | null;
  uptime_secs: number | null;
  version: string;
  /** The last Scrip Master download. */
  scrip_loaded_at: string | null;
  signals: SignalCounts;
  orders: OrderCounts;
  telegram: { listening: number | null; messages: number; last_at: string | null };
  kotak: { relogins: number; logged_in_at: string | null };
  upstox: { configured: boolean; token_expires: string | null };
}

// ── Market (v1's `/api/market/*`, unchanged) ────────────────────────────── //

export type MarketIndex = 'NIFTY' | 'BANKNIFTY' | 'SENSEX';
export type Bias = 'BULLISH' | 'BEARISH' | 'NEUTRAL';

export interface IndicatorScore {
  call: number;
  put: number;
  ratio: number | null;
  /** +1 buy, −1 sell, 0. */
  option: number;
  vwap: number;
  /** −2 … +2. */
  overall: number;
}

export interface IndicatorReading {
  index: string;
  /** The check's `HH:MM`. */
  check_time: string;
  /** When its snapshot was polled. */
  ts: string;
  score: IndicatorScore;
  bias: Bias;
  /** The opening 09:16 check, always neutral. */
  opening: boolean;
}

/** What a call arriving now would get. */
export interface CallVerdict {
  action: 'SCALE' | 'WAIT' | 'SKIP';
  multiplier: number | null;
  base_lots: number;
  lots: number | null;
  note: string;
}

/** `GET /api/market/indicator`. */
export interface IndicatorAnswer {
  configured: boolean;
  /** Indicator sizing is on. */
  enabled: boolean;
  now: string;
  indices: { index: MarketIndex; reading: IndicatorReading | null; bias: Bias; ce: CallVerdict; pe: CallVerdict }[];
}

export interface ChainStrike {
  strike: number;
  call_oi: number;
  call_prev_oi: number;
  put_oi: number;
  put_prev_oi: number;
}

/** `GET /api/market/option-chain?underlying=`. */
export interface OptionChainAnswer {
  underlying: MarketIndex;
  expiry: string | null;
  spot: number | null;
  atm_strike: number | null;
  updated_at: string | null;
  strikes: ChainStrike[];
}

export interface TrendRow {
  /** The check's `HH:MM`, or the forming one's latest snapshot. */
  time: string;
  live: boolean;
  ts: string;
  call_oi: number;
  put_oi: number;
  call_oi_chg: number;
  put_oi_chg: number;
  spot: number;
  fut_ltp: number | null;
  fut_vwap: number | null;
}

/** `GET /api/market/intraday-trend`. */
export interface TrendAnswer {
  underlying: MarketIndex;
  date: string | null;
  interval: number;
  window: number;
  /** Newest first. */
  rows: TrendRow[];
}

export interface MarketLiveIndex {
  underlying: MarketIndex;
  expiry: string | null;
  spot: number | null;
  future_symbol: string | null;
  fut_ltp: number | null;
  fut_vwap: number | null;
  updated_at: string | null;
  last_error: string | null;
}

/** `GET /api/market/live`. */
export interface MarketLive {
  configured: boolean;
  indices: MarketLiveIndex[];
}

// ── Connections ─────────────────────────────────────────────────────────── //

/** `GET /api/auth/kotak`. */
export interface KotakState {
  connected: boolean;
  session_present?: boolean;
  session_healthy?: boolean;
  has_env_credentials: boolean;
  has_totp_secret: boolean;
  auto_login_enabled: boolean;
  auto_login_ready: boolean;
  auto_login_reason?: string | null;
  masked_ucc?: string | null;
}

/** `GET /api/auth/telegram/status`. */
export interface TelegramState {
  state: string;
  chat_ids: number[];
}

/** `GET /api/v2/mode-check`: what switching between Paper and LIVE would do. */
export interface ModeCheck {
  mode: Mode;
  kotak_connected: boolean;
  /** Kotak's net funds, from the hourly cache. */
  funds: { net: number; fetched_at: string } | null;
  kill_switch: boolean;
  /** Positions holding quantity now, in the current mode. */
  open: number;
  waiting: number;
  lots: { index: string; lots: number }[];
}
