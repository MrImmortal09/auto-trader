// The Trade screen's model: what a chart shows, a level's estimated rupees,
// the words for a level, and VWAP.

import type { ChartLevel, ExecutePosition } from '../../types';
import { level, signedMoney } from '../lib/format';

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

/** What a chart shows: an index, or one of its options. */
export type ChartSym =
  | { kind: 'index'; index: string }
  | { kind: 'option'; index: string; expiry: string; strike: number; side: 'CE' | 'PE' };

/** The candle service's name for it: `NIFTY`, `NIFTY:2026-10-27:25100:CE`. */
export function symbolName(s: ChartSym): string {
  return s.kind === 'index' ? s.index : `${s.index}:${s.expiry}:${s.strike}:${s.side}`;
}

/** The index's future, whose volume makes an index chart's VWAP. */
export function futureName(index: string): string {
  return `${index}:FUT`;
}

/** "NIFTY 50", "BANKNIFTY", "NIFTY 25100 CE". */
export function symbolLabel(s: ChartSym): string {
  if (s.kind === 'option') return `${s.index} ${s.strike} ${s.side}`;
  return s.index === 'NIFTY' ? 'NIFTY 50' : s.index;
}

export function sameSym(a: ChartSym, b: ChartSym): boolean {
  return symbolName(a) === symbolName(b);
}

/** A scalp's option, as a chart symbol, on `expiry` (its own). */
export function optionOf(p: Pick<ExecutePosition, 'index' | 'strike' | 'option_type'>, expiry: string): ChartSym | null {
  if (p.strike == null || (p.option_type !== 'CE' && p.option_type !== 'PE')) return null;
  return { kind: 'option', index: p.index, expiry, strike: p.strike, side: p.option_type };
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
 * price of every bar with Upstox's official volume (a forming bar has none
 * yet, so the line runs to the last closed bar).
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
    if (b.v == null || b.v <= 0) continue;
    pv += ((b.h + b.l + b.c) / 3) * b.v;
    vol += b.v;
    out.set(b.t, pv / vol);
  }
  return out;
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
