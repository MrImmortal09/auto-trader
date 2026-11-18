// Words for a manual trade: what it is doing, and how a level fires.

import type { ChartLevel, EntryLine, ExecutePosition } from '../../types';
import { chartTargets, targetLotsLabel } from '../../lib/format';
import { price } from '../lib/format';
import type { LevelKind } from './lines';
import { lineUntil } from './lineOrder';

/** What a trade is doing, if anything. */
export function busyNote(p: ExecutePosition, sending: boolean): string | null {
  if (p.halted) return 'Halted by the engine — see System';
  if (p.placing) return p.line && !p.line.triggered_at ? 'Waiting on its buy line' : 'Buying…';
  if (p.adding) return 'Adding…';
  if (p.exiting) return 'Exiting…';
  return sending ? 'Sending…' : null;
}

/** "NIFTY ≤ 25,060.00" or "₹92.50". */
export function levelWords(p: ExecutePosition, kind: LevelKind, lvl: ChartLevel): string {
  if (lvl.trigger === 'premium') return `₹${price(lvl.price)}`;
  const falls = (kind === 'stop') === (p.option_type === 'CE');
  return `${p.index} ${falls ? '≤' : '≥'} ${price(lvl.price)}`;
}

/** "Buys 4 lots when NIFTY ≤ 25,980.00 · till 11:45 · stop NIFTY ≤ 25,950.00
 * · NIFTY ≥ 26,050.00 (1 lot), NIFTY ≥ 26,100.00 (3 lots)": a line order. */
export function lineSummary(p: ExecutePosition, line: EntryLine): string {
  const lots = p.lots ?? 0;
  const stop = p.chart?.stop;
  const targets = chartTargets(p.chart);
  return [
    `Buys ${lots} ${lots === 1 ? 'lot' : 'lots'} when ${p.index} ${line.rising ? '≥' : '≤'} ${price(line.price)}`,
    lineUntil(line),
    stop ? `stop ${levelWords(p, 'stop', stop)}` : null,
    targets.length ? targets.map((t) => `${levelWords(p, 'target', t)} (${targetLotsLabel(t.lots)})`).join(', ') : null,
  ]
    .filter(Boolean)
    .join(' · ');
}
