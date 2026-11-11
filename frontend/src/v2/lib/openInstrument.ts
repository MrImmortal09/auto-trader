import { INDEX_ORDER, readStored, writeStored } from '../../lib/execute';
import type { ChartSym } from '../trade/model';
import { TRADE_KEY } from './layout';
import type { InstrumentRow } from './types';

// Where a search result or an instrument page sends you. The Trade screen
// charts the six indices and their options and ladders the indices, so
// those open there; everything else opens its instrument page.

/** What the Trade screen should show when it opens: slot 1's chart and/or
 * the ladder's index (and expiry). */
export interface TradeIntent {
  chart?: ChartSym;
  ladder?: { index: string; expiry?: string | null };
}

/** One of the six indices the Trade screen knows, by its catalog symbol. */
export function tradeIndexOf(row: Pick<InstrumentRow, 'kind' | 'symbol' | 'underlying'>): string | null {
  const index = row.kind === 'INDEX' ? row.symbol : row.underlying;
  return INDEX_ORDER.includes(index) ? index : null;
}

/** The chart the Trade screen can draw for `row`: an index, or an index's
 * option; `null` for a stock, a future or a stock option (Phase 3). */
export function chartOf(row: Pick<InstrumentRow, 'kind' | 'symbol' | 'underlying' | 'expiry' | 'strike'>): ChartSym | null {
  const index = tradeIndexOf(row);
  if (!index) return null;
  if (row.kind === 'INDEX') return { kind: 'index', index };
  if ((row.kind === 'CE' || row.kind === 'PE') && row.expiry && row.strike != null) return { kind: 'option', index, expiry: row.expiry, strike: row.strike, side: row.kind };
  return null;
}

/** How a row is opened: `open` is Enter or a tap, `ladder` Shift+Enter,
 * `chart` a result row's chart button. */
export type OpenHow = 'open' | 'ladder' | 'chart';

/** Where `row` opens: the Trade screen for an index (Enter, the chart
 * button) or for an index put on the ladder (Shift+Enter, also from an
 * index option, on its expiry); an index option's chart button charts it
 * there, with the ladder on its index at its expiry (as the instrument
 * page's Open chart does); its instrument page otherwise. */
export function targetOf(row: InstrumentRow, how: OpenHow): { trade: TradeIntent } | { instrument: string } {
  const index = tradeIndexOf(row);
  if (index && row.kind === 'INDEX') {
    return { trade: how === 'ladder' ? { ladder: { index } } : { chart: { kind: 'index', index }, ladder: { index } } };
  }
  if (index && how === 'ladder' && (row.kind === 'CE' || row.kind === 'PE')) {
    return { trade: { ladder: { index, expiry: row.expiry } } };
  }
  const chart = how === 'chart' ? chartOf(row) : null;
  if (index && chart) return { trade: { chart, ladder: { index, expiry: row.expiry } } };
  return { instrument: row.id };
}

/**
 * Point the Trade screen's saved layout (`v2.trade`) at `intent` before it
 * opens: slot 1's symbol, the ladder's index and its expiry. Everything
 * else saved (timeframes, layers, pane sizes) stays. The screen reads this
 * when it mounts, so the caller remounts it if it is already open.
 */
export function pointTradeAt(intent: TradeIntent) {
  const saved = readStored<Record<string, unknown>>(TRADE_KEY, {});
  const next: Record<string, unknown> = { ...saved };
  if (intent.chart) {
    const slots = Array.isArray(saved.slots) ? [...(saved.slots as unknown[])] : [];
    const first = slots[0] && typeof slots[0] === 'object' ? (slots[0] as Record<string, unknown>) : {};
    slots[0] = { ...first, sym: intent.chart };
    next.slots = slots;
  }
  if (intent.ladder) {
    next.index = intent.ladder.index;
    if (intent.ladder.expiry) {
      const expiry = saved.expiry && typeof saved.expiry === 'object' ? (saved.expiry as Record<string, unknown>) : {};
      next.expiry = { ...expiry, [intent.ladder.index]: intent.ladder.expiry };
    }
  }
  writeStored(TRADE_KEY, next);
}
