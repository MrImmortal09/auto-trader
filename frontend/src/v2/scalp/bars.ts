// Loading a chart's bars from the candle service (PR 3): a few days of
// history, then the stream's updates merged in.

import type { KLineData } from 'klinecharts';
import { apiFetch } from '../../lib/api';
import { TF_DAYS, type Bar, type Tf } from './model';

export const toK = (b: Bar): KLineData => ({ timestamp: b.t, open: b.o, high: b.h, low: b.l, close: b.c, volume: b.v ?? undefined });

function isoDay(t: number): string {
  return new Intl.DateTimeFormat('en-CA', { timeZone: 'Asia/Kolkata' }).format(new Date(t));
}

export async function fetchBars(base: string, symbol: string, tf: Tf): Promise<Bar[]> {
  const to = Date.now();
  const from = to - (TF_DAYS[tf] - 1) * 86_400_000;
  const res = await apiFetch(base, `/api/v2/candles?symbol=${encodeURIComponent(symbol)}&tf=${tf}&from=${isoDay(from)}&to=${isoDay(to)}`);
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error ?? `Bars unavailable (${res.status})`);
  }
  const body = (await res.json()) as { bars: Bar[] };
  return body.bars;
}

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
