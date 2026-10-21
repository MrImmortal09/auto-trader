// The Market screen's model: the engine's indicator settings, how a check is
// scored (the same rules as `shared_domain::entry_gate`), and the words for
// a bias and for what a call arriving now would get.

import type { TradingConfig } from '../../types';
import { MINUS, price } from '../lib/format';
import type { CallVerdict, ChainStrike, IndicatorReading, IndicatorScore, MarketIndex, Tone, TrendRow } from '../lib/types';

export const MARKET_INDICES: { id: MarketIndex; label: string }[] = [
  { id: 'NIFTY', label: 'NIFTY 50' },
  { id: 'BANKNIFTY', label: 'BANKNIFTY' },
  { id: 'SENSEX', label: 'SENSEX' },
];

export type Basis = 'chg' | 'total';

export const WINDOWS: { value: string; label: string }[] = [
  { value: '5', label: 'ATM ±5' },
  { value: '10', label: '±10' },
  { value: '20', label: '±20' },
  { value: '0', label: 'All' },
];

/** "ATM ±5", "all strikes". */
export function windowLabel(window: number): string {
  return window === 0 ? 'all strikes' : `ATM ±${window}`;
}

/** What the engine scores a check with — the saved settings. */
export interface ScoreSettings {
  interval: number;
  basis: Basis;
  window: number;
  pcrBuyAbove: number;
  pcrSellBelow: number;
  vwapBandPct: number;
}

/** The server's defaults, until the settings load. */
export const DEFAULT_SCORE: ScoreSettings = { interval: 15, basis: 'chg', window: 10, pcrBuyAbove: 1, pcrSellBelow: 1, vwapBandPct: 0 };

export function scoreSettings(cfg: TradingConfig | null): ScoreSettings {
  if (!cfg) return DEFAULT_SCORE;
  return {
    interval: cfg.indicator_interval_min || DEFAULT_SCORE.interval,
    basis: cfg.indicator_basis === 'total' ? 'total' : 'chg',
    window: cfg.indicator_strike_window ?? DEFAULT_SCORE.window,
    pcrBuyAbove: cfg.indicator_pcr_buy_above ?? 1,
    pcrSellBelow: cfg.indicator_pcr_sell_below ?? 1,
    vwapBandPct: cfg.indicator_vwap_band_pct ?? 0,
  };
}

/** Put ÷ call, only when both sides are positive. */
export function pcr(call: number, put: number): number | null {
  return call > 0 && put > 0 ? put / call : null;
}

/** The two sides a basis compares. */
export function sides(row: Pick<TrendRow, 'call_oi' | 'put_oi' | 'call_oi_chg' | 'put_oi_chg'>, basis: Basis): [number, number] {
  return basis === 'chg' ? [row.call_oi_chg, row.put_oi_chg] : [row.call_oi, row.put_oi];
}

function optionScore(call: number, put: number, s: ScoreSettings): number {
  const ratio = pcr(call, put);
  if (ratio != null) return ratio > s.pcrBuyAbove ? 1 : ratio < s.pcrSellBelow ? -1 : 0;
  return put > call ? 1 : put < call ? -1 : 0;
}

function vwapScore(price: number | null, vwap: number | null, s: ScoreSettings): number {
  if (price == null || vwap == null || price <= 0 || vwap <= 0) return 0;
  const band = (vwap * s.vwapBandPct) / 100;
  return price > vwap + band ? 1 : price < vwap - band ? -1 : 0;
}

/** A check scored the engine's way, on `basis`. */
export function scoreRow(row: TrendRow, s: ScoreSettings, basis: Basis): IndicatorScore {
  const [call, put] = sides(row, basis);
  const option = optionScore(call, put, s);
  const vwap = vwapScore(row.fut_ltp, row.fut_vwap, s);
  return { call, put, ratio: pcr(call, put), option, vwap, overall: option + vwap };
}

/** "+2", "0", "−1". */
export function signedScore(n: number): string {
  return n > 0 ? `+${n}` : n < 0 ? `${MINUS}${-n}` : '0';
}

export function scoreTone(n: number): Tone {
  return n > 0 ? 'profit' : n < 0 ? 'loss' : 'neutral';
}

/** "Strong bullish", "Bearish", "Neutral". */
export function biasWord(overall: number): string {
  if (overall >= 2) return 'Strong bullish';
  if (overall === 1) return 'Bullish';
  if (overall === -1) return 'Bearish';
  if (overall <= -2) return 'Strong bearish';
  return 'Neutral';
}

/** The bias chip: "Strong bullish · +2", "Neutral · opening check". */
export function biasChip(reading: IndicatorReading | null): { text: string; tone: Tone } {
  if (!reading) return { text: 'No fresh check', tone: 'neutral' };
  if (reading.opening) return { text: 'Neutral · opening check', tone: 'neutral' };
  const o = reading.score.overall;
  return { text: `${biasWord(o)} · ${signedScore(o)}`, tone: scoreTone(o) };
}

/** "Buy", "Sell", or the neutral word for a component. */
export function signalWord(score: number, neutral: string): string {
  return score > 0 ? 'Buy' : score < 0 ? 'Sell' : neutral;
}

/** "PCR 1.24 · above 1.00" — where the ratio sits against the thresholds. */
export function pcrLine(score: IndicatorScore, s: ScoreSettings): string {
  const r = score.ratio;
  if (r == null) {
    const diff = score.put - score.call;
    return `PCR n/a · puts ${diff >= 0 ? 'over' : 'under'} calls by ${lakh(Math.abs(diff), false)}`;
  }
  const at = r.toFixed(2);
  if (s.pcrBuyAbove === s.pcrSellBelow) {
    const t = s.pcrBuyAbove.toFixed(2);
    return `PCR ${at} · ${r > s.pcrBuyAbove ? 'above' : r < s.pcrSellBelow ? 'below' : 'at'} ${t}`;
  }
  if (r > s.pcrBuyAbove) return `PCR ${at} · above ${s.pcrBuyAbove.toFixed(2)}`;
  if (r < s.pcrSellBelow) return `PCR ${at} · below ${s.pcrSellBelow.toFixed(2)}`;
  return `PCR ${at} · between ${s.pcrSellBelow.toFixed(2)} and ${s.pcrBuyAbove.toFixed(2)}`;
}

const LOTS = (n: number) => `${n} ${n === 1 ? 'lot' : 'lots'}`;

/** What a CE or PE call arriving now would get, in the card's words. */
export function verdictLine(v: CallVerdict, side: 'CE' | 'PE', reading: IndicatorReading | null, sizingOn: boolean): { text: string; tone: 'strong' | 'plain' | 'muted' } {
  if (v.base_lots <= 0) return { text: 'Not traded · no lots set', tone: 'muted' };
  if (!sizingOn) return { text: `Buys ${LOTS(v.base_lots)} · indicator sizing is off`, tone: 'muted' };
  if (v.action === 'WAIT') return { text: 'Waits · the indicator disagrees', tone: 'muted' };
  if (v.action === 'SKIP') return { text: 'Skipped · the indicator disagrees', tone: 'muted' };
  const lots = v.lots ?? v.base_lots;
  const m = v.multiplier ?? 1;
  const sized = `Sized ×${m.toFixed(1)} · ${LOTS(v.base_lots)} ${lots === v.base_lots ? (v.base_lots === 1 ? 'stays' : 'stay') : v.base_lots === 1 ? 'becomes' : 'become'} ${lots}`;
  const o = reading?.opening ? 0 : reading?.score.overall ?? 0;
  const strong = side === 'CE' ? o >= 2 : o <= -2;
  return { text: sized, tone: strong ? 'strong' : 'plain' };
}

/** "+56.9 L": open interest in lakh, with its sign when `signed`. */
export function lakh(n: number, signed = true): string {
  const v = Math.abs(n) / 1e5;
  const body = `${v >= 100 ? Math.round(v).toLocaleString('en-IN') : v.toFixed(1)} L`;
  if (!signed) return body;
  return n > 0.05e5 ? `+${body}` : n < -0.05e5 ? `${MINUS}${body}` : body;
}

const minutes = (hhmm: string) => Number(hhmm.slice(0, 2)) * 60 + Number(hhmm.slice(3, 5));
const clock = (m: number) => `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`;
const FIRST_CHECK = 9 * 60 + 16;
const LAST_CHECK = 15 * 60 + 31;

/** The check after the one at `hhmm`: one minute past each interval
 * boundary, 09:16 first and 15:31 last (`entry_gate::check_minute`). */
export function nextCheck(hhmm: string, interval: number): string | null {
  const m = minutes(hhmm);
  if (m >= LAST_CHECK) return null;
  const next = m < FIRST_CHECK ? FIRST_CHECK : Math.ceil(m / interval) * interval + 1;
  return clock(Math.min(next, LAST_CHECK));
}

/** A chain's strikes within `window` of its ATM (0: the whole chain). */
export function strikesAround(strikes: ChainStrike[], atm: number | null, window: number): ChainStrike[] {
  if (window === 0 || atm == null) return strikes;
  const i = strikes.findIndex((s) => s.strike === atm);
  if (i < 0) return strikes;
  return strikes.slice(Math.max(0, i - window), i + window + 1);
}

/** A strike's calls and puts on `basis`: change since yesterday, or total. */
export function strikeSides(s: ChainStrike, basis: Basis): [number, number] {
  return basis === 'chg' ? [s.call_oi - s.call_prev_oi, s.put_oi - s.put_prev_oi] : [s.call_oi, s.put_oi];
}

/** "Futures 25,168.40 · VWAP 25,126.10". */
export function futuresLine(fut: number | null, vwap: number | null): string {
  const f = (n: number | null) => (n == null ? '—' : price(n));
  return `Futures ${f(fut)} · VWAP ${f(vwap)}`;
}

const IST_MS = 330 * 60_000;
const istDay = (t: number) => Math.floor((t + IST_MS) / 86_400_000);

/**
 * PCR at each bar: the latest check taken before the bar closed, on the
 * bar's own day — before a day's first check there is none.
 */
export function pcrByBar(bars: { t: number }[], checks: { t: number; v: number }[], tfMs: number): Map<number, number> {
  const sorted = [...checks].sort((a, b) => a.t - b.t);
  const out = new Map<number, number>();
  let j = -1;
  for (const b of [...bars].sort((x, y) => x.t - y.t)) {
    while (j + 1 < sorted.length && sorted[j + 1].t < b.t + tfMs) j++;
    if (j >= 0 && istDay(sorted[j].t) === istDay(b.t)) out.set(b.t, sorted[j].v);
  }
  return out;
}

/** The last close against the previous day's last close, in percent. */
export function dayChange(bars: { t: number; c: number }[]): number | null {
  if (bars.length === 0) return null;
  const last = bars[bars.length - 1];
  for (let i = bars.length - 2; i >= 0; i--) {
    if (istDay(bars[i].t) < istDay(last.t)) return bars[i].c > 0 ? ((last.c - bars[i].c) / bars[i].c) * 100 : null;
  }
  return null;
}
