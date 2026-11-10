import { useEffect, useRef, useState } from 'react';
import { readStored, writeStored } from '../../lib/execute';
import { searchInstruments } from './api';
import type { InstrumentRow, SearchAnswer, SearchRow } from './types';
import { seedQuotes } from './useQuotes';

/** Typing pauses this long before a query goes out. */
const DEBOUNCE_MS = 150;
/** An open query is asked again this often, so prices fill in and tick. */
const REPOLL_MS = 2000;
/** Where the last searches opened are kept, newest first. */
const RECENTS_KEY = 'v2.recentSearches';
const MAX_RECENTS = 5;

export interface Search {
  /** The answer for `query` — the last one while a newer query is pending. */
  answer: SearchAnswer | null;
  /** No answer yet for the query as typed. */
  pending: boolean;
  error: string | null;
}

/**
 * The search for `query`: debounced 150 ms, then asked again every 2 s while
 * `enabled` so the rows the hub is still fetching get their prices (every
 * row's key is marked wanted by the answer itself). A slow answer never
 * overwrites a newer query's. An empty query is no search.
 */
export function useSearch(serverBase: string, query: string, enabled: boolean): Search {
  const q = query.trim();
  const [state, setState] = useState<{ query: string; answer: SearchAnswer | null; error: string | null }>({ query: '', answer: null, error: null });
  const seq = useRef(0);

  useEffect(() => {
    if (!enabled || !q) return;
    const counter = seq;
    const mine = ++counter.current;
    let timer: number | null = null;
    const run = async () => {
      try {
        const answer = await searchInstruments(serverBase, q);
        if (mine !== counter.current) return;
        seedQuotes([...answer.indices, ...answer.stocks, ...answer.futures, ...answer.options].map((r) => r.quote));
        setState({ query: q, answer, error: null });
      } catch (e) {
        if (mine !== counter.current) return;
        setState((prev) => ({ query: q, answer: prev.query === q ? prev.answer : null, error: e instanceof Error ? e.message : 'Could not reach the server' }));
      }
    };
    const debounce = window.setTimeout(() => {
      void run();
      timer = window.setInterval(() => {
        if (document.visibilityState === 'visible') void run();
      }, REPOLL_MS);
    }, DEBOUNCE_MS);
    return () => {
      // Answers still in flight are for the old query: drop them.
      counter.current++;
      window.clearTimeout(debounce);
      if (timer != null) window.clearInterval(timer);
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
