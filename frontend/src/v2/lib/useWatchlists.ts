import { useEffect, useSyncExternalStore } from 'react';
import { useIdentity } from '../auth/identity';
import {
  addWatchlistItem,
  createWatchlist,
  deleteWatchlist,
  getWatchlists,
  patchWatchlist,
  putWatchlistItems,
  removeWatchlistItem,
  type Sent,
} from './api';
import type { Watchlist, WatchlistAnswer, WatchlistsAnswer } from './types';
import { noteServerClock, seedQuotes } from './useQuotes';

// One copy of the signed-in person's watchlists for the whole app (plan
// Phase 2): the Watchlist screen reads it, the "+" on a search result or an
// instrument page adds to it, and every answer the server gives back — the
// whole set on a read, the one list a change was about — lands here, so
// every view agrees without a second request. Prices are not kept here:
// each row's key goes to `useQuotes`, whose 2 s poll the rows tick from,
// and an answer's quotes seed it so a row never waits for the first poll.
// The set itself is re-read every 30 s while the screen is open (a list
// edited on another device, a contract the catalog dropped) and after
// every change.

/** How often the set is re-read while a screen shows it. */
const REFRESH_MS = 30_000;

interface State {
  /** Whose lists these are: the server and the sign-in. */
  scope: string;
  answer: WatchlistsAnswer | null;
  /** The last read's failure, cleared by the next success. */
  error: string | null;
  /** A read is in flight. */
  loading: boolean;
}

let state: State = { scope: '', answer: null, error: null, loading: false };
/** The server the lists live on: `''` is this site (same origin). */
let serverBase = '';
const listeners = new Set<() => void>();
/** The read in flight, and whose it is: a scope that changed meanwhile
 * (another sign-in) drops its answer and reads afresh. */
let inFlight: { scope: string; promise: Promise<void> } | null = null;
let timer: number | null = null;
/** How many mounted hooks want the set polled. */
let pollers = 0;

function set(patch: Partial<State>) {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

function seed(lists: Watchlist[]) {
  seedQuotes(lists.flatMap((w) => w.items.map((i) => i.quote)));
}

/** The set as held right now, for code outside a render. */
export function currentWatchlists(): WatchlistsAnswer | null {
  return state.answer;
}

/** Read the whole set — one request at a time; a second call while one is
 * out gets the same promise. */
export function loadWatchlists(): Promise<void> {
  // No scope: no hook has bound a sign-in yet (or the switch is off).
  if (!state.scope) return Promise.resolve();
  const forScope = state.scope;
  if (inFlight?.scope === forScope) return inFlight.promise;
  set({ loading: true });
  const sentAt = Date.now();
  const promise = getWatchlists(serverBase)
    .then(
      (answer) => {
        if (state.scope !== forScope) return;
        // The rows age their quotes by the server's clock: known before
        // they first draw, not only after the first quotes poll.
        noteServerClock(answer.at, sentAt, Date.now());
        seed(answer.watchlists);
        set({ answer, error: null, loading: false });
      },
      (e: unknown) => {
        if (state.scope !== forScope) return;
        set({ error: e instanceof Error ? e.message : 'Could not reach the server', loading: false });
      },
    )
    .finally(() => {
      if (inFlight?.promise === promise) inFlight = null;
    });
  inFlight = { scope: forScope, promise };
  return promise;
}

/** `one`'s list put in place of the held copy (or added at the end), the
 * rest untouched: right after an item change, a rename or a creation, whose
 * answers carry the whole list. A move or a deletion renumbers the others
 * too, so those re-read the set instead. */
function take(one: WatchlistAnswer) {
  const held = state.answer;
  if (!held) return;
  const lists = held.watchlists.some((w) => w.id === one.watchlist.id)
    ? held.watchlists.map((w) => (w.id === one.watchlist.id ? one.watchlist : w))
    : [...held.watchlists, one.watchlist];
  seed([one.watchlist]);
  set({ answer: { ...held, at: one.at, market_open: one.market_open, watchlists: lists } });
}

function isOne(body: unknown): body is WatchlistAnswer {
  return !!body && typeof body === 'object' && 'watchlist' in body && typeof (body as WatchlistAnswer).watchlist?.id === 'number';
}

/** Send a change; its answer (the one list) replaces the held copy. */
async function change(run: () => Promise<Sent>): Promise<Sent> {
  const sent = await run();
  if (sent.ok && isOne(sent.body)) take(sent.body);
  return sent;
}

/** Send a change after which the whole set is re-read. */
async function changeAll(run: () => Promise<Sent>): Promise<Sent> {
  const sent = await run();
  if (sent.ok) await loadWatchlists();
  return sent;
}

export const watchlistActions = {
  /** A new, empty list named `name` at the end. */
  create: (name: string) => change(() => createWatchlist(serverBase, name)),
  rename: (id: number, name: string) => change(() => patchWatchlist(serverBase, id, { name })),
  /** `id` to `position` among the person's lists (0-based). */
  move: (id: number, position: number) => changeAll(() => patchWatchlist(serverBase, id, { position })),
  remove: (id: number) => changeAll(() => deleteWatchlist(serverBase, id)),
  /**
   * The whole ordered list of keys: a reorder or a removal. Shown at once,
   * so the row lands where it was dropped; the server's answer then stands,
   * and a refusal puts the held copy back.
   */
  setItems: (id: number, keys: string[]) => {
    const held = state.answer;
    if (held) {
      const lists = held.watchlists.map((w) => {
        if (w.id !== id) return w;
        const byKey = new Map(w.items.map((i) => [i.key, i]));
        return { ...w, items: keys.flatMap((k, position) => (byKey.has(k) ? [{ ...byKey.get(k)!, position }] : [])) };
      });
      set({ answer: { ...held, watchlists: lists } });
    }
    return change(() => putWatchlistItems(serverBase, id, keys)).then((sent) => {
      if (!sent.ok && held) set({ answer: held });
      return sent;
    });
  },
  addItem: (id: number, key: string) => change(() => addWatchlistItem(serverBase, id, key)),
  removeItem: (id: number, key: string) => change(() => removeWatchlistItem(serverBase, id, key)),
};

function start() {
  if (timer != null) return;
  timer = window.setInterval(() => {
    if (document.visibilityState === 'visible') void loadWatchlists();
  }, REFRESH_MS);
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
  if (document.visibilityState === 'visible') void loadWatchlists();
}

export interface Watchlists {
  /** The set, once read; `null` before (see `loading` and `error`). */
  answer: WatchlistsAnswer | null;
  /** The lists in order; `[]` before the first answer. */
  lists: Watchlist[];
  /** Why there is no (fresh) answer: the last read's failure, or — before
   * the first read — that the sign-in itself could not be confirmed. */
  error: string | null;
  loading: boolean;
  /** Read the set again now. */
  refresh: () => Promise<void>;
}

/**
 * The signed-in person's watchlists, read once `enabled` (the `watchlists`
 * switch is on) and the server has confirmed who is signed in, and shared
 * by every component that asks. With `poll`, the set is re-read every 30 s
 * while mounted and the tab is visible. A new server or sign-in starts over.
 */
export function useWatchlists(base: string, { enabled = true, poll = false }: { enabled?: boolean; poll?: boolean } = {}): Watchlists {
  // The confirmed sign-in (`/api/auth/me`), not the token's claim: the lists
  // are keyed by its user id, and the scope must not change once read.
  const { user, userError } = useIdentity();
  const scope = enabled && user ? `${base}|${user.id}` : '';
  useEffect(() => {
    if (!scope) return;
    if (state.scope !== scope || serverBase !== base) {
      serverBase = base;
      set({ scope, answer: null, error: null, loading: false });
      // An answer still in flight is the old scope's: `loadWatchlists` drops it.
      void loadWatchlists();
    } else if (!state.answer && inFlight?.scope !== scope) {
      void loadWatchlists();
    }
  }, [base, scope]);
  useEffect(() => {
    if (!scope || !poll) return;
    pollers++;
    start();
    return () => {
      pollers--;
      if (pollers === 0) stop();
    };
  }, [scope, poll]);
  const s = useSyncExternalStore(subscribe, () => state, () => state);
  const current = s.scope === scope ? s : null;
  // No scope because the sign-in's check never got through: the read can't
  // start, and the screen would otherwise say "Loading…" for as long as the
  // server stays away. The check is retried, and a confirmed sign-in reads.
  const unconfirmed = enabled && !user && userError ? `the sign-in could not be confirmed (${userError})` : null;
  return {
    answer: current?.answer ?? null,
    lists: current?.answer?.watchlists ?? [],
    error: current?.error ?? unconfirmed,
    loading: current?.loading ?? false,
    refresh: loadWatchlists,
  };
}
