// Words for a manual trade: what it is doing, and how a level fires.

import type { ChartLevel, ExecutePosition } from '../../types';
import { price } from '../lib/format';
import type { LevelKind } from './lines';

/** What a trade is doing, if anything. */
export function busyNote(p: ExecutePosition, sending: boolean): string | null {
  if (p.halted) return 'Halted by the engine — see System';
  if (p.placing) return 'Buying…';
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
