// Vela's data provider over the candle service: history from /api/v2/candles,
// the forming bar from the stream the chart opens, and the NSE/BSE session as
// the market calendar so the time axis skips closed hours.

import type { DataProvider, OHLCV, ProviderInfo } from '@luxalgo/vela';
import { fetchBars, upsert } from './bars';
import type { Bar, Tf } from './model';

export const PROVIDER = 'at';

/** Vela reads `EXCHANGE:` as a provider prefix, so the candle service's
 * `NIFTY:2026-10-27:25100:CE` travels as `at:NIFTY_2026-10-27_25100_CE`. */
export const velaSymbol = (symbol: string) => `${PROVIDER}:${symbol.replace(/:/g, '_')}`;
export const apiSymbol = (ticker: string) => ticker.replace(/_/g, ':');

export const toOhlcv = (b: Bar): OHLCV => ({ time: b.t, open: b.o, high: b.h, low: b.l, close: b.c, volume: b.v ?? undefined });

const TF_OF: Record<string, Tf> = { '1': '1m', '1m': '1m', '5': '5m', '5m': '5m', '15': '15m', '15m': '15m' };
/** The candle service's timeframe for a Vela one (`'5'` or `'5m'`). */
export const tfOf = (timeframe: string): Tf => TF_OF[timeframe] ?? '5m';

const IST_MS = 330 * 60_000;
const DAY_MS = 86_400_000;

/** 09:15–15:30 IST on each weekday overlapping `[from, to)`, as `[start, end)` pairs. */
export function sessionWindows(from: number, to: number): [number, number][] {
  const out: [number, number][] = [];
  // IST midnight of `from`'s day, as a Unix instant.
  let day = Math.floor((from + IST_MS) / DAY_MS) * DAY_MS - IST_MS;
  for (; day < to; day += DAY_MS) {
    const weekday = new Date(day + IST_MS).getUTCDay();
    if (weekday === 0 || weekday === 6) continue;
    const start = day + (9 * 60 + 15) * 60_000;
    const end = day + (15 * 60 + 30) * 60_000;
    if (end > from && start < to) out.push([start, end]);
  }
  return out;
}

export interface Feed {
  /** The live listener Vela registered, if any. */
  onBar: ((bar: OHLCV) => void) | null;
}

/**
 * One chart's provider. A symbol's bars are fetched once and kept, so Vela's
 * backfill requests (older `to`) are answered from them and end at our
 * oldest bar; every load is reported to the component, which keeps the same
 * bars for its lines, VWAP and pivots.
 */
export class CandleProvider implements DataProvider {
  private cache: { key: string; bars: Bar[] } | null = null;
  private readonly serverBase: () => string;
  private readonly feed: Feed;
  private readonly onLoaded: (symbol: string, tf: Tf, bars: Bar[]) => void;
  private readonly onError: (symbol: string, error: string) => void;

  constructor(serverBase: () => string, feed: Feed, onLoaded: (symbol: string, tf: Tf, bars: Bar[]) => void, onError: (symbol: string, error: string) => void) {
    this.serverBase = serverBase;
    this.feed = feed;
    this.onLoaded = onLoaded;
    this.onError = onError;
  }

  info(): ProviderInfo {
    return { name: PROVIDER, displayName: 'Auto Trader', supportedTimeframes: ['1m', '5m', '15m'], capabilities: { enumerate: false, stream: true, symbolInfo: false } };
  }

  async getBars(ticker: string, timeframe: string, range: { from?: number; to?: number; limit?: number }): Promise<OHLCV[]> {
    const symbol = apiSymbol(ticker);
    const tf = tfOf(timeframe);
    const key = `${symbol}|${tf}`;
    if (this.cache?.key !== key) {
      try {
        const bars = await fetchBars(this.serverBase(), symbol, tf);
        this.cache = { key, bars };
        this.onLoaded(symbol, tf, bars);
      } catch (e) {
        this.onError(symbol, e instanceof Error ? e.message : 'Bars unavailable');
        throw e;
      }
    }
    let bars = this.cache.bars;
    const { from, to, limit } = range;
    if (from != null) bars = bars.filter((b) => b.t >= from);
    if (to != null) bars = bars.filter((b) => b.t <= to);
    if (limit != null && bars.length > limit) bars = bars.slice(-limit);
    return bars.map(toOhlcv);
  }

  subscribe(_ticker: string, _timeframe: string, onBar: (bar: OHLCV) => void): () => void {
    this.feed.onBar = onBar;
    return () => {
      if (this.feed.onBar === onBar) this.feed.onBar = null;
    };
  }

  async getCalendar(_ticker: string, range: { from: number; to: number }): Promise<ReadonlyArray<readonly [number, number]>> {
    return sessionWindows(range.from, range.to);
  }

  /** A streamed bar of `symbol` at `tf`: into the kept bars, and to Vela. */
  push(symbol: string, tf: Tf, bar: Bar) {
    if (this.cache?.key !== `${symbol}|${tf}`) return;
    this.cache.bars = upsert(this.cache.bars, bar);
    this.feed.onBar?.(toOhlcv(bar));
  }

  /** The next load starts from nothing (a market switch). */
  forget() {
    this.cache = null;
  }
}
