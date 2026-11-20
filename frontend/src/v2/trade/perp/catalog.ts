// The Hyperliquid perps on the app's list (`GET /api/v2/hl/catalog`), kept
// once for the whole page: the Trade screen, the chart pickers and the
// searches read the same list. Until it is read, the three the app started
// with stand in, so a saved BTC chart or ticket works from the first frame.

import { useEffect, useSyncExternalStore } from 'react';
import { getHlCatalog } from '../../lib/api';
import type { HlPerpRules } from '../../lib/types';

const builtin = (key: string, coin: string, dex: string | null, venue: string, name: string, sz: number, lev: number, taker: number): HlPerpRules => ({
  key,
  symbol: `HL:${key}`,
  coin,
  base: key,
  dex,
  venue,
  label: `${key}-PERP`,
  name,
  sz_decimals: sz,
  max_leverage: lev,
  leverage_cap: lev,
  taker_fee: taker,
  maker_fee: taker / 3,
  tradable: true,
  header: true,
});

/** The server's three built in (`hyperliquid::markets::BUILTIN`). */
const BUILTIN: HlPerpRules[] = [
  builtin('BTC', 'BTC', null, 'Hyperliquid', 'Bitcoin', 5, 40, 0.00045),
  builtin('ETH', 'ETH', null, 'Hyperliquid', 'Ether', 4, 25, 0.00045),
  builtin('GOLD', 'xyz:GOLD', 'xyz', 'trade.xyz', 'Gold (trade.xyz)', 4, 25, 0.0009),
];

export interface PerpCatalog {
  /** The list's revision; null until it is read. */
  rev: number | null;
  /** Every perp on the list, in the order added; close-only ones too. */
  list: HlPerpRules[];
  byKey: Map<string, HlPerpRules>;
}

function make(rev: number | null, list: HlPerpRules[]): PerpCatalog {
  return { rev, list, byKey: new Map(list.map((r) => [r.key.toLowerCase(), r])) };
}

let catalog = make(null, BUILTIN);
const listeners = new Set<() => void>();

function subscribe(fn: () => void) {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

export function setPerpCatalog(rev: number, list: HlPerpRules[]) {
  catalog = make(rev, list);
  listeners.forEach((fn) => fn());
}

/** `key`'s rules (any case), if it is on the list. */
export function perpInfo(key: string): HlPerpRules | undefined {
  return catalog.byKey.get(key.toLowerCase());
}

/** The list, re-rendering whoever reads it when it changes. */
export function usePerpCatalog(): PerpCatalog {
  return useSyncExternalStore(subscribe, () => catalog);
}

let loading: Promise<void> | null = null;

/**
 * Read the list while `enabled`: once, and again whenever `rev` (the markets
 * poll's) differs from the one read. Readers mounted together share one read.
 */
export function useLoadPerpCatalog(serverBase: string, enabled: boolean, rev?: number | null): PerpCatalog {
  const c = usePerpCatalog();
  const stale = enabled && (c.rev == null || (rev != null && rev !== c.rev));
  useEffect(() => {
    if (!stale || loading) return;
    loading = getHlCatalog(serverBase)
      .then((a) => setPerpCatalog(a.rev, a.markets))
      .catch(() => {})
      .finally(() => {
        loading = null;
      });
  }, [serverBase, stale, rev]);
  return c;
}

/** A key a saved layout may name: Hyperliquid's coins are letters and digits, a builder's prefixed `dex:`. */
export function isPerpKey(raw: unknown): raw is string {
  return typeof raw === 'string' && /^[A-Za-z0-9]{1,24}(:[A-Za-z0-9]{1,24})?$/.test(raw);
}

/**
 * The added perps matching `query`: by name without the dex (`NVDA`), the
 * coin (`xyz:NVDA`) or the label; whole names first, then names starting
 * with it, then ones containing it, each core perps first.
 */
export function searchPerps(list: HlPerpRules[], query: string, limit = 8): HlPerpRules[] {
  const q = query.trim().toLowerCase().replace(/-perp$/, '');
  if (!q) return [];
  const rank = (r: HlPerpRules): number | null => {
    const names = [r.base, r.coin, r.key, r.label, r.name].map((s) => s.toLowerCase());
    if (names.some((n) => n === q)) return 0;
    if (names.some((n) => n.startsWith(q))) return 1;
    if (names.some((n) => n.includes(q))) return 2;
    return null;
  };
  return list
    .filter((r) => r.tradable)
    .map((r, i) => ({ r, i, rank: rank(r) }))
    .filter((x): x is { r: HlPerpRules; i: number; rank: number } => x.rank != null)
    .sort((a, b) => a.rank - b.rank || Number(a.r.dex != null) - Number(b.r.dex != null) || a.i - b.i)
    .slice(0, limit)
    .map((x) => x.r);
}
