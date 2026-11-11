import { useEffect, useId, useRef, useState } from 'react';
import { Plus, Search as SearchIcon, X } from 'lucide-react';
import type { InstrumentRow } from '../lib/types';
import { addRecent, clearRecents, readRecents, useSearch } from '../lib/useSearch';
import { useQuotes } from '../lib/useQuotes';
import { Button } from '../ui/Button';
import { defaultRow, derivativeNote, groupsOf, rowSymbol, rowTitle } from './searchModel';
import { AnswerNote, Highlight, KindChips, PriceCell } from './searchParts';
import type { OpenHow } from './Search';

// Spec §13, phone: the Search tab opens this full-screen sheet — a 48 px
// well with the keyboard up, Cancel, and the same groups as the desktop
// popover in 56 px rows (symbol and chips on the first line, name and the
// price on the second). A tap opens the instrument; the "+" at the end of a
// row adds it to a watchlist (`onAdd`, while watchlists are on); the
// keyboard's search key, once the answer is for what was typed, opens what
// it names: the typed contract or future (nothing when it matched none), else
// the first row.

const PLACEHOLDER = 'Search NIFTY, RELIANCE, BANKNIFTY 52000 CE…';

export function SearchSheet({ serverBase, open, onClose, onOpen, onAdd }: {
  serverBase: string;
  open: boolean;
  onClose: () => void;
  onOpen: (row: InstrumentRow, how: OpenHow) => void;
  /** Add a result to a watchlist; left out while watchlists are off. */
  onAdd?: (row: InstrumentRow) => void;
}) {
  const [query, setQuery] = useState('');
  // Bumped when the recents are cleared, so the list re-reads them.
  const [recentsSeq, setRecentsSeq] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const listId = useId();
  const typed = query.trim();
  const search = useSearch(serverBase, query, open);
  // Read when drawn: five rows at most, and the sheet opens afresh each time.
  const recents: InstrumentRow[] = open && recentsSeq >= 0 ? readRecents() : [];
  const groups = typed ? (search.answer ? groupsOf(search.answer) : []) : recents.length ? [{ head: 'Recent', rows: recents }] : [];
  const flat = groups.flatMap((g) => g.rows);
  const quotes = useQuotes(serverBase, open ? flat.map((r) => r.ws_key) : []);
  // The caller's latest `onClose`, for the Escape listener: a new function
  // on every render of the caller must not re-run the effect below, which
  // would pull the keyboard back up on each status poll.
  const closeRef = useRef(onClose);
  useEffect(() => {
    closeRef.current = onClose;
  });

  // Opened: the keyboard comes up on the well, once.
  useEffect(() => {
    if (!open) return;
    input.current?.focus();
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && closeRef.current();
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open]);

  if (!open) return null;

  function pick(row: InstrumentRow) {
    addRecent(row);
    setQuery('');
    onOpen(row, 'open');
  }

  const closedAsOf = flat.map((r) => quotes.get(r.ws_key)?.as_of).find((s) => !!s) ?? null;
  const empty = typed && search.answer && !search.pending && flat.length === 0;

  return (
    <div role="dialog" aria-modal="true" aria-label="Search" className="fixed inset-0 z-40 flex flex-col bg-panel pb-safe pt-safe animate-fade-in">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2">
        <div className="flex h-12 min-w-0 flex-1 items-center gap-2.5 rounded-8 border border-line bg-inset px-3 transition-colors duration-120 focus-within:border-accent">
          <SearchIcon size={18} className="shrink-0 text-fg-muted" aria-hidden />
          <input
            ref={input}
            type="search"
            role="combobox"
            aria-label="Search instruments"
            aria-expanded
            aria-controls={listId}
            aria-haspopup="listbox"
            aria-autocomplete="list"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="characters"
            spellCheck={false}
            enterKeyHint="search"
            placeholder={PLACEHOLDER}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              // What the answer to what was typed names (`defaultRow`): the
              // typed contract or future, else its first row; never a row of
              // an earlier query's answer still on show while this one is
              // pending, nor a recent when nothing is typed.
              if (e.key !== 'Enter' || !typed || search.pending || !search.answer) return;
              const first = defaultRow(search.answer);
              if (first) pick(first);
            }}
            className="min-w-0 flex-1 bg-transparent type-body text-ellipsis text-fg outline-none placeholder:text-fg-subtle [&::-webkit-search-cancel-button]:hidden"
          />
          {query && (
            <button
              type="button"
              aria-label="Clear the search"
              onClick={() => {
                setQuery('');
                input.current?.focus();
              }}
              className="grid size-8 shrink-0 place-items-center rounded-6 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
            >
              <X size={16} />
            </button>
          )}
        </div>
        <Button size="lg" kind="quiet" onClick={() => { setQuery(''); onClose(); }} className="-mr-2">
          Cancel
        </Button>
      </div>

      <div id={listId} role="listbox" aria-label="Search results" className="min-h-0 flex-1 overflow-y-auto px-2 py-1">
        {groups.map((g) => (
          <div key={g.head} role="group" aria-labelledby={`${listId}-${g.head}`}>
            <div className="flex items-center justify-between px-2 pb-1 pt-3">
              <span id={`${listId}-${g.head}`} className="type-eyebrow text-fg-muted">
                {g.head}
              </span>
              {g.head === 'Recent' && (
                <button
                  type="button"
                  onClick={() => {
                    clearRecents();
                    setRecentsSeq((n) => n + 1);
                  }}
                  className="-my-1 rounded-6 px-2 py-1 type-label text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
                >
                  Clear
                </button>
              )}
            </div>
            {g.rows.map((row) => (
              <div key={row.id} className="flex items-stretch">
                <button
                  type="button"
                  role="option"
                  aria-selected={false}
                  onClick={() => pick(row)}
                  title={row.expiry ? row.trading_symbol : undefined}
                  className="grid h-14 min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] grid-rows-2 items-center gap-x-3 rounded-8 px-2 text-left transition-colors duration-120 active:bg-raised/40"
                >
                  <span className="flex min-w-0 items-center gap-2 self-end">
                    <span className="shrink-0 whitespace-nowrap type-body-strong text-fg">
                      <Highlight text={rowSymbol(row)} fragment={typed} />
                    </span>
                    <KindChips row={row} />
                    {derivativeNote(row) && <span className="shrink-0 whitespace-nowrap num-small text-fg-muted">{derivativeNote(row)}</span>}
                  </span>
                  <span aria-hidden className="row-start-1 col-start-2" />
                  <span className="min-w-0 truncate type-caption text-fg-subtle">{row.name}</span>
                  <span className="col-start-2 row-start-2 flex items-center">
                    <PriceCell quote={quotes.get(row.ws_key)} identity={row.ws_key} stacked={false} />
                  </span>
                </button>
                {onAdd && (
                  <button
                    type="button"
                    aria-label={`Add ${rowTitle(row)} to a watchlist`}
                    onClick={() => onAdd(row)}
                    className="grid w-11 shrink-0 place-items-center rounded-8 text-fg-muted transition-colors duration-120 active:bg-raised/40"
                  >
                    <Plus size={18} />
                  </button>
                )}
              </div>
            ))}
          </div>
        ))}
        {!typed && recents.length === 0 && <p className="px-2 py-4 type-body text-fg-subtle">Type a symbol, a company name, or a contract like BANKNIFTY 52000 CE.</p>}
        {empty && <p className="px-2 py-4 type-body text-fg-subtle">No match for '{typed}'</p>}
        {search.error && typed && <p className="px-2 py-2 type-caption text-loss">{search.error}</p>}
        {typed && (search.pending || search.answer) && (
          <div className="flex flex-wrap items-center gap-3 px-2 py-3 type-caption text-fg-subtle empty:hidden">
            {search.pending && <span>Searching…</span>}
            {search.answer && <AnswerNote answer={search.answer} closedAsOf={closedAsOf} />}
          </div>
        )}
      </div>
    </div>
  );
}
