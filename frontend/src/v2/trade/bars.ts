// Loading a chart's bars from the candle service (PR 3): a few days of
// history, then the stream's updates merged in.

import type { KLineData } from 'klinecharts';
import { getCandles } from '../lib/api';
import { TF_DAYS, type Bar, type Tf } from './model';

export const toK = (b: Bar): KLineData => ({ timestamp: b.t, open: b.o, high: b.h, low: b.l, close: b.c, volume: b.v ?? undefined });

function isoDay(t: number): string {
  return new Intl.DateTimeFormat('en-CA', { timeZone: 'Asia/Kolkata' }).format(new Date(t));
}

/** `days` calendar days of `symbol`'s bars at `tf`, today included. */
export async function fetchDays(base: string, symbol: string, tf: Tf, days: number): Promise<Bar[]> {
  const to = Date.now();
  const from = to - (days - 1) * 86_400_000;
  // As whoever is signed in.
  return (await getCandles(base, symbol, tf, isoDay(from), isoDay(to))).bars;
}

/** The days of bars a chart opens with at `tf`. */
export const fetchBars = (base: string, symbol: string, tf: Tf) => fetchDays(base, symbol, tf, TF_DAYS[tf]);

/** Merge a streamed bar into a sorted list: replace its start, or append. */
export function upsert(bars: Bar[], bar: Bar): Bar[] {
  const last = bars[bars.length - 1];
  if (!last || bar.t > last.t) return [...bars, bar];
  const i = bars.findIndex((b) => b.t === bar.t);
  if (i < 0) return bars;
  const next = bars.slice();
  next[i] = bar;
  return next;
}
