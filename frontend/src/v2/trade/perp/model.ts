// Hyperliquid perps on the Trade screen: each perp's rules (the server's
// list, `catalog.ts`) and the arithmetic the ticket, the panel and the chart
// lines share.

import type { PerpSide } from '../../lib/types';
import type { PerpKey } from '../model';
import { perpInfo } from './catalog';

export interface PerpRules {
  /** Decimals a size may carry. */
  szDecimals: number;
  /** Hyperliquid's own leverage limit. */
  maxLeverage: number;
  taker: number;
  maker: number;
  /** The size's unit: `BTC`, `NVDA`. */
  unit: string;
  /** On the list: until it is read (or for a key no longer on it), the
   * stand-in rules below, and nothing should be sent. */
  known: boolean;
  /** Added: takes orders that open. False: close-only. */
  tradable: boolean;
}

/** `k`'s rules, from the server's list; conservative stand-ins (two size
 * decimals, 1x) for a perp not on it. */
export function rulesOf(k: PerpKey): PerpRules {
  const r = perpInfo(k);
  if (!r) return { szDecimals: 2, maxLeverage: 1, taker: 0.00045, maker: 0.00015, unit: k.split(':').pop() ?? k, known: false, tradable: false };
  return { szDecimals: r.sz_decimals, maxLeverage: r.max_leverage, taker: r.taker_fee, maker: r.maker_fee, unit: r.base, known: true, tradable: r.tradable };
}

/** What the screens call `k`: "BTC-PERP", "NVDA-PERP (io)". */
export function perpLabel(k: PerpKey): string {
  return perpInfo(k)?.label ?? `${k.split(':').pop() ?? k}-PERP`;
}

/** The most leverage the app takes on any perp, whatever Hyperliquid allows. */
export const MAX_LEVERAGE = 20;

export const leverageCap = (k: PerpKey) => Math.min(MAX_LEVERAGE, rulesOf(k).maxLeverage);

/** Maintenance margin as a fraction of notional: half the initial margin at max leverage. */
export const maintenance = (k: PerpKey) => 1 / (2 * rulesOf(k).maxLeverage);

/** `size` on the perp's size grid, rounded down. */
export function roundSize(k: PerpKey, size: number): number {
  const f = 10 ** rulesOf(k).szDecimals;
  return Math.floor(size * f + 1e-9) / f;
}

/** `price` as Hyperliquid takes it: five significant figures and at most
 * `6 - szDecimals` decimals (an integer always passes). */
export function roundPrice(k: PerpKey, price: number): number {
  if (!(Number.isFinite(price) && price > 0)) return price;
  const maxDecimals = 6 - rulesOf(k).szDecimals;
  const magnitude = Math.floor(Math.log10(price)) + 1;
  const decimals = Math.max(0, Math.min(5 - magnitude, maxDecimals));
  if (decimals === 0) return Math.round(price);
  const f = 10 ** decimals;
  return Math.round(price * f) / f;
}

/** Where a new position at `entry` with `leverage` would be liquidated;
 * null when no price can (a long at 1x). */
export function estLiq(k: PerpKey, side: PerpSide, entry: number, leverage: number): number | null {
  const mm = maintenance(k);
  const perUnit = entry / leverage;
  const liq = side === 'long' ? (entry - perUnit) / (1 - mm) : (entry + perUnit) / (1 + mm);
  return Number.isFinite(liq) && liq > 0 ? liq : null;
}

/** P&L of `size` held `side` from `entry`, at `price`. */
export function pnlAt(side: PerpSide, size: number, entry: number, price: number): number {
  return (side === 'long' ? 1 : -1) * size * (price - entry);
}

const USD = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** "$1,234.56". */
export const usd = (n: number) => USD.format(n);
/** "+$12.30" / "−$4.10". */
export const signedUsd = (n: number) => `${n >= 0 ? '+' : '−'}${USD.format(Math.abs(n))}`;

/** A perp price: as many decimals as it carries, grouped. */
export function perpPrice(n: number): string {
  return n.toLocaleString('en-US', { maximumFractionDigits: 5 });
}

/** A size in the coin, without trailing zeros: "0.015 BTC", "2.5 NVDA". */
export function sizeText(k: PerpKey, size: number): string {
  const r = rulesOf(k);
  return `${Number(size.toFixed(r.szDecimals))} ${r.unit}`;
}

/** How long a line order stays armed, as the ticket offers it. */
export const VALIDITIES: { value: number | null; label: string }[] = [
  { value: 30, label: '30 min' },
  { value: 60, label: '1 h' },
  { value: 240, label: '4 h' },
  { value: 1440, label: '24 h' },
  { value: null, label: 'Until cancelled' },
];

/** "Long" or "Short". */
export const sideWord = (s: PerpSide) => (s === 'long' ? 'Long' : 'Short');
