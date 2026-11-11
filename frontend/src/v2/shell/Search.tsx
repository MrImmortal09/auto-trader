import { useEffect, useId, useRef, useState } from 'react';
import { ChartLine, Plus, Search as SearchIcon, X } from 'lucide-react';
import { chartOf } from '../lib/openInstrument';
import type { InstrumentRow } from '../lib/types';
import { addRecent, clearRecents, readRecents, useSearch } from '../lib/useSearch';
import { useQuotes } from '../lib/useQuotes';
import { Chip } from '../ui/Chip';
import { derivativeNote, groupsOf, optionId, rowSymbol, rowTitle } from './searchModel';
import { AnswerNote, Highlight, KindChips, PriceCell } from './searchParts';

// Spec §13: the global instrument search. A 320 px well in the desktop
// status bar; focus, `/` anywhere outside an input, or ⌘/Ctrl K opens the
// results under it, grouped Indices / Stocks / Futures / Options, each row
// priced and ticking. ↑/↓ move the highlight, Enter opens the instrument
// (an index opens the Trade screen on it), Shift+Enter puts an index on the
// ladder, `+` adds it to a watchlist (`onAdd`, while watchlists are on),
// Escape closes. The well is the combobox, the results its listbox.

/** How a result is opened: Enter / a tap, or Shift+Enter for the ladder. */
export type OpenHow = 'open' | 'ladder';

const PLACEHOLDER = 'Search NIFTY, RELIANCE, BANKNIFTY 52000 CE…';

/** Whether a key press landed somewhere that types. */
function inField(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  const tag = el.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
}

export function Search({ serverBase, onOpen, onAdd }: {
  serverBase: string;
  onOpen: (row: InstrumentRow, how: OpenHow) => void;
  /** Add a result to a watchlist; left out while watchlists are off. */
  onAdd?: (row: InstrumentRow) => void;
}) {
  const [query, setQuery] = useState('');
  const [open, setOpen] = useState(false);
  // The highlighted row's id; nothing highlighted until ↓ (or the first
  // result of a typed query).
  const [active, setActive] = useState<string | null>(null);
  const [recents, setRecents] = useState<InstrumentRow[]>([]);
  const input = useRef<HTMLInputElement>(null);
  const box = useRef<HTMLDivElement>(null);
  const listId = useId();
  const typed = query.trim();
  const search = useSearch(serverBase, query, open);
  const groups = typed ? (search.answer ? groupsOf(search.answer) : []) : recents.length ? [{ head: 'Recent', rows: recents }] : [];
  const flat = groups.flatMap((g) => g.rows);
  const quotes = useQuotes(serverBase, open ? flat.map((r) => r.ws_key) : []);
  const activeRow = flat.find((r) => r.id === active) ?? (typed ? flat[0] : undefined);

  // `/` and ⌘K from anywhere outside a field put the cursor in the well.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const cmdK = (e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === 'k';
      const slash = e.key === '/' && !e.metaKey && !e.ctrlKey && !e.altKey && !inField(e.target);
      if (!cmdK && !slash) return;
      e.preventDefault();
      input.current?.focus();
      input.current?.select();
      setOpen(true);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // A press outside closes the results.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('pointerdown', onDown);
    return () => document.removeEventListener('pointerdown', onDown);
  }, [open]);

  // The highlight follows the keyboard into view.
  useEffect(() => {
    if (!open || !activeRow) return;
    document.getElementById(optionId(listId, activeRow.id))?.scrollIntoView({ block: 'nearest' });
  }, [open, activeRow, listId]);

  function show() {
    setRecents(readRecents());
    setOpen(true);
  }

  // The well lives in the status bar, which stays up across screens: an
  // opened result leaves it empty and unfocused, so the next `/` starts afresh.
  function pick(row: InstrumentRow, how: OpenHow) {
    addRecent(row);
    setRecents(readRecents());
    setOpen(false);
    setActive(null);
    setQuery('');
    input.current?.blur();
    onOpen(row, how);
  }

  function move(step: 1 | -1) {
    if (flat.length === 0) return;
    const at = activeRow ? flat.findIndex((r) => r.id === activeRow.id) : -1;
    const next = at < 0 ? (step > 0 ? 0 : flat.length - 1) : (at + step + flat.length) % flat.length;
    setActive(flat[next].id);
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) show();
      move(e.key === 'ArrowDown' ? 1 : -1);
    } else if (e.key === 'Enter') {
      if (!activeRow) return;
      e.preventDefault();
      pick(activeRow, e.shiftKey ? 'ladder' : 'open');
    } else if (e.key === '+' && onAdd && activeRow && open) {
      e.preventDefault();
      onAdd(activeRow);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      if (open) setOpen(false);
      else setQuery('');
    } else if (e.key === 'Tab') {
      setOpen(false);
    }
  }

  const closedAsOf = flat.map((r) => quotes.get(r.ws_key)?.as_of).find((s) => !!s) ?? null;
  const empty = typed && search.answer && !search.pending && flat.length === 0;

  return (
    <div ref={box} className="relative shrink-0">
      {/* 320 px from 1280 up; at the narrowest desktop the bar's dots and
          controls leave room for a short well (the results are 480 px either way). */}
      <div className="flex h-7 w-[144px] items-center gap-2 rounded-6 border border-line bg-panel px-2.5 transition-colors duration-120 focus-within:border-accent xl:w-[320px]">
        <SearchIcon size={14} className="shrink-0 text-fg-muted" aria-hidden />
        <input
          ref={input}
          type="text"
          role="combobox"
          aria-label="Search instruments"
          aria-expanded={open}
          aria-controls={listId}
          aria-haspopup="listbox"
          aria-autocomplete="list"
          aria-activedescendant={open && activeRow ? optionId(listId, activeRow.id) : undefined}
          autoComplete="off"
          spellCheck={false}
          placeholder={PLACEHOLDER}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(null);
            if (!open) show();
          }}
          onFocus={show}
          onKeyDown={onKeyDown}
          className="min-w-0 flex-1 bg-transparent type-body text-ellipsis text-fg outline-none placeholder:text-fg-subtle"
        />
        {query ? (
          <button
            type="button"
            aria-label="Clear the search"
            onClick={() => {
              setQuery('');
              setActive(null);
              input.current?.focus();
            }}
            className="grid size-5 shrink-0 place-items-center rounded-4 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
          >
            <X size={12} />
          </button>
        ) : (
          <Chip kind="accent" className="px-1">
            /
          </Chip>
        )}
      </div>

      {open && (
        <div className="absolute left-0 top-full z-30 mt-1 w-[480px] rounded-12 border border-line bg-panel p-2 shadow-md animate-fade-in">
          <div id={listId} role="listbox" aria-label="Search results" className="max-h-[60dvh] overflow-y-auto">
            {groups.map((g) => (
              <div key={g.head} role="group" aria-labelledby={`${listId}-${g.head}`}>
                <div className="flex items-center justify-between px-2 pb-1 pt-2">
                  <span id={`${listId}-${g.head}`} className="type-eyebrow text-fg-muted">
                    {g.head}
                  </span>
                  {g.head === 'Recent' && (
                    <button
                      type="button"
                      tabIndex={-1}
                      onClick={() => {
                        clearRecents();
                        setRecents([]);
                      }}
                      className="rounded-4 px-1 type-caption text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
                    >
                      Clear
                    </button>
                  )}
                </div>
                {g.rows.map((row) => {
                  const on = activeRow?.id === row.id;
                  const chart = chartOf(row);
                  const extra = derivativeNote(row);
                  return (
                    <div
                      key={row.id}
                      id={optionId(listId, row.id)}
                      role="option"
                      aria-selected={on}
                      onPointerMove={() => {
                        if (!on) setActive(row.id);
                      }}
                      onClick={() => pick(row, 'open')}
                      title={extra ? row.trading_symbol : undefined}
                      className={`group flex h-10 cursor-pointer items-center gap-2 rounded-6 px-2 ${on ? 'bg-accent-subtle text-accent' : 'text-fg hover:bg-raised/40'}`}
                    >
                      <div className="flex min-w-0 flex-1 items-center gap-2">
                        <span className="shrink-0 whitespace-nowrap type-body-strong">
                          <Highlight text={rowSymbol(row)} fragment={typed} />
                        </span>
                        <KindChips row={row} />
                        {/* A contract's expiry and strike say which one it is; the underlying's name is its symbol's. */}
                        {extra ? (
                          <span className="min-w-0 truncate num-small text-fg-muted">{extra}</span>
                        ) : (
                          <span className="min-w-0 truncate type-caption text-fg-subtle">{row.name}</span>
                        )}
                      </div>
                      <PriceCell quote={quotes.get(row.ws_key)} identity={row.ws_key} stacked={false} muted={on} />
                      {/* Named in full ("RELIANCE NOV FUT"), since a stock and its contracts share a symbol. */}
                      <span className={`flex shrink-0 items-center transition-opacity duration-120 ${on ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'}`}>
                        <button
                          type="button"
                          tabIndex={-1}
                          aria-label={chart ? `Chart ${rowTitle(row)}` : 'Chart'}
                          title={chart ? 'Open the chart' : 'Charts for stocks come in Phase 3'}
                          disabled={!chart}
                          onClick={(e) => {
                            e.stopPropagation();
                            pick(row, 'open');
                          }}
                          className="grid size-7 place-items-center rounded-6 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent"
                        >
                          <ChartLine size={14} />
                        </button>
                        <button
                          type="button"
                          tabIndex={-1}
                          aria-label={`Add ${rowTitle(row)} to a watchlist`}
                          title={onAdd ? 'Add to a watchlist' : 'Watchlists are off'}
                          disabled={!onAdd}
                          onClick={(e) => {
                            e.stopPropagation();
                            onAdd?.(row);
                          }}
                          className="grid size-7 place-items-center rounded-6 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent"
                        >
                          <Plus size={14} />
                        </button>
                      </span>
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
          {!typed && recents.length === 0 && <p className="px-2 py-3 type-body text-fg-subtle">Type a symbol, a company name, or a contract like BANKNIFTY 52000 CE.</p>}
          {empty && <p className="px-2 py-3 type-body text-fg-subtle">No match for '{typed}'</p>}
          {search.error && typed && <p className="px-2 py-2 type-caption text-loss">{search.error}</p>}
          <div className="flex items-center gap-3 px-2 pb-1 pt-2 type-caption text-fg-subtle">
            {typed && search.pending && <span>Searching…</span>}
            {typed && search.answer && <AnswerNote answer={search.answer} closedAsOf={closedAsOf} />}
            <span className="ml-auto flex items-center gap-2 whitespace-nowrap">
              <span className="flex items-center gap-1">
                <Chip kind="accent" className="px-1">
                  ↵
                </Chip>
                open
              </span>
              <span className="flex items-center gap-1">
                <Chip kind="accent" className="px-1">
                  ⇧↵
                </Chip>
                ladder
              </span>
              {onAdd && (
                <span className="flex items-center gap-1">
                  <Chip kind="accent" className="px-1">
                    +
                  </Chip>
                  watchlist
                </span>
              )}
            </span>
          </div>
        </div>
      )}
    </div>
  );
}
