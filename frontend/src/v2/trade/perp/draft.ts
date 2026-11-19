// The order being set up in the perps ticket, as far as the chart draws it:
// a line order's line, stop and target as they are placed, and where the
// position it opens would be liquidated. Kept once for the page (the ticket
// writes it, the chart lines read it), and cleared when the ticket closes
// or the order is sent.

import { useSyncExternalStore } from 'react';
import type { PerpSide } from '../../lib/types';
import type { PerpKey } from '../model';

export interface PerpDraft {
  key: PerpKey;
  side: PerpSide;
  /** Where it would fill: the mid for a market order, else its price. */
  entry: number;
  leverage: number;
  /** The estimated liquidation price; null for an order that opens nothing. */
  liq: number | null;
  /** The margin it would put up, when a size is typed: what liquidation loses. */
  margin: number | null;
  /** A line order: it fills only once the mark crosses `entry`. Its line,
   * stop and target are drawn too until it is armed. */
  line: boolean;
  /** In the coin, when known. */
  size: number | null;
  stop: number | null;
  target: number | null;
}

let draft: PerpDraft | null = null;
const listeners = new Set<() => void>();

const same = (a: PerpDraft | null, b: PerpDraft | null) =>
  a === b ||
  (a != null &&
    b != null &&
    a.key === b.key &&
    a.side === b.side &&
    a.entry === b.entry &&
    a.leverage === b.leverage &&
    a.liq === b.liq &&
    a.margin === b.margin &&
    a.line === b.line &&
    a.size === b.size &&
    a.stop === b.stop &&
    a.target === b.target);

export function setPerpDraft(next: PerpDraft | null) {
  if (same(draft, next)) return;
  draft = next;
  listeners.forEach((fn) => fn());
}

export function usePerpDraft(): PerpDraft | null {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => {
        listeners.delete(fn);
      };
    },
    () => draft,
  );
}
