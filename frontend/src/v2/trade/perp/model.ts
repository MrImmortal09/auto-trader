// Hyperliquid perps on the Trade screen: each perp's rules (as the server
// keeps them, `hyperliquid::MARKETS`) and the arithmetic the ticket, the
// panel and the chart lines share.

import type { PerpSide } from '../../lib/types';
import type { PerpKey } from '../model';

export interface PerpRules {
  /** Decimals a size may carry. */
  szDecimals: number;
  /** Hyperliquid's own leverage limit. */
  maxLeverage: number;
  taker: number;
  maker: number;
}

/** Hyperliquid's rules for the three perps; gold (builder-deployed) pays double fees. */
export const PERP_RULES: Record<PerpKey, PerpRules> = {
  BTC: { szDecimals: 5, maxLeverage: 40, taker: 0.00045, maker: 0.00015 },
  ETH: { szDecimals: 4, maxLeverage: 25, taker: 0.00045, maker: 0.00015 },
  GOLD: { szDecimals: 4, maxLeverage: 25, taker: 0.0009, maker: 0.0003 },
};

/** The most leverage the app takes on any perp, whatever Hyperliquid allows. */
export const MAX_LEVERAGE = 20;

export const leverageCap = (k: PerpKey) => Math.min(MAX_LEVERAGE, PERP_RULES[k].maxLeverage);

/** Maintenance margin as a fraction of notional: half the initial margin at max leverage. */
export const maintenance = (k: PerpKey) => 1 / (2 * PERP_RULES[k].maxLeverage);

/** `size` on the perp's size grid, rounded down. */
export function roundSize(k: PerpKey, size: number): number {
  const f = 10 ** PERP_RULES[k].szDecimals;
  return Math.floor(size * f + 1e-9) / f;
}

/** `price` as Hyperliquid takes it: five significant figures and at most
 * `6 - szDecimals` decimals (an integer always passes). */
export function roundPrice(k: PerpKey, price: number): number {
  if (!(Number.isFinite(price) && price > 0)) return price;
  const maxDecimals = 6 - PERP_RULES[k].szDecimals;
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

/** A size in the coin, without trailing zeros: "0.015 BTC". */
export function sizeText(k: PerpKey, size: number): string {
  return `${Number(size.toFixed(PERP_RULES[k].szDecimals))} ${k}`;
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
