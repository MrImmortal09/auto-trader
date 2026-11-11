import { INDEX_ORDER, readStored, writeStored } from '../../lib/execute';
import type { ChartSym } from '../trade/model';
import { TRADE_KEY } from './layout';
import type { InstrumentRow } from './types';

// Where a search result, a watchlist row or an instrument page sends you.
// The Trade screen charts the six indices and their options and ladders the
// indices; with the `stock_charts` switch on (`stocksOn`), any stock too —
// its own chart, its futures and options, and the ladder of any F&O stock.
// Everything else opens its instrument page.

/** The stock the Trade screen shows as a seventh tab beside the six indices:
 * the one last chosen from a search, a watchlist row or an instrument page.
 * `key` is its cash row's price-map key (`nse_cm|2885`), for its price on the
 * tab; null when the row that chose it was a contract. `fo`: it has options,
 * so the tab can put it on the ladder. */
export interface StockChip {
  symbol: string;
  key: string | null;
  fo: boolean;
}

/** What the Trade screen should show when it opens: slot 1's chart and/or
 * the ladder's underlying (and expiry), and the stock tab. */
export interface TradeIntent {
  chart?: ChartSym;
  ladder?: { index: string; expiry?: string | null };
  stock?: StockChip;
}

/** One of the six indices the Trade screen knows, by its catalog symbol. */
export function tradeIndexOf(row: Pick<InstrumentRow, 'kind' | 'symbol' | 'underlying'>): string | null {
  const index = row.kind === 'INDEX' ? row.symbol : row.underlying;
  return INDEX_ORDER.includes(index) ? index : null;
}

type RowFacts = Pick<InstrumentRow, 'kind' | 'symbol' | 'underlying' | 'expiry' | 'strike' | 'exchange' | 'has_fo' | 'ws_key'>;

/**
 * The chart the Trade screen can draw for `row`: an index or an index's
 * option always; with `stocksOn`, a stock, a stock's future or option too.
 * `null` for an index's future (no row of the six has one in the catalog
 * the ladder knows) and for a stock while the switch is off.
 */
export function chartSymOf(row: Pick<RowFacts, 'kind' | 'symbol' | 'underlying' | 'expiry' | 'strike' | 'exchange'>, stocksOn: boolean): ChartSym | null {
  const index = tradeIndexOf(row);
  if (row.kind === 'INDEX') return index ? { kind: 'index', index } : null;
  const underlying = index ?? (stocksOn ? row.underlying : null);
  if (!underlying) return null;
  if (row.kind === 'EQ') return stocksOn ? { kind: 'equity', symbol: row.symbol, exchange: row.exchange === 'BSE' ? 'BSE' : 'NSE' } : null;
  if (row.kind === 'FUT') return { kind: 'future', index: underlying };
  if ((row.kind === 'CE' || row.kind === 'PE') && row.expiry && row.strike != null) return { kind: 'option', index: underlying, expiry: row.expiry, strike: row.strike, side: row.kind };
  return null;
}

/** The chart for `row` as the screens knew it before stock charts: an
 * index or an index's option. */
export function chartOf(row: Pick<InstrumentRow, 'kind' | 'symbol' | 'underlying' | 'expiry' | 'strike'>): ChartSym | null {
  return chartSymOf({ ...row, exchange: 'NSE' }, false);
}

/** The ladder `row` can open: its index, or with `stocksOn` its F&O stock,
 * on a contract's own expiry. */
export function ladderOf(row: Pick<RowFacts, 'kind' | 'symbol' | 'underlying' | 'expiry' | 'has_fo'>, stocksOn: boolean): { index: string; expiry?: string | null } | null {
  const index = tradeIndexOf(row) ?? (stocksOn && row.has_fo ? row.underlying : null);
  if (!index) return null;
  const derivative = row.kind === 'FUT' || row.kind === 'CE' || row.kind === 'PE';
  return { index, expiry: derivative ? row.expiry : null };
}

/** The stock tab `row` puts on the Trade screen: its underlying, when that
 * is a stock; nothing for an index or an index's contract. */
export function stockChipOf(row: Pick<RowFacts, 'kind' | 'symbol' | 'underlying' | 'has_fo' | 'ws_key'>): StockChip | undefined {
  if (tradeIndexOf(row)) return undefined;
  return { symbol: row.underlying, key: row.kind === 'EQ' ? row.ws_key : null, fo: row.has_fo };
}

/** How a row is opened: `open` is Enter or a tap on a search result,
 * `ladder` Shift+Enter, `chart` a chart button (a search result's, a
 * watchlist row's, the instrument page's Open chart), `page` a watchlist
 * row's tap. */
export type OpenHow = 'open' | 'ladder' | 'chart' | 'page';

/**
 * Where `row` opens, with the stock switch `stocksOn`. An index: the Trade
 * screen, charted (`open`, `chart`, `page`) or on the ladder (`ladder`). A
 * watchlist row's tap (`page`) otherwise keeps the instrument page a tap
 * away, since the row's own Chart and Chain actions reach the Trade screen.
 * Any other row: on the ladder (`ladder`) where it has one — its index at an
 * option's expiry, or while stocks are on its F&O stock — or charted
 * (`chart`; `open` too while stocks are on, the plan's "Enter on any search
 * row opens its chart"), with the stock tab and the ladder on its underlying
 * at a contract's expiry (an index option's chart button: its index at its
 * expiry, as the instrument page's Open chart does); its instrument page when
 * the Trade screen has nothing to show for it.
 */
export function targetOf(row: InstrumentRow, how: OpenHow, stocksOn: boolean): { trade: TradeIntent } | { instrument: string } {
  const index = tradeIndexOf(row);
  if (index && row.kind === 'INDEX') {
    return { trade: how === 'ladder' ? { ladder: { index } } : { chart: { kind: 'index', index }, ladder: { index } } };
  }
  const page = { instrument: row.id };
  if (how === 'page') return page;
  const ladder = ladderOf(row, stocksOn);
  const stock = stockChipOf(row);
  if (how === 'ladder') return ladder ? { trade: { ladder, stock } } : page;
  const chart = chartSymOf(row, stocksOn);
  // Before stocks, Enter on an index's option opened its page; it still does while the switch is off.
  if (chart && (how === 'chart' || stocksOn)) return { trade: { chart, ladder: ladder ?? undefined, stock } };
  return page;
}

/**
 * Point the Trade screen's saved layout (`v2.trade`) at `intent` before it
 * opens: slot 1's symbol, the ladder's underlying and its expiry, the stock
 * tab. Everything else saved (timeframes, layers, pane sizes) stays. The
 * screen reads this when it mounts, so the caller remounts it if it is
 * already open.
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
  if (intent.stock) next.stock = intent.stock;
  writeStored(TRADE_KEY, next);
}
