import { useEffect, useSyncExternalStore } from 'react';
import { getQuotes } from './api';
import type { Quote } from './types';

// One poll of `/api/v2/quotes` for every key on screen — search rows, the
// instrument page, later the watchlist — every 2 s while the tab is
// visible, so a price never needs more than one request a tick however many
// rows show it. Each consumer asks for its keys with `useQuote`; the store
// keeps the newest quote per key and tells every subscriber when anything
// changed. A search answer seeds the store with the quotes it carries, so a
// row's tick flash compares against the same baseline the poll continues.

/** How often the wanted keys are asked for. */
const EVERY_MS = 2000;
/** Keys one `/api/v2/quotes` call may ask for (the server's cap). */
const MAX_KEYS = 60;

const quotes = new Map<string, Quote>();
/** How many consumers want each key right now. */
const wanted = new Map<string, number>();
const listeners = new Set<() => void>();
let version = 0;
let serverBase = '';
let timer: number | null = null;
/** A poll promised for the next tick of the event loop: new keys wanted. */
let soon: number | null = null;
let inFlight = false;
/** Unix ms of the last answer, and whether the market was open then. */
let at: number | null = null;
let marketOpen: boolean | null = null;

function notify() {
  version++;
  for (const l of listeners) l();
}

/** Keep `incoming` where it is newer than what is held. */
export function seedQuotes(incoming: (Quote | null | undefined)[]) {
  let changed = false;
  for (const q of incoming) {
    if (!q) continue;
    const held = quotes.get(q.key);
    if (!held || q.at >= held.at) {
      quotes.set(q.key, q);
      changed = true;
    }
  }
  if (changed) notify();
}

async function poll() {
  if (inFlight || !serverBase || document.visibilityState !== 'visible') return;
  const keys = [...wanted.keys()];
  if (keys.length === 0) return;
  inFlight = true;
  try {
    for (let i = 0; i < keys.length; i += MAX_KEYS) {
      const answer = await getQuotes(serverBase, keys.slice(i, i + MAX_KEYS));
      at = answer.at;
      marketOpen = answer.market_open;
      seedQuotes(answer.quotes);
    }
  } catch {
    // The next tick tries again; the rows keep their last price.
  } finally {
    inFlight = false;
  }
}

function pollSoon() {
  if (soon != null) return;
  soon = window.setTimeout(() => {
    soon = null;
    void poll();
  }, 0);
}

function start() {
  if (timer != null) return;
  timer = window.setInterval(() => void poll(), EVERY_MS);
  document.addEventListener('visibilitychange', onVisible);
}

function stop() {
  if (timer == null) return;
  window.clearInterval(timer);
  timer = null;
  document.removeEventListener('visibilitychange', onVisible);
}

/** Shown again after being hidden: catch up at once. */
function onVisible() {
  if (document.visibilityState === 'visible') pollSoon();
}

function want(keys: string[]) {
  let fresh = false;
  for (const key of keys) {
    const n = wanted.get(key) ?? 0;
    wanted.set(key, n + 1);
    if (n === 0) fresh = true;
  }
  if (wanted.size > 0) start();
  if (fresh) pollSoon();
}

function drop(keys: string[]) {
  for (const key of keys) {
    const n = wanted.get(key) ?? 0;
    if (n <= 1) wanted.delete(key);
    else wanted.set(key, n - 1);
  }
  if (wanted.size === 0) stop();
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

/** The quotes held: `get(key)` the newest, `at` when the last answer came,
 * `marketOpen` what it said. */
export interface Quotes {
  get: (key: string | null | undefined) => Quote | null;
  at: number | null;
  marketOpen: boolean | null;
}

/**
 * The quotes for `keys`, polled every 2 s while any component wants them
 * (one request for every key on screen). Re-renders when any quote changes.
 * A new `serverBase` starts over.
 */
export function useQuotes(base: string, keys: (string | null | undefined)[]): Quotes {
  const asked = keys.filter((k): k is string => typeof k === 'string' && k.length > 0);
  // Keyed by content, so a re-render with the same keys asks for nothing new.
  const signature = asked.join(',');
  useEffect(() => {
    if (serverBase !== base) {
      serverBase = base;
      quotes.clear();
      at = null;
      marketOpen = null;
      notify();
    }
    const mine = signature ? signature.split(',') : [];
    want(mine);
    return () => drop(mine);
  }, [base, signature]);
  useSyncExternalStore(subscribe, () => version, () => version);
  return { get: (key) => (key ? quotes.get(key) ?? null : null), at, marketOpen };
}

/** One key's quote, polled with everything else on screen. */
export function useQuote(base: string, key: string | null | undefined): Quote | null {
  return useQuotes(base, [key]).get(key);
}
