// The Trade screen's model: what a chart shows, a level's estimated rupees,
// the words for a level, and VWAP.

import type { ChartLevel, ExecuteBoard, ExecutePosition, ExecuteQuote } from '../../types';
import { INDEX_ORDER } from '../../lib/execute';
import { level, shortDate, signedMoney } from '../lib/format';

export type Tf = '1m' | '5m' | '15m';
/** Which library draws the charts: KLineChart (the default) or Vela. */
export type ChartLib = 'kline' | 'vela';
/** How the price series is drawn, the same on either library. */
export type CandleStyle = 'candles' | 'bars' | 'area';
export const CANDLE_STYLES: { value: CandleStyle; label: string }[] = [
  { value: 'candles', label: 'Candles' },
  { value: 'bars', label: 'Bars' },
  { value: 'area', label: 'Area' },
];
export const CHART_LIBS: { value: ChartLib; label: string }[] = [
  { value: 'kline', label: 'KLine' },
  { value: 'vela', label: 'Vela' },
];
export const TF_MS: Record<Tf, number> = { '1m': 60_000, '5m': 300_000, '15m': 900_000 };
/** Days of bars a chart opens with, under each timeframe's server cap. */
export const TF_DAYS: Record<Tf, number> = { '1m': 2, '5m': 5, '15m': 10 };

/** A stock's listing. The catalog is NSE-only, so `BSE` is read but the
 * server refuses to chart it (`RELIANCE@BSE`). */
export type Exchange = 'NSE' | 'BSE';

/**
 * What a chart shows: an index or a stock, one of its futures, or one of
 * its options. `index` names the underlying on a future or an option — an
 * index or, behind the `stock_charts` switch, any F&O stock — under the
 * name it has had since charts were index-only, so saved layouts load. A
 * future carries its `expiry` (`YYYY-MM-DD`) once picked from a catalog row:
 * the candle service charts that contract and no other (#74's
 * `RELIANCE:FUT:2026-12-29`). Without one it is the nearest future still
 * trading (`RELIANCE:FUT`), as every future saved before futures had a date.
 */
export type ChartSym =
  | { kind: 'index'; index: string }
  | { kind: 'equity'; symbol: string; exchange: Exchange }
  | { kind: 'future'; index: string; expiry?: string }
  | { kind: 'option'; index: string; expiry: string; strike: number; side: 'CE' | 'PE' };

const DAY = /^\d{4}-\d{2}-\d{2}$/;

/** One of the six indices the app has a feed name for. */
export function isIndex(name: string): boolean {
  return INDEX_ORDER.includes(name);
}

/** An underlying's name as the scrip master spells it: letters, digits and
 * the `&`, `-` and `_` some stocks carry (`M&M`, `BAJAJ-AUTO`). */
export function isTicker(name: unknown): name is string {
  return typeof name === 'string' && /^[A-Z0-9&_-]+$/.test(name);
}

/** The index or stock `s` is, or is a contract on. */
export function underlyingOf(s: ChartSym): string {
  return s.kind === 'equity' ? s.symbol : s.index;
}

/** An underlying's own chart: the index, or the stock's cash price. */
export function ownChartOf(name: string): ChartSym {
  return isIndex(name) ? { kind: 'index', index: name } : { kind: 'equity', symbol: name, exchange: 'NSE' };
}

/** Whether `s` is `underlying`'s own chart (the index, or the stock's cash
 * price) rather than one of its contracts: where an index-trigger level lives. */
export function isOwnChart(s: ChartSym, underlying: string): boolean {
  return (s.kind === 'index' || s.kind === 'equity') && underlyingOf(s) === underlying;
}

/** The candle service's name for it: `NIFTY`, `RELIANCE` (`RELIANCE@BSE`
 * for a BSE listing), `RELIANCE:FUT:2026-12-29` (a future by its expiry;
 * `NIFTY:FUT`, the nearest, without one), `NIFTY:2026-10-27:25100:CE`. */
export function symbolName(s: ChartSym): string {
  switch (s.kind) {
    case 'index':
      return s.index;
    case 'equity':
      return s.exchange === 'NSE' ? s.symbol : `${s.symbol}@${s.exchange}`;
    case 'future':
      return s.expiry ? `${futureName(s.index)}:${s.expiry}` : futureName(s.index);
    case 'option':
      return `${s.index}:${s.expiry}:${s.strike}:${s.side}`;
  }
}

/** An underlying's nearest future, whose volume makes an index chart's VWAP. */
export function futureName(underlying: string): string {
  return `${underlying}:FUT`;
}

/** The future whose volume an index chart's VWAP is built from; a stock,
 * a future and an option trade themselves. */
export function futureFor(s: ChartSym): string | null {
  return s.kind === 'index' ? futureName(s.index) : null;
}

/** Whether the instrument trades, so its bars carry volume: an index's don't. */
export function hasVolume(s: ChartSym): boolean {
  return s.kind !== 'index';
}

/** A contract's expiry month as the market names its futures: "DEC". */
export function expiryMonth(expiry: string): string {
  return shortDate(expiry).split(' ')[1].toUpperCase();
}

/** "NIFTY 50", "BANKNIFTY", "RELIANCE", "RELIANCE · BSE", "RELIANCE DEC FUT"
 * ("RELIANCE FUT": the nearest, a future saved without its expiry),
 * "NIFTY 25100 CE". */
export function symbolLabel(s: ChartSym): string {
  switch (s.kind) {
    case 'option':
      return `${s.index} ${s.strike} ${s.side}`;
    case 'future':
      return s.expiry ? `${s.index} ${expiryMonth(s.expiry)} FUT` : `${s.index} FUT`;
    case 'equity':
      return s.exchange === 'NSE' ? s.symbol : `${s.symbol} · ${s.exchange}`;
    case 'index':
      return s.index === 'NIFTY' ? 'NIFTY 50' : s.index;
  }
}

/**
 * A saved chart symbol read back (`v2.trade`): the shape as kept, or `null`
 * for anything else — a layout saved by a newer or older build, or edited
 * by hand — so the caller falls back to an index. A future or an option on
 * a stock, and a stock itself, are kept; whether the server will chart them
 * is the `stock_charts` switch's call when the screen opens.
 */
export function parseChartSym(raw: unknown): ChartSym | null {
  if (!raw || typeof raw !== 'object') return null;
  const s = raw as Record<string, unknown>;
  const index = isTicker(s.index) ? s.index : null;
  if (s.kind === 'index') return index && isIndex(index) ? { kind: 'index', index } : null;
  // A future saved before futures had a date loads as the nearest one.
  if (s.kind === 'future') {
    if (!index) return null;
    if (s.expiry == null) return { kind: 'future', index };
    return typeof s.expiry === 'string' && DAY.test(s.expiry) ? { kind: 'future', index, expiry: s.expiry } : null;
  }
  if (s.kind === 'equity') {
    if (!isTicker(s.symbol) || isIndex(s.symbol)) return null;
    return { kind: 'equity', symbol: s.symbol, exchange: s.exchange === 'BSE' ? 'BSE' : 'NSE' };
  }
  if (s.kind === 'option') {
    const ok = index && typeof s.expiry === 'string' && DAY.test(s.expiry) && typeof s.strike === 'number' && Number.isFinite(s.strike) && s.strike > 0 && (s.side === 'CE' || s.side === 'PE');
    return ok ? { kind: 'option', index, expiry: s.expiry as string, strike: s.strike as number, side: s.side as 'CE' | 'PE' } : null;
  }
  return null;
}

/** What a ladder's underlying is: the server says since stocks joined;
 * an older one only ever served the six indices. */
export function boardKind(board: Pick<ExecuteBoard, 'index'> & { kind?: string }): 'index' | 'stock' {
  if (board.kind === 'stock' || board.kind === 'index') return board.kind;
  return isIndex(board.index) ? 'index' : 'stock';
}

/** The owner's one-tap buy on a stock's ladder: never widened without an
 * explicit re-confirmation (the plan's question 8), whatever the switches
 * say — `POST /api/execute/buy` resolves index options only. */
export const OWNER_STOCK_BUY = 'Live stock options stay index-only until you re-confirm';
/** A user's paper buy on a stock's ladder while the server's `stock_paper`
 * switch is off, or not known yet. */
export const STOCK_PAPER_OFF = 'Stock options on paper are off';

/**
 * Why no row of a ladder can be bought from this desk, or null. An index's
 * rows never are. A stock's rows: on the owner's desk always
 * ({@link OWNER_STOCK_BUY}); on a user's paper book only while the server's
 * `stock_paper` switch is on — not yet known counts as off, so nothing is
 * bought on a guess. Everything else (market hours, the feed, a row's own
 * tick, the caps, the wallet) is the server's to refuse, and its text shows.
 */
export function stockBuyBlocked(kind: 'index' | 'stock', paper: boolean, stockPaper: boolean | undefined): string | null {
  if (kind === 'index') return null;
  if (!paper) return OWNER_STOCK_BUY;
  return stockPaper === true ? null : STOCK_PAPER_OFF;
}

/** The price is the quote hub's (Kotak's REST quote every ~2 s), not the
 * feed's: the strike is past the feed budget. No buy route accepts it. */
export function quotePolled(q: ExecuteQuote & { polled?: boolean }): boolean {
  return q.polled === true;
}

/** "<1 s", "12 s", "4 min", "3 h", "2 d": how old a polled price is, in the
 * largest unit that fits; '' without an age. */
export function fmtAge(ms: number | null): string {
  if (ms == null) return '';
  if (ms < 1000) return '<1 s';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h} h` : `${Math.floor(h / 24)} d`;
}

/**
 * The marker under a polled price: "polled", with its age once that is
 * worth seeing (10 s and more) while the market is open. Closed (after
 * hours, before the open, a weekend), the hub's price is the last session's
 * close snapshot and its age only counts the hours since: it says "close".
 */
export function polledMarker(ageMs: number | null, marketOpen: boolean): string {
  if (!marketOpen) return 'polled · close';
  return ageMs != null && ageMs >= 10_000 ? `polled · ${fmtAge(ageMs)}` : 'polled';
}

/** The charted instrument's day so far, from its bars: the latest IST day's
 * open, high, low, last and volume, and the close of the day before it
 * (null when the bars start on that day). */
export interface DaySummary {
  open: number;
  high: number;
  low: number;
  close: number;
  /** The day's traded volume; null when no bar carries any (an index). */
  volume: number | null;
  prevClose: number | null;
}

export function daySummary(bars: Bar[] | null): DaySummary | null {
  if (!bars || bars.length === 0) return null;
  const last = bars[bars.length - 1];
  const day = dayOf(last.t);
  let open = last.o;
  let high = last.h;
  let low = last.l;
  let volume: number | null = null;
  let prevClose: number | null = null;
  for (let i = bars.length - 1; i >= 0; i--) {
    const b = bars[i];
    if (dayOf(b.t) !== day) {
      prevClose = b.c;
      break;
    }
    open = b.o;
    if (b.h > high) high = b.h;
    if (b.l < low) low = b.l;
    if (b.v != null) volume = (volume ?? 0) + b.v;
  }
  return { open, high, low, close: last.c, volume, prevClose };
}

/** The same instrument: its candle name, so two futures of one stock (or
 * a dated future and the bare nearest one) are told apart. */
export function sameSym(a: ChartSym, b: ChartSym): boolean {
  return symbolName(a) === symbolName(b);
}

/** The expiry a chart's contract trades to: an option's, a dated future's;
 * null for anything else (the bare future included: the server picks it). */
export function symExpiry(s: ChartSym): string | null {
  return s.kind === 'option' ? s.expiry : s.kind === 'future' ? (s.expiry ?? null) : null;
}

/** A scalp's option, as a chart symbol, on `expiry` (its own). */
export function optionOf(p: Pick<ExecutePosition, 'index' | 'strike' | 'option_type'>, expiry: string): ChartSym | null {
  if (p.strike == null || (p.option_type !== 'CE' && p.option_type !== 'PE')) return null;
  return { kind: 'option', index: p.index, expiry, strike: p.strike, side: p.option_type };
}

/**
 * Which level of `p` a chart showing `sym` takes: a premium level on the
 * position's own option (on `expiry`: its own, or the nearest for a row from
 * before that was recorded), an index level on its underlying's own chart
 * (the index, or the stock's cash price), and none on any other chart. A
 * level read off any other chart's price scale — NIFTY's ~25,000 for a
 * RELIANCE option — would sit far past the price and sell the whole
 * position at once.
 */
export function levelTriggerOn(sym: ChartSym, p: Pick<ExecutePosition, 'index' | 'strike' | 'option_type'>, expiry: string | null | undefined): ChartLevel['trigger'] | null {
  const option = expiry ? optionOf(p, expiry) : null;
  if (option && sameSym(option, sym)) return 'premium';
  return isOwnChart(sym, p.index) ? 'index' : null;
}

/**
 * What exiting at `lvl` would make, roughly. On the option's price it is
 * `(level − avg) × qty`; on the index, the option is taken to move by its
 * delta for each point the index moves from now.
 */
export function levelPnl(
  lvl: ChartLevel,
  p: Pick<ExecutePosition, 'avg' | 'qty' | 'ltp'>,
  indexNow: number | null,
  delta: number,
): number | null {
  if (lvl.trigger === 'premium') return (lvl.price - p.avg) * p.qty;
  if (p.ltp == null || indexNow == null) return null;
  const premium = p.ltp + delta * (lvl.price - indexNow);
  return (premium - p.avg) * p.qty;
}

/**
 * Whether a stop or target at `lvl` would fire at once — the engine's rule
 * (`chart_level_hit`): on the premium a stop is reached falling and a target
 * rising; on the index a call loses as it falls, a put as it rises. `null`
 * without a live price to tell. Set there, it sells the whole holding on the
 * engine's next tick — the chart says so while a line is past the price.
 */
export function levelCrossed(
  kind: 'stop' | 'target',
  lvl: ChartLevel,
  side: string | null,
  premiumNow: number | null,
  indexNow: number | null,
): boolean | null {
  const now = lvl.trigger === 'premium' ? premiumNow : indexNow;
  if (now == null || !(now > 0)) return null;
  return levelSide(kind, lvl.trigger, side) === 'below' ? now <= lvl.price : now >= lvl.price;
}

/** Where a stop or target belongs against the price it watches: a stop
 * below the premium, a call's index stop below the index, a put's above —
 * and each target the other way. */
export function levelSide(kind: 'stop' | 'target', trigger: ChartLevel['trigger'], side: string | null): 'below' | 'above' {
  const isStop = kind === 'stop';
  const falling = trigger === 'premium' ? isStop : isStop === (side === 'CE');
  return falling ? 'below' : 'above';
}

/** "≈ −₹2,665" for a level's estimated P&L. */
export function approxMoney(pnl: number | null): string {
  return pnl == null ? '≈ ?' : `≈ ${signedMoney(pnl, 0)}`;
}

/** "NIFTY ≤ 25,060" or "₹92.50": the way a level fires. A call's stop and a
 * put's target fire as the index falls, the other two as it rises. */
export function levelText(index: string, side: string | null, lvl: ChartLevel, kind: 'stop' | 'target'): string {
  if (lvl.trigger === 'premium') return `₹${level(lvl.price)}`;
  const falls = (kind === 'stop') === (side === 'CE');
  return `${index} ${falls ? '≤' : '≥'} ${level(lvl.price)}`;
}

/** Round to the exchange's tick: levels are prices the engine compares.
 * Paise at most, so 25,132.35 never goes out as 25132.350000000002. */
export function toTick(price: number, tick = 0.05): number {
  return Number((Math.round(price / tick) * tick).toFixed(2));
}

export interface Bar {
  t: number;
  o: number;
  h: number;
  l: number;
  c: number;
  v: number | null;
}

const IST_MS = 330 * 60_000;
const dayOf = (t: number) => Math.floor((t + IST_MS) / 86_400_000);

/** The IST calendar day of a Unix-ms instant, as a day number. */
export const istDay = dayOf;

/**
 * VWAP by bar start, restarting each IST day: the volume-weighted typical
 * price of the day's bars with Upstox's official volume. A bar without any
 * yet — the forming bar, until its official minute lands — carries the day's
 * VWAP so far, so the line and its readout reach the latest bar. Before the
 * day's first volume there is none.
 */
export function vwapOf(bars: Bar[]): Map<number, number> {
  const out = new Map<number, number>();
  let day = -1;
  let pv = 0;
  let vol = 0;
  for (const b of [...bars].sort((a, z) => a.t - z.t)) {
    if (dayOf(b.t) !== day) {
      day = dayOf(b.t);
      pv = 0;
      vol = 0;
    }
    if (b.v != null && b.v > 0) {
      pv += ((b.h + b.l + b.c) / 3) * b.v;
      vol += b.v;
    }
    if (vol > 0) out.set(b.t, pv / vol);
  }
  return out;
}

/** A VWAP series cut into its IST days, oldest first: each day's line is
 * its own, never joined to the day before. */
export function vwapDays(series: Map<number, number>): { day: number; points: { time: number; price: number }[] }[] {
  const days = new Map<number, { time: number; price: number }[]>();
  for (const [time, price] of [...series].sort((a, z) => a[0] - z[0])) {
    const day = dayOf(time);
    if (!days.has(day)) days.set(day, []);
    days.get(day)!.push({ time, price });
  }
  return [...days].map(([day, points]) => ({ day, points }));
}

/**
 * The future's VWAP moved onto the index: each bar's VWAP less that bar's
 * basis (future − index), so the index sits above or below the line exactly
 * as the future sits against its own VWAP.
 */
export function vwapOnIndex(future: Bar[], index: Bar[]): Map<number, number> {
  const vwap = vwapOf(future);
  const closes = new Map(future.map((b) => [b.t, b.c]));
  const out = new Map<number, number>();
  for (const b of index) {
    const v = vwap.get(b.t);
    const f = closes.get(b.t);
    if (v != null && f != null) out.set(b.t, v - (f - b.c));
  }
  return out;
}

/** The bar containing `t`, from bars sorted by start. */
export function barAt(bars: Bar[], t: number, tfMs: number): Bar | null {
  for (let i = bars.length - 1; i >= 0; i--) {
    if (bars[i].t <= t) return t < bars[i].t + tfMs ? bars[i] : null;
  }
  return null;
}

/** Unix ms of an IST `YYYY-MM-DD HH:MM:SS`. */
export function istMs(ts: string): number {
  return Date.parse(`${ts.replace(' ', 'T')}+05:30`);
}

/** "03:52" until `end`, or '' once it has passed. */
export function countdown(end: number, now: number): string {
  const s = Math.floor((end - now) / 1000);
  if (s < 0) return '';
  return `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`;
}

/** Whether `t` is inside the 09:15–15:30 IST session. */
export function inSession(t: number): boolean {
  const m = Math.floor(((t + IST_MS) % 86_400_000) / 60_000);
  return m >= 9 * 60 + 15 && m < 15 * 60 + 30;
}

const IST_TIME = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });

/** IST `HH:MM` of a Unix-ms instant. */
export function istClock(t: number): string {
  return IST_TIME.format(new Date(t));
}
