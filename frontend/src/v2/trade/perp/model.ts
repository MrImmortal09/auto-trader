// Hyperliquid perps on the Trade screen: each perp's rules (the server's
// list, `catalog.ts`) and the arithmetic the ticket, the panel and the chart
// lines share.

import type { PerpSide, PerpToday } from '../../lib/types';
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

/** The most leverage the app takes on `k`: Hyperliquid's own limit (40x on BTC). */
export const leverageCap = (k: PerpKey) => rulesOf(k).maxLeverage;
/** The ticket's leverage chips, below a perp's own limit (its "Max" chip). */
export const LEVERAGE_PRESETS = [1, 2, 5, 10, 20];

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

/** What today's figures leave out because Hyperliquid didn't answer it. */
export function unreadNote(t: PerpToday): string | null {
  return t.unread?.includes('funding') ? "Today's funding couldn't be read from Hyperliquid — the charges and P&L leave it out." : null;
}

/** The least order Hyperliquid takes, in USD: only one closing all of a position may be smaller. */
export const MIN_ORDER_USD = 10;
/** A target fires as a market order: a long's may fill as far as 10 % under its price. */
export const TRIGGER_SLIP = 0.1;
/** The share a new target sells when its prompt opens (the owner's pick). */
export const TARGET_PCT_DEFAULT = 25;

/** What a target for `pct` % of a `side` position of `held` sells at `price`. */
export interface TargetShare {
  /** What is sent: `null` sells all that is left when it is reached. */
  size: number | null;
  /** What it sells as things stand. */
  sells: number;
  /** Why it can't be placed, if it can't. */
  why: string | null;
  /** Which way it is wrong: worth too little, or more than the other targets leave. */
  problem: 'small' | 'over' | null;
  /** The smallest whole % that works at `price`, when `pct` is too small. */
  minPct: number | null;
}

/**
 * A target selling `pct` % of a `side` position of `held` on `k`, at
 * `price`, as the server checks it: the size on the perp's grid, rounded
 * down (never more than asked); one for part of it worth at least
 * {@link MIN_ORDER_USD} where it may fill (a long's {@link TRIGGER_SLIP}
 * under its price); with `others` (paper: what the other sized targets
 * sell) no more than is held. 100 % is "all left", always taken.
 */
export function targetShare(k: PerpKey, side: PerpSide, held: number, others: number, pct: number, price: number): TargetShare {
  const left = Math.max(0, held - others);
  if (pct >= 100) return { size: null, sells: left, why: null, problem: null, minPct: null };
  const sizeAt = (p: number) => roundSize(k, (held * p) / 100);
  const worst = side === 'long' ? price * (1 - TRIGGER_SLIP) : price;
  const fits = (s: number) => s > 0 && s <= left + 1e-12 && s * worst + 1e-9 >= MIN_ORDER_USD;
  const size = sizeAt(pct);
  if (size <= 0) {
    const minPct = smallest(pct, sizeAt, fits);
    return { size, sells: 0, why: `${pct}% of ${sizeText(k, held)} rounds to nothing. ${minPct != null ? `Pick at least ${minPct}%` : 'Pick 100%'}`, problem: 'small', minPct };
  }
  if (size > left + 1e-12) {
    const room = Math.floor((left / held) * 100);
    return { size, sells: size, why: `Only ${sizeText(k, left)} (${room}%) isn't on another target`, problem: 'over', minPct: null };
  }
  if (!fits(size)) {
    const minPct = smallest(pct, sizeAt, fits);
    const at = side === 'long' ? ' at its worst fill, 10% under the target' : '';
    return { size, sells: size, why: `${sizeText(k, size)} is ${usd(size * worst)}${at}: Hyperliquid takes nothing under ${usd(MIN_ORDER_USD)} but a close of all of it. ${minPct != null ? `Pick at least ${minPct}%` : 'Pick 100%'}`, problem: 'small', minPct };
  }
  return { size, sells: size, why: null, problem: null, minPct: null };
}

/** The smallest whole % above `pct`, under 100, whose size `fits`. */
function smallest(pct: number, sizeAt: (p: number) => number, fits: (s: number) => boolean): number | null {
  for (let p = Math.floor(pct) + 1; p < 100; p++) if (fits(sizeAt(p))) return p;
  return null;
}
