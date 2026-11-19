// Loading a chart's bars from the candle service (PR 3): a few days of
// history, then the stream's updates merged in.

import type { Chart, KLineData } from 'klinecharts';
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

/** Merge a streamed bar into a sorted list: replace its start, append it, or
 * put it in its place (a minute the service filled in late). */
export function upsert(bars: Bar[], bar: Bar): Bar[] {
  const last = bars[bars.length - 1];
  if (!last || bar.t > last.t) return [...bars, bar];
  let i = bars.length - 1;
  while (i >= 0 && bars[i].t > bar.t) i--;
  const next = bars.slice();
  if (i >= 0 && bars[i].t === bar.t) next[i] = bar;
  else next.splice(i + 1, 0, bar);
  return next;
}

/**
 * Put streamed bars (oldest first) on a KLineChart. Its own update, `push`
 * (the data loader's `subscribeBar` callback), only replaces the last bar or
 * appends a newer one, and drops anything older without a word. The candle
 * service sends older bars too: a closed bar again once Upstox's official
 * minutes correct it (a bar's final minute always lands after the next bar has
 * opened), and a minute it filled in late. Dropped, the bar kept what the ticks
 * alone had built and hung apart from its neighbours until a reload. So an
 * older bar is written into the chart's own list (KLineChart hands back the
 * array it draws from), replaced or inserted in order, and the last bar is
 * pushed again, which redraws and recomputes the indicators.
 */
export function putOnChart(chart: Chart, push: (k: KLineData) => void, bars: Bar[]) {
  const data = chart.getDataList();
  let older = false;
  for (const bar of bars) {
    const k = toK(bar);
    const last = data[data.length - 1];
    if (!last || k.timestamp >= last.timestamp) {
      push(k);
      older = false;
      continue;
    }
    let i = data.length - 1;
    while (i >= 0 && data[i].timestamp > k.timestamp) i--;
    if (i >= 0 && data[i].timestamp === k.timestamp) data[i] = k;
    else data.splice(i + 1, 0, k);
    older = true;
  }
  const last = data[data.length - 1];
  if (older && last) push(last);
}
