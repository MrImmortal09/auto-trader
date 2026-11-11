import { useState } from 'react';
import { ChevronLeft, ChevronRight, Ellipsis, Pencil, Plus, Trash2 } from 'lucide-react';
import type { Watchlist } from '../lib/types';
import { Button } from '../ui/Button';
import { Popover } from '../ui/Popover';
import { Segment } from '../ui/Segment';

// The lists as a Segment — up to five, the open one the white pill — with a
// "+" for a new list and a menu on the open one: rename, move left or
// right among the lists, delete (`ListMenu`). The dialogs the menu opens
// are the screen's (`dialogs.tsx`); this only says what was asked. On
// desktop the buttons sit beside the Segment; on a phone the Segment has
// the whole width (scrolling edge to edge when five long names need it)
// and the buttons go on the line under it, with the count.

/** What the open list's menu can ask for. */
export type ListAction = 'rename' | 'left' | 'right' | 'delete';

const ITEM = 'flex h-9 w-full items-center gap-2.5 rounded-6 px-2 text-left type-body transition-colors duration-120 hover:bg-raised disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent';

export function ListTabs({ lists, selected, desktop, onSelect }: {
  lists: Watchlist[];
  /** The open list's id. */
  selected: number;
  desktop: boolean;
  onSelect: (id: number) => void;
}) {
  return (
    <div className={desktop ? 'min-w-0' : '-mx-4 min-w-0 overflow-x-auto px-4 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden'}>
      <Segment label="Watchlists" size={desktop ? 'sm' : 'lg'} value={String(selected)} onChange={(id) => onSelect(Number(id))} options={lists.map((l) => ({ value: String(l.id), label: l.name }))} />
    </div>
  );
}

/** "+" for a new list, and the open list's menu. */
export function ListMenu({ lists, selected, maxLists, desktop, onNew, onAction }: {
  lists: Watchlist[];
  selected: number;
  maxLists: number;
  desktop: boolean;
  onNew: () => void;
  onAction: (action: ListAction) => void;
}) {
  const [menu, setMenu] = useState(false);
  const at = lists.findIndex((l) => l.id === selected);
  const list = lists[at];
  const full = lists.length >= maxLists;
  const size = desktop ? 'sm' : 'lg';
  const icon = desktop ? 14 : 18;
  return (
    <div className="flex shrink-0 items-center gap-2">
      <Button
        size={size}
        kind="quiet"
        icon={<Plus size={icon} />}
        aria-label="New watchlist"
        title={full ? `You already have ${maxLists} watchlists — delete one first` : 'New watchlist'}
        disabled={full}
        onClick={onNew}
        className={desktop ? 'px-2' : 'px-3'}
      />
      <span className="relative shrink-0">
        <Button
          size={size}
          kind="quiet"
          icon={<Ellipsis size={icon} />}
          aria-label={list ? `${list.name}: rename, move or delete` : 'Watchlist menu'}
          aria-expanded={menu}
          aria-haspopup="menu"
          disabled={!list}
          onClick={() => setMenu((o) => !o)}
          className={desktop ? 'px-2' : 'px-3'}
        />
        <Popover open={menu} onClose={() => setMenu(false)} align="right" backdrop={!desktop}>
          <div role="menu" aria-label={list ? `${list.name}` : 'Watchlist'} className="flex flex-col">
            <button type="button" role="menuitem" className={ITEM} onClick={() => { setMenu(false); onAction('rename'); }}>
              <Pencil size={14} className="text-fg-muted" /> Rename…
            </button>
            <button type="button" role="menuitem" className={ITEM} disabled={at <= 0} onClick={() => { setMenu(false); onAction('left'); }}>
              <ChevronLeft size={14} className="text-fg-muted" /> Move left
            </button>
            <button type="button" role="menuitem" className={ITEM} disabled={at < 0 || at >= lists.length - 1} onClick={() => { setMenu(false); onAction('right'); }}>
              <ChevronRight size={14} className="text-fg-muted" /> Move right
            </button>
            <div aria-hidden className="my-1 h-px bg-line" />
            <button
              type="button"
              role="menuitem"
              className={`${ITEM} text-loss hover:bg-loss-subtle`}
              disabled={lists.length <= 1}
              title={lists.length <= 1 ? 'Keep at least one watchlist — rename or empty this one instead' : undefined}
              onClick={() => { setMenu(false); onAction('delete'); }}
            >
              <Trash2 size={14} /> Delete…
            </button>
          </div>
        </Popover>
      </span>
    </div>
  );
}
