// ---------------------------------------------------------------------------
// Execute screen helpers — formatting, per-viewer preferences, buy guards.
// ---------------------------------------------------------------------------

import type { ExecuteFrame } from '../types';

/** Index tabs — weekly-expiry indices first; keys 1–6 follow this order. */
export const INDEX_ORDER = ['NIFTY', 'BANKNIFTY', 'SENSEX', 'FINNIFTY', 'MIDCPNIFTY', 'BANKEX'];

/** Most lots one tap may buy; the server enforces the same cap. */
export const MAX_LOTS = 10;
/** Strikes shown each side of the ladder's centre. */
export const VISIBLE_HALF = 5;
/** A feed tick older than this and the screen won't send a buy (server: 5 s). */
export const FEED_STALE_MS = 5_000;
/** Above this the feed badge turns amber. */
export const FEED_SLOW_MS = 2_000;
/** A strike whose own price is older than this can't be bought (server: 10 s). */
export const CONTRACT_STALE_MS = 10_000;

export function contractLabel(index: string, strike: number | null | undefined, optionType: string | null | undefined) {
  return [index, strike != null ? fmtStrike(strike) : null, optionType].filter(Boolean).join(' ');
}

export function fmtStrike(n: number) {
  return Number.isInteger(n) ? String(n) : n.toFixed(2);
}

export function fmtLtp(n: number | null | undefined) {
  return n == null ? '—' : n.toFixed(2);
}

const inr = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 2, minimumFractionDigits: 2 });
const inr0 = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 0 });

/** `+₹617.50` / `−₹157.00`. */
export function fmtPnl(n: number) {
  return `${n >= 0 ? '+' : '−'}₹${inr.format(Math.abs(n))}`;
}

export function fmtSpot(n: number) {
  return inr.format(n);
}

export function fmtRupees(n: number) {
  return `₹${inr0.format(n)}`;
}

/** "Tue 20 Oct" from `YYYY-MM-DD`, read as a calendar date (no timezone shift). */
export function expiryLabel(iso: string) {
  const [y, m, d] = iso.split('-').map(Number);
  const date = new Date(Date.UTC(y, m - 1, d));
  return date.toLocaleDateString('en-IN', { weekday: 'short', day: 'numeric', month: 'short', timeZone: 'UTC' });
}

export function daysLeftLabel(days: number) {
  if (days <= 0) return 'expires today';
  return days === 1 ? '1 day left' : `${days} days left`;
}

/** Minutes past midnight IST for a Unix-ms instant. */
export function istMinutes(ms: number) {
  const ist = new Date(ms + 330 * 60_000);
  return ist.getUTCHours() * 60 + ist.getUTCMinutes();
}

/** After 15:20 IST an open NRML position is heading into the overnight carry. */
export function nearClose(ms: number) {
  return istMinutes(ms) >= 15 * 60 + 20;
}

/** Why a buy can't be sent right now, or null when it can. */
export function buyBlockedReason(frame: ExecuteFrame | null, streamStale: boolean): string | null {
  if (!frame || streamStale) return 'Not connected to the server';
  if (frame.kill_switch) return 'Kill switch is active';
  if (!frame.market_open) return 'Market is closed (09:15–15:30 IST)';
  const age = frame.feed.last_tick_age_ms;
  if (!frame.feed.connected || age == null || age > FEED_STALE_MS) return 'Price feed is stale';
  if (frame.mode === 'LIVE' && !frame.kotak_ready) return 'Kotak is not connected';
  return null;
}

/** localStorage read, tolerating private windows / blocked storage. */
export function readStored<T>(key: string, fallback: T): T {
  try {
    const raw = window.localStorage.getItem(key);
    return raw == null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

export function writeStored(key: string, value: unknown) {
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Per-viewer convenience only — nothing depends on it being saved.
  }
}
