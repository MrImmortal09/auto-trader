// The Trade header's shapes: what one tile names, and the six indices as
// tiles. Kept apart from the components so Fast Refresh keeps working.

import type { IndexHeadline } from '../../types';
import { INDEX_ORDER } from '../../lib/execute';

/** Which rows the header shows: the indices (plus the stock tab) or a watchlist. */
export type HeaderMode = 'indices' | 'watchlist';

export const HEADER_MODES: { value: HeaderMode; label: string }[] = [
  { value: 'indices', label: 'Indices' },
  { value: 'watchlist', label: 'Watchlist' },
];

/** One tile of the header: an index, the stock, or a watchlist row. */
export interface HeaderTile {
  id: string;
  /** "NIFTY", "RELIANCE", "RELIANCE 1400 CE". */
  label: string;
  spot: number | null;
  /** The previous session's close, for the day's points. */
  prevClose: number | null;
  changePct: number | null;
  /** Selected: outlined on the phone, underlined on the desktop. */
  on: boolean;
  /** A tap: the ladder for an index, the chart for a watchlist row. */
  onSelect: () => void;
  /** Shift+click, or a long press on the phone: the ladder, where a row has one. */
  onAlt?: () => void;
  /** The stock tab's ×. */
  onRemove?: () => void;
  title?: string;
  /** The desktop ticker shows the day's points beside the percent whenever
   * the bar has room (`always`: the three big indices), or only on a wide
   * window (`wide`). */
  points: 'always' | 'wide';
}

/** The indices the ticker gives the day's points as well as the percent
 * whenever the bar has room for them; the others only on a wide window. */
const TICKER_POINTS = new Set(['NIFTY', 'BANKNIFTY', 'SENSEX']);

/** The six indices as tiles, from the stream's headlines. */
export function indexTiles(indices: IndexHeadline[], selected: string, onSelect: (index: string) => void): HeaderTile[] {
  const by = new Map(indices.map((i) => [i.index, i]));
  return INDEX_ORDER.map((index) => {
    const i = by.get(index);
    return {
      id: index,
      label: index,
      spot: i?.spot ?? null,
      prevClose: i?.prev_close ?? null,
      changePct: i?.change_pct ?? null,
      on: index === selected,
      onSelect: () => onSelect(index),
      points: TICKER_POINTS.has(index) ? 'always' : 'wide',
    };
  });
}

