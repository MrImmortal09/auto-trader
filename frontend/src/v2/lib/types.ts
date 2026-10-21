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

/** `GET /api/v2/status`. */
export interface StatusAnswer {
  now: string;
  mode: Mode;
  kill_switch: boolean;
  stopped: Stopped | null;
  market_open: boolean;
  market: string;
  health: HealthSet | null;
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

/** `GET /api/v2/chain?index=`: the nearest expiry, as the Execute screen trades it. */
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
