import { useEffect, useRef, useState } from 'react';
import { readStored, writeStored } from '../../lib/execute';
import { searchInstruments } from './api';
import type { InstrumentRow, SearchAnswer, SearchRow } from './types';
import { seedQuotes } from './useQuotes';

/** Typing pauses this long before a query goes out. */
const DEBOUNCE_MS = 150;
/** An open query whose options still wait for the spot (`awaitsSpot`), or
 * whose ask failed, is asked again after this long… */
const REASK_MS = 2000;
/** …at most this many asks in all, so a spot that never comes (or a server
 * that stays down) doesn't keep the search going while the query is open. */
const MAX_ASKS = 10;
/** Where the last searches opened are kept, newest first. */
const RECENTS_KEY = 'v2.recentSearches';
const MAX_RECENTS = 5;

export interface Search {
  /** The answer for `query` — the last one while a newer query is pending,
   * so the list doesn't blink as a query grows; never one from before the
   * query was emptied. */
  answer: SearchAnswer | null;
  /** No answer yet for the query as typed: `answer` (if any) is for an
   * earlier one, so nothing should act on its rows by default. */
  pending: boolean;
  error: string | null;
}

/**
 * Whether `answer` listed its options before the server had the
 * underlying's price. A plain query's options are the strikes around that
 * spot, and the spot's row in the answer has no quote yet: once the quotes
 * poll has fetched it, the same query asked again lists the strikes around
 * it. Rows' own prices never need the search asked again: every row on
 * screen is in the quotes poll (`useQuotes`), which fills them in and ticks
 * them. An underlying not among the rows is never fetched by the search, so
 * asking again would not bring its spot either.
 */
export function awaitsSpot(answer: SearchAnswer): boolean {
  if (answer.parsed !== 'plain' || answer.options.length === 0) return false;
  const underlying = answer.options[0].underlying;
  const spot = [...answer.indices, ...answer.stocks].find((r) => r.symbol === underlying);
  return !!spot && spot.quote == null;
}

/**
 * The search for `query`: debounced 150 ms, then, while `enabled`, asked
 * again every 2 s only while its options wait for the spot (`awaitsSpot`) or
 * the ask failed, ten asks at most; the quotes poll keeps the prices. A slow
 * answer never overwrites a newer query's. An empty query is no search, and
 * forgets the last answer.
 */
export function useSearch(serverBase: string, query: string, enabled: boolean): Search {
  const q = query.trim();
  const [state, setState] = useState<{ query: string; answer: SearchAnswer | null; error: string | null }>({ query: '', answer: null, error: null });
  const seq = useRef(0);

  // Emptied: the last answer goes, so the next query starts from no rows
  // rather than from the rows of what was typed before (where Enter or a
  // stray tap would open the old instrument).
  if (!q && (state.query || state.answer || state.error)) setState({ query: '', answer: null, error: null });

  useEffect(() => {
    if (!enabled || !q) return;
    const counter = seq;
    const mine = ++counter.current;
    let timer: number | null = null;
    let asks = 0;
    const again = () => {
      if (asks < MAX_ASKS) timer = window.setTimeout(() => void run(), REASK_MS);
    };
    const run = async () => {
      timer = null;
      // A hidden tab asks nothing, and tries again later.
      if (document.visibilityState !== 'visible') return again();
      asks++;
      try {
        const answer = await searchInstruments(serverBase, q);
        if (mine !== counter.current) return;
        seedQuotes([...answer.indices, ...answer.stocks, ...answer.futures, ...answer.options].map((r) => r.quote));
        setState({ query: q, answer, error: null });
        if (awaitsSpot(answer)) again();
      } catch (e) {
        if (mine !== counter.current) return;
        setState((prev) => ({ query: q, answer: prev.query === q ? prev.answer : null, error: e instanceof Error ? e.message : 'Could not reach the server' }));
        again();
      }
    };
    timer = window.setTimeout(() => void run(), DEBOUNCE_MS);
    return () => {
      // Answers still in flight are for the old query: drop them.
      counter.current++;
      if (timer != null) window.clearTimeout(timer);
    };
  }, [serverBase, q, enabled]);

  if (!q) return { answer: null, pending: false, error: null };
  // The last rows stay up while a newer query is pending, so the list doesn't
  // blink on every keystroke; "Searching…" says a newer answer is coming.
  const current = state.query === q;
  return { answer: state.answer, pending: !current, error: current ? state.error : null };
}

/** The rows of an answer in reading order: indices, stocks, futures, options. */
export function rowsOf(answer: SearchAnswer | null): SearchRow[] {
  return answer ? [...answer.indices, ...answer.stocks, ...answer.futures, ...answer.options] : [];
}

/** The last instruments opened from a search (up to five, newest first),
 * kept in this browser. */
export function readRecents(): InstrumentRow[] {
  const raw = readStored<unknown>(RECENTS_KEY, []);
  if (!Array.isArray(raw)) return [];
  return raw.filter((r): r is InstrumentRow => !!r && typeof r === 'object' && typeof (r as InstrumentRow).id === 'string' && typeof (r as InstrumentRow).symbol === 'string').slice(0, MAX_RECENTS);
}

/** Note that `row` was opened: it goes to the top of the recents. */
export function addRecent(row: InstrumentRow) {
  const { id, kind, symbol, name, exchange, trading_symbol, underlying, expiry, strike, lot_size, tick_size, ws_key, has_fo } = row;
  const kept: InstrumentRow = { id, kind, symbol, name, exchange, trading_symbol, underlying, expiry, strike, lot_size, tick_size, ws_key, has_fo, tier: null };
  writeStored(RECENTS_KEY, [kept, ...readRecents().filter((r) => r.id !== id)].slice(0, MAX_RECENTS));
}

export function clearRecents() {
  writeStored(RECENTS_KEY, []);
}
