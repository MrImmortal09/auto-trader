// What the v2 endpoints answer — mirrors backend/server/src/desk.rs and
// history.rs. v1's own types stay in src/types.ts.

import type { ChartLevels } from '../../types';

export type { ChartLevel, ChartLevels, ChartTarget } from '../../types';

export type Mode = 'PAPER' | 'LIVE';
export type Tone = 'profit' | 'loss' | 'neutral';

/** How a closed call ended, as its chip reads. */
export interface Outcome {
  label: string;
  tone: Tone;
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

/** Who a page's caller is, as the server sees it. */
export type Role = 'anon' | 'user' | 'admin';

/** A phone sign-in's user, as `/api/auth/otp/verify` and `/api/auth/me` return it. */
export interface SessionUser {
  id: number;
  phone: string;
  name: string | null;
  role: 'admin' | 'user';
}

/** `GET /api/auth/me`. */
export interface MeAnswer {
  identity: 'admin' | 'user' | 'anonymous';
  user: SessionUser | null;
}

/** `GET /api/admin/users`: one row of the allow-list. */
export interface AdminUser {
  id: number;
  phone: string;
  name: string | null;
  role: 'admin' | 'user';
  enabled: boolean;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  created_at: string;
  last_login_at: string | null;
  /** A user's paper figures; null for the admin. */
  paper: AdminPaper | null;
}

/** One special trading session (e.g. Diwali Muhurat). Times are IST `HH:MM`. */
export interface SpecialSession {
  date: string;
  start: string;
  end: string;
}

/** `GET /api/admin/special-sessions` */
export interface SpecialSessionsAnswer {
  sessions: SpecialSession[];
  today: string;
  active: boolean;
}

/** A user's paper portfolio at a glance, on the Admin page. */
export interface AdminPaper {
  balance: number;
  /** Times they started over. */
  resets: number;
  open: number;
  /** The open positions' P&L at the live price. */
  open_pnl: number;
  /** Today's booked plus open P&L, since the last reset. */
  day_pnl: number;
  /** Fills since the last reset. */
  trades: number;
  /** IST `YYYY-MM-DD HH:MM:SS` of the newest fill, any reset. */
  last_trade_at: string | null;
}

/** One paper fill (`GET /api/admin/trades`). */
export interface AdminTrade {
  id: number;
  user_id: number;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  t: string;
  action: 'BUY' | 'SELL';
  contract: string;
  qty: number;
  price: number;
  charges: number;
  /** What the wallet paid (a buy) or got (a sell), charges included. */
  net: number;
  /** A sell's P&L, charges included on both sides. */
  pnl: number | null;
  reason: string | null;
  /** From before the user's last reset: gone from their own views. */
  before_reset: boolean;
}

export interface AdminTradesAnswer {
  /** Newest first. */
  trades: AdminTrade[];
  /** Fills in the range beyond those listed. */
  more: number;
  count: number;
  /** The P&L of every sell in the range. */
  realized: number;
}

/** Count, mean, median and 95th percentile of one latency, in ms. */
export interface LatencySummary {
  n: number;
  mean: number;
  p50: number;
  p95: number;
}

export interface LatencyFigures {
  /** Upstox's tick against the exchange's time of the trade. */
  upstox_delay: LatencySummary | null;
  /** Kotak's tick against the same trade's exchange time (from Upstox). */
  kotak_delay: LatencySummary | null;
  /** How much sooner Kotak had the price; negative when Upstox was first. */
  kotak_lead: LatencySummary | null;
}

export type LatencyClass = 'index' | 'future' | 'position';

/** How late each feed's prices arrive (`GET /api/admin/latency`). */
export interface LatencyAnswer {
  status: 'not_configured' | 'market_closed' | 'streaming';
  /** The IST day the figures are for. */
  day: string | null;
  upstox_error: string | null;
  /** Kotak's feed is connected and ticked in the last 10 s. */
  kotak_streaming: boolean;
  window_s: number;
  classes: { class: LatencyClass; label: string; minute: LatencyFigures; day: LatencyFigures }[];
  instruments: {
    label: string;
    class: LatencyClass;
    upstox_key: string;
    kotak_key: string;
    upstox_age_ms: number | null;
    kotak_age_ms: number | null;
    minute: LatencyFigures;
    day: LatencyFigures;
  }[];
  kotak_lagged: number;
}

/** One LIVE order's timing (`GET /api/admin/order-timings`). Times are Unix
 * ms on the server's clock unless named otherwise. */
export interface OrderTimingRow {
  order_no: string | null;
  trading_symbol: string | null;
  /** `B` / `S`. */
  side: string | null;
  /** `MKT`, `L`, `SL`, `SL-M`. */
  order_type: string | null;
  qty: number | null;
  price: number | null;
  trigger_price: number | null;
  sent_at_ms: number | null;
  /** When Kotak answered the send. */
  replied_at_ms: number | null;
  /** Why Kotak refused the send. */
  error: string | null;
  /** From the push stream: `open`, `complete`, `rejected`, `cancelled`, … */
  status: string | null;
  filled_qty: number | null;
  avg_price: number | null;
  reject_reason: string | null;
  first_update_ms: number | null;
  accepted_at_ms: number | null;
  first_fill_ms: number | null;
  filled_at_ms: number | null;
  /** Kotak's own receive time of the latest update, epoch ns. */
  kotak_received_ns: number | null;
  /** The exchange's broadcast time, epoch seconds. */
  exchange_epoch_s: number | null;
  /** The exchange's confirmation time, as Kotak writes it. */
  exchange_confirm: string | null;
  order_time: string | null;
  /** When the engine's own order-book poll first saw it finished. */
  seen_at_ms: number | null;
}

export interface OrderTimingAnswer {
  day: string;
  push: {
    /** `KOTAK_ORDER_STREAM=on` on the server. */
    enabled: boolean;
    connected: boolean;
    style: 'json' | 'raw' | null;
    last_event_ms: number | null;
    error: string | null;
    updates: number;
  };
  /** Newest first. */
  orders: OrderTimingRow[];
}

/** What the server's database keeps (`GET /api/admin/data`). */
export interface StoredData {
  /** The database file, its write-ahead log aside. */
  db_bytes: number;
  tables: StoredTable[];
  /** Indices first, each followed by its futures by expiry. */
  archive: ArchivedSymbol[];
  /** Upstox is configured, so the daily archive sweep runs. */
  archiving: boolean;
}

export interface StoredTable {
  table: string;
  label: string;
  what: string;
  /** "Forever", "30 days", "Today". */
  kept: string;
  rows: number;
  /** IST `YYYY-MM-DD HH:MM:SS` of the oldest and newest row. */
  first: string | null;
  last: string | null;
}

/** One index or future in the candle archive. */
export interface ArchivedSymbol {
  /** `NIFTY`, or `NIFTY:FUT:2026-11-24`. */
  symbol: string;
  index: string;
  /** A future's expiry; null for the index. */
  expiry: string | null;
  first_day: string;
  last_day: string;
  days: number;
  candles: number;
  /** Candles that traded; an index never does. */
  with_volume: number;
}

/** One open paper position (`GET /api/admin/positions`). */
export interface AdminPosition {
  user_id: number;
  id: string;
  contract: string;
  /** `YYYY-MM-DD`. */
  expiry: string;
  qty: number;
  lots: number;
  avg: number;
  ltp: number | null;
  pnl: number | null;
  /** IST `YYYY-MM-DD HH:MM:SS` of the first buy. */
  opened_at: string;
}

/** One trade on a day of a user's report: a paper position, from its first
 * buy to its last sell. */
export interface ReportTrade {
  /** The paper position's id. */
  id: string;
  contract: string;
  /** IST `YYYY-MM-DD HH:MM:SS` of the first buy. */
  opened_at: string;
  /** The last sell, once everything bought was sold; null while held. */
  closed_at: string | null;
  held_secs: number | null;
  bought_qty: number;
  sold_qty: number;
  /** Average fill prices, before charges. */
  avg_buy: number | null;
  avg_sell: number | null;
  /** The whole trade's P&L after charges, once closed. */
  pnl: number | null;
  /** What its sells on this day booked: gross − charges = net. */
  day_gross: number;
  day_charges: number;
  day_net: number;
  outcome: Outcome | null;
  before_reset: boolean;
  /** Every fill of it, oldest first — other days' too. */
  legs: LegRow[];
}

/** One IST day of a user's report. */
export interface ReportDay {
  date: string;
  /** Booked by the day's sells. */
  gross: number;
  charges: number;
  net: number;
  turnover: number;
  buys: number;
  sells: number;
  /** Trades fully sold that day. */
  closed: number;
  wins: number;
  losses: number;
  /** IST timestamps of the day's first and last fill. */
  first_at: string;
  last_at: string;
  expiries: string[];
  /** Every trade with a fill that day, in the order they opened. */
  trades: ReportTrade[];
  /** The day's trades left out of `trades`, past the report's limit. */
  unlisted: number;
}

/** One contract's fills over the range. */
export interface ReportContract {
  contract: string;
  trades: number;
  buy_qty: number;
  buy_value: number;
  sell_qty: number;
  sell_value: number;
  avg_buy: number | null;
  avg_sell: number | null;
  gross: number;
  charges: number;
  net: number;
}

/** The trades closed in the range, by the IST hour they opened in. */
export interface ReportHour {
  hour: number;
  closed: number;
  wins: number;
  net: number;
}

/** One fill in a user's tradebook. */
export interface ReportFill {
  id: number;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  t: string;
  action: 'BUY' | 'SELL';
  contract: string;
  qty: number;
  price: number;
  /** Quantity × price. */
  value: number;
  /** The fill's own charges. */
  charges: number;
  net: number;
  /** A sell's P&L, charges included on both sides. */
  pnl: number | null;
  reason: string | null;
  position_id: string;
  before_reset: boolean;
}

export interface ReportSummary {
  /** Booked by every sell in the range: gross − charges = net. */
  gross: number;
  charges: number;
  net: number;
  /** Charges on every fill in the range, bought or sold. */
  charges_paid: number;
  turnover: number;
  buys: number;
  sells: number;
  /** Trades fully sold in the range. */
  closed: number;
  wins: number;
  losses: number;
  win_rate: number | null;
  profit_factor: number | null;
  avg_per_trade: number | null;
  avg_win: number | null;
  avg_loss: number | null;
  best: number | null;
  worst: number | null;
  avg_held_secs: number | null;
  max_drawdown: number;
  days_traded: number;
  green_days: number;
  red_days: number;
}

/** `GET /api/admin/users/:id/report`: one user's paper P&L over a range. */
export interface UserReport {
  user: AdminUser;
  /** IST day of their first fill, before any reset. */
  first_day: string | null;
  /** What they hold now, at the live price. */
  open: AdminPosition[];
  /** The range's first day; null from the first fill on. */
  from: string | null;
  to: string;
  summary: ReportSummary;
  /** The booked P&L after each sell, oldest first. */
  curve: CurvePoint[];
  /** Newest first. */
  days: ReportDay[];
  /** Best first. */
  contracts: ReportContract[];
  hours: ReportHour[];
  /** Newest first. */
  fills: ReportFill[];
  more_fills: number;
}

/** One line of a user's paper activity (`GET /api/v2/me/activity`): a fill,
 * a chart level set, or a reset — kept in the server's memory, newest first. */
export interface PaperActivity {
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  t: string;
  kind: 'buy' | 'sell' | 'levels' | 'reset';
  position_id: string | null;
  message: string;
}

/** `GET /api/v2/flags`. */
export interface Flags {
  index_options_only: boolean;
  chart_stops: boolean;
  /** The instrument search and its quotes (`/api/v2/search`, `/api/v2/quotes`,
   * `/api/v2/instruments/*`) for anyone signed in; while off they answer 404
   * and the search well and tab are hidden. */
  search: boolean;
  /** Watchlists (`/api/v2/watchlists`) for anyone signed in; while off they
   * answer 404 and the Watchlist screen, its tab and the "+" on a search
   * result or an instrument page are hidden. The app counts them as on only
   * while `search` is on too (no prices, no way to add one without it). */
  watchlists: boolean;
  /** Any F&O stock's chart, ladder and option chain (plan Phase 3), on top
   * of `search`; while off the charts and ladders are the six indices' and
   * every stock control is hidden. Missing from a server older than it. */
  stock_charts?: boolean;
  /** Stock options on users' paper books (plan Phase 4), on top of `search`
   * and `stock_charts`: `POST /api/v2/me/buy` takes an NSE stock's CE/PE, one
   * tap as an index option's. While off, the ladder's Buy on a stock's row is
   * disabled and says so. The owner's live buy stays index-only whatever it
   * says. Missing from a server older than it. */
  stock_paper?: boolean;
}

export interface ChainSide {
  /** The broker's trading symbol — what a Buy sends. */
  symbol: string;
  /** Signed (a put's is negative); `assumed` when Upstox had none. On a
   * stock's chain, Upstox's or `null` — never the assumed ±0.50. */
  delta: { value: number; assumed: boolean } | null;
}

/** `GET /api/positions`: the engine's own rows, the few fields the Trade panel's Orders tab reads. */
export interface RawPosition {
  id: string;
  manual?: boolean;
  state: string;
  executed_qty: number;
  signal: { instrument_name: string; strike: number | null; option_type: string | null };
  /** LIVE: the entry buy at Kotak, while it fills. */
  entry_order_id?: string | null;
  /** LIVE: an exit sell at Kotak, while it fills. */
  pending_exit_order_id?: string | null;
  /** LIVE: a resting SL-M order at Kotak (one adopted at startup; the engine places none). */
  sl_order_id?: string | null;
  sl_order_qty?: number;
  sl_order_trigger?: number;
}

/** `GET /api/v2/expiries?index=`: every expiry the underlying's options are
 * listed for, earliest first. The underlying is an index, or with the
 * `stock_charts` switch on any F&O stock the catalog lists. */
export interface ExpiriesAnswer {
  /** The underlying, under its older name. */
  index: string;
  /** The same as `index`; missing from a server older than stock charts. */
  underlying?: string;
  kind?: 'index' | 'stock';
  /** `YYYY-MM-DD`, IST. */
  today: string;
  expiries: { date: string; days_to_expiry: number; lot_size: number }[];
}

/** `GET /api/v2/chain?index=&expiry=`: one expiry's strikes (the nearest by default), as the Execute screen trades them. */
export interface ChainAnswer {
  /** The underlying, under its older name. */
  index: string;
  /** The same as `index`; missing from a server older than stock charts. */
  underlying?: string;
  kind?: 'index' | 'stock';
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

// ── Instrument search — mirrors backend/server/src/routes/v2/instruments.rs,
// instruments/{load,search}.rs and quotes/hub.rs ────────────────────────── //

/** `INDEX`, a cash equity, a future, a call or a put. */
export type InstrumentKind = 'INDEX' | 'EQ' | 'FUT' | 'CE' | 'PE';

/** How a query matched an underlying, best first; `null` on a derivative
 * listed under a match. */
export type SearchTier = 'exact' | 'symbol_prefix' | 'word_prefix' | 'substring';

/** Where a quote's price came from: the feed or Kotak's REST quotes
 * (`live`, never told apart), Upstox while Kotak is down, or the 15:35
 * close served after hours. */
export type QuoteSource = 'live' | 'upstox' | 'close';

/** One instrument's price, from the quote hub. */
export interface Quote {
  /** The price-map key: `nse_cm|2885`, `nse_fo|51386`, `idx|NIFTY 50`. */
  key: string;
  ltp: number;
  open: number | null;
  high: number | null;
  low: number | null;
  /** The previous close. */
  close: number | null;
  /** Against the previous close, for this quote's own `ltp`. */
  change: number | null;
  per_change: number | null;
  volume: number | null;
  oi: number | null;
  year_high: number | null;
  year_low: number | null;
  /** Unix ms the price was read. */
  at: number;
  source: QuoteSource;
  /** For a close: the IST day and time it stands for, `YYYY-MM-DD 15:30`. */
  as_of: string | null;
}

/** A catalog row: an index, a stock, a future or an option. */
export interface InstrumentRow {
  /** `<segment>:<token>`: `nse_cm:2885`, `nse_fo:51386`, `nse_cm:Nifty 50`. */
  id: string;
  kind: InstrumentKind;
  /** `RELIANCE`, `NIFTY`. */
  symbol: string;
  /** `Reliance Industries Ltd`, `Nifty 50`. */
  name: string;
  /** `NSE` or `BSE`. */
  exchange: string;
  /** `RELIANCE26NOV1400CE`. */
  trading_symbol: string;
  underlying: string;
  /** `YYYY-MM-DD`, derivatives only. */
  expiry: string | null;
  strike: number | null;
  lot_size: number;
  tick_size: number;
  /** The price-map key, for `/api/v2/quotes`. */
  ws_key: string;
  /** The underlying has F&O listed. */
  has_fo: boolean;
  tier: SearchTier | null;
}

/** A search result: the row with its quote, when any source has one
 * (`null` until the poller has fetched it — ask again in 2 s). */
export interface SearchRow extends InstrumentRow {
  quote: Quote | null;
}

/** `GET /api/v2/search?q=&limit=`: the catalog's groups, ranked, at most
 * `limit` (1–8) rows each. */
export interface SearchAnswer {
  query: string;
  /** How the query was read: a symbol or name, `<SYM> FUT`, or
   * `<SYM> [<MON>] <STRIKE> CE|PE`. */
  parsed: 'plain' | 'future' | 'contract';
  /** IST `YYYY-MM-DD` the derivatives are listed from. */
  today: string;
  /** The IST day of the CSVs the catalog was built from, for a "catalog from
   * <date>" note when it is not today; `null` before the first build. */
  catalog_day: string | null;
  market_open: boolean;
  indices: SearchRow[];
  stocks: SearchRow[];
  futures: SearchRow[];
  options: SearchRow[];
}

/** `GET /api/v2/quotes?keys=` (at most 60 keys). */
export interface QuoteAnswer {
  /** Unix ms the answer was made. */
  at: number;
  market_open: boolean;
  /** One per key with a quote, in the order asked. */
  quotes: Quote[];
  /** Known keys no source has a quote for yet; polled from now. */
  missing: string[];
  /** Keys the catalog has no row for. */
  unknown: string[];
}

/** `GET /api/v2/instruments/:id`: the instrument page. */
export interface Instrument extends InstrumentRow {
  segment: string;
  /** `pSymbol`: the token the feed and the quotes API take. */
  token: string;
  isin: string | null;
  /** The price-map key of the underlying's cash or index row: a derivative's
   * spot, a cash row's own key; `null` for an underlying with no cash or
   * index row. */
  underlying_key: string | null;
  /** The nearest expiry listed under the underlying from today, on an F&O
   * underlying's cash or index row; `null` on a contract or a stock with no
   * F&O. */
  nearest_expiry: string | null;
  /** `null` until the poller has fetched it. */
  quote: Quote | null;
  /** A derivative's underlying, priced the same way. */
  spot: Quote | null;
  catalog_day: string | null;
  market_open: boolean;
}

/** The catalog, for `GET /api/v2/instruments/status`. */
export interface CatalogStatus {
  /** IST date of the CSVs the catalog was built from. */
  day: string | null;
  /** IST `YYYY-MM-DD HH:MM:SS` of the last successful build. */
  loaded_at: string | null;
  /** Rows per segment (`nse_cm`, `nse_fo`, `bse_fo`) and the six `index` rows. */
  rows: Record<string, number>;
  /** Rows read per kind (`INDEX`, `EQ`, `FUT`, `CE`, `PE`). */
  kinds: Record<string, number>;
  /** Symbols in the search list. */
  underlyings: number;
  fts: boolean;
  loading: boolean;
  last_error: string | null;
}

/** The quote hub, for `GET /api/v2/instruments/status`. */
export interface QuoteHubStatus {
  /** Keys asked for in the last 15 s. */
  wanted: number;
  quotes: number;
  upstox_quotes: number;
  batch_size: number;
  quarantined: number;
  calls: number;
  refused: number;
  rate_limited: number;
  errors: number;
  unmatched: number;
  upstox_calls: number;
  upstox_errors: number;
  calls_last_minute: number;
  calls_per_sec: number;
  upstox_calls_last_minute: number;
  /** IST `HH:MM:SS` of the last call that answered. */
  last_call_at: string | null;
  last_error: string | null;
  /** Why nothing is being polled, when nothing is. */
  paused: string | null;
  sweep: { day: string; total: number; done: number } | null;
  close: { day: string; taken_at: string; rows: number; asked: number; partial: boolean } | null;
}

/** `GET /api/v2/instruments/status` — the admin's. */
export interface InstrumentsStatus {
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  now: string;
  market_open: boolean;
  catalog: CatalogStatus;
  quotes: QuoteHubStatus;
  /** The day of the close snapshot served after hours, when one is on file. */
  snapshot_day: string | null;
}

// ── Watchlists — mirrors backend/server/src/routes/v2/watchlists.rs ──────── //

/** One row of a watchlist: the key as kept, the catalog's row for it and
 * its quote. */
export interface WatchlistItem {
  /** The price-map key (`nse_cm|2885`, `nse_fo|51386`, `idx|NIFTY 50`). */
  key: string;
  /** 0-based within the list. */
  position: number;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  added_at: string;
  /** The catalog's row, as `/api/v2/search` lists it; `null` when the
   * catalog no longer has one (an expired contract, or no catalog built
   * yet). */
  instrument: InstrumentRow | null;
  /** From the feed or Kotak's REST quotes during hours (`live`), Upstox
   * while Kotak is down, the 15:35 close after hours (`close`, with
   * `as_of`); `null` until the poller has fetched it — ask again in 2 s. */
  quote: Quote | null;
  /** How old the quote's price is, by the server's clock; `null` without a quote. */
  age_secs: number | null;
}

export interface Watchlist {
  id: number;
  name: string;
  /** 0-based among the person's lists. */
  position: number;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  created_at: string;
  /** In order. */
  items: WatchlistItem[];
}

/** `GET /api/v2/watchlists`: every list of the signed-in person's, the
 * admin's included — they are keyed by user id, not role. A first call
 * makes the default list (`Watchlist 1`: NIFTY, BANKNIFTY, SENSEX). */
export interface WatchlistsAnswer {
  /** Unix ms the answer was made, to age the rows against. */
  at: number;
  market_open: boolean;
  /** 5. */
  max_lists: number;
  /** 50. */
  max_items: number;
  /** 40. */
  max_name_chars: number;
  /** In position order. */
  watchlists: Watchlist[];
}

/** Every other watchlist route's answer: the one list the call was about. */
export interface WatchlistAnswer {
  at: number;
  market_open: boolean;
  watchlist: Watchlist;
}
