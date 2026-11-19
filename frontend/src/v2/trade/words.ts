// Words for a manual trade: what it is doing, and how a level fires.

import type { ChartLevel, EntryLine, ExecutePosition, LimitClose } from '../../types';
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

/** "150 @ ₹152.00", with "· 75 sold" once part of it has filled. */
export function limitWords(lc: LimitClose): string {
  return `${lc.qty} @ ₹${price(lc.price)}${lc.filled > 0 ? ` · ${lc.filled} sold` : ''}`;
}

/** Where a limit close is: on the owner's LIVE desk at Kotak, else watched
 * here for the price. */
export function limitStatus(lc: LimitClose, live: boolean): string {
  switch (lc.status) {
    case 'sending':
      return 'Sending to Kotak…';
    case 'unconfirmed':
      return 'No answer from Kotak — checking its order book; nothing else sells meanwhile';
    case 'missing':
      return "Not in Kotak's order book — it may still rest there, so nothing else sells. Check the Kotak app (cancel it there if it rests), then × releases it";
    case 'cancelling':
      return 'Cancelling…';
    default:
      return live ? 'Resting at Kotak until it fills, is cancelled, or the close' : 'Fills when the price reaches it; lapses at the close';
  }
}

/** Whether its × can cancel it now: not while it is being cancelled, or
 * while Kotak hasn't answered its send — until it is `missing`, when the ×
 * releases it (after the owner has checked Kotak). */
export function limitCancellable(lc: LimitClose): boolean {
  return lc.status === 'resting' || lc.status === 'sending' || lc.status === 'missing';
}
