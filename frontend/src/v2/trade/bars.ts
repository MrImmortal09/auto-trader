// Loading a chart's bars from the candle service (PR 3): a few days of
// history, then the stream's updates merged in.

import type { Chart, KLineData } from 'klinecharts';
import { apiUrl } from '../../lib/api';
import { getCandles, streamToken } from '../lib/api';
import { TF_DAYS, type Bar, type Tf } from './model';

/** The first wait before a closed candle stream is opened again; it doubles up to the second. */
const STREAM_RETRY_MS = 2_000;
const STREAM_RETRY_MAX_MS = 60_000;

/** One bar the candle stream says changed. */
export interface StreamUpdate {
  symbol: string;
  bar: Bar;
}

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

/**
 * Follow `symbols` at `tf` on the candle stream until the returned function is
 * called; `onBars` gets each event's updates. EventSource reconnects by itself
 * only after a network error: an answer that isn't the stream (nginx's 502
 * while the server restarts for a deploy, the service's own 503 while it
 * restarts) closes it for good, and the chart would sit still until a reload.
 * So a closed stream is opened again here, backing off to a minute, with the
 * token read afresh. The stream sends only what changes after it is open, so
 * `onReopen` runs on every open after the first, to fetch again what changed
 * while it was down.
 */
export function followCandles(base: string, symbols: string[], tf: Tf, onBars: (updates: StreamUpdate[]) => void, onReopen: () => void): () => void {
  let source: EventSource | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let wait = STREAM_RETRY_MS;
  let opened = false;
  let stopped = false;
  const connect = () => {
    const params = new URLSearchParams({ symbols: symbols.join(','), tf });
    const token = streamToken();
    if (token) params.set('token', token);
    const es = new EventSource(`${apiUrl(base, '/api/v2/candles/stream')}?${params}`);
    source = es;
    es.addEventListener('open', () => {
      wait = STREAM_RETRY_MS;
      if (opened) onReopen();
      opened = true;
    });
    es.addEventListener('bars', (e) => {
      let body: { updates: StreamUpdate[] };
      try {
        body = JSON.parse((e as MessageEvent<string>).data);
      } catch {
        return;
      }
      onBars(body.updates);
    });
    // Also the stream's own `error` event (symbols it could not follow),
    // which comes while it is open and is not a reason to reconnect.
    es.addEventListener('error', () => {
      if (stopped || es.readyState !== EventSource.CLOSED) return;
      retry = setTimeout(connect, wait);
      wait = Math.min(wait * 2, STREAM_RETRY_MAX_MS);
    });
  };
  connect();
  return () => {
    stopped = true;
    clearTimeout(retry);
    source?.close();
  };
}

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
