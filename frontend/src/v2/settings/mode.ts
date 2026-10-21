// Switching between Paper and LIVE: the engine trades every position in the
// one current mode, so a switch changes what happens to what's already open.

import { plural } from '../lib/format';
import type { ModeCheck } from '../lib/types';

/** "NIFTY 2 · BANKNIFTY 1 · …": what each index's call buys. */
export function lotsLine(lots: ModeCheck['lots']): string {
  return lots.map((l) => `${l.index} ${l.lots}`).join(' · ');
}

/** What the switch does to calls still waiting: the engine trades every
 * position in the one mode, so they buy on Kotak. */
export function consequences(c: ModeCheck): string[] {
  if (c.waiting === 0) return [];
  return [`${c.waiting === 1 ? 'The waiting call buys' : `The ${c.waiting} waiting calls buy`} on Kotak, with real money, when ${c.waiting === 1 ? 'it triggers' : 'they trigger'}.`];
}

/** Why switching to LIVE isn't possible now, or null. LIVE manages every
 * open position as if Kotak held it — and Kotak holds no paper position —
 * so those are closed first; and LIVE can't place an order without Kotak. */
export function liveBlockedReason(c: ModeCheck): string | null {
  if (c.open > 0) {
    return `Close the ${plural(c.open, 'open paper position')} first. LIVE manages every open position as if Kotak held it, and Kotak doesn't hold ${c.open === 1 ? 'this one' : 'these'}.`;
  }
  if (!c.kotak_connected) return "Kotak isn't logged in. Log in first: LIVE can't place an order without it.";
  return null;
}

/** Why switching to Paper isn't safe now, or null. In Paper the engine
 * would only pretend to sell LIVE positions, leaving them open at Kotak. */
export function paperBlockedReason(c: ModeCheck): string | null {
  if (c.mode !== 'LIVE' || c.open === 0) return null;
  return `Exit the ${plural(c.open, 'LIVE position')} first: in Paper ${c.open === 1 ? 'it' : 'they'} would stay open at Kotak with nothing watching ${c.open === 1 ? 'it' : 'them'}.`;
}
