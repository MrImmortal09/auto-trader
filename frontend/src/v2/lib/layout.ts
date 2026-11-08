// The screen layout this browser keeps: pane sizes and what is folded away,
// and the one button that puts all of it back (Settings → Trade → Reset layout).
// Per browser only, like the rest of v2's view preferences.

import { useSyncExternalStore } from 'react';
import { readStored, writeStored } from '../../lib/execute';

/** The Trade screen's saved state (`Trade.tsx`). */
export const TRADE_KEY = 'v2.trade';
/** The desktop navigation rail, hidden or shown. */
const NAV_KEY = 'v2.nav';

/**
 * The Trade screen's layout fields: sizes and folded panes. Its picks — the
 * charts and what they show, the index, lots, expiries and chart library —
 * aren't layout, and a reset leaves them alone.
 */
export const TRADE_LAYOUT_FIELDS = [
  'chartHidden',
  'phoneChartHeight',
  'panelOpen',
  'panelHeight',
  'barHidden',
  'ladderHidden',
  'ladderWidth',
  'chartShares',
  'deskWidth',
  'deskHidden',
] as const;

// The rail's state is read where it's toggled (the status bar's mode chip)
// and where it's drawn (the app shell), so it lives here with listeners.
const listeners = new Set<() => void>();
let navHidden = readStored<{ hidden?: boolean }>(NAV_KEY, {}).hidden === true;

function setNav(hidden: boolean) {
  navHidden = hidden;
  writeStored(NAV_KEY, { hidden });
  listeners.forEach((l) => l());
}

/** Whether the desktop navigation rail is hidden, and a toggle for it. */
export function useNavHidden(): [boolean, () => void] {
  const hidden = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => navHidden,
  );
  return [hidden, () => setNav(!navHidden)];
}

/**
 * Every pane back to its first size and shown again, and the navigation rail
 * back. The Trade screen reads its layout when it opens, and it isn't open
 * while Settings is, so the next visit starts from the defaults.
 */
export function resetLayout() {
  const saved = readStored<Record<string, unknown> | null>(TRADE_KEY, null);
  if (saved) {
    const kept = { ...saved };
    for (const field of TRADE_LAYOUT_FIELDS) delete kept[field];
    writeStored(TRADE_KEY, kept);
  }
  setNav(false);
}
