import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';
import { ChevronDown, LayoutGrid, Star, X } from 'lucide-react';
import { MINUS, price, signedPct, toneOf } from '../lib/format';
import type { Watchlist } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { Popover } from '../ui/Popover';
import { Segment } from '../ui/Segment';
import { HEADER_MODES, type HeaderMode, type HeaderTile } from './headerModel';

// The Trade screen's header: the six index tabs (and the stock tab beside
// them, behind the `stock_charts` switch), or — the owner's "both ways" for
// the plan's question 7 — the person's watchlist rows in their place. One
// tile is one `HeaderTile`, whatever it names: the screen builds the list,
// this file draws it on the desktop's slim bar and as the phone's tabs;
// `headerModel.ts` has the shapes and the indices' tiles.

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** The day's move in points: "−43.95". From the previous close, or worked
 * back from the percent when the server sends none (an older one). */
function signedPoints(spot: number, prevClose: number | null, changePct: number): string {
  const d = prevClose != null && prevClose > 0 ? spot - prevClose : spot - spot / (1 + changePct / 100);
  const v = Math.abs(d) < 0.005 ? 0 : d;
  return `${v > 0 ? '+' : v < 0 ? MINUS : ''}${price(Math.abs(v))}`;
}

/** How long a finger rests on a tile before it means the ladder. */
const LONG_PRESS_MS = 500;

/**
 * A press that tells a tap from a long one: `onLong` fires once the pointer
 * has rested `LONG_PRESS_MS`, and the click that follows the release is
 * swallowed; a shorter press is an ordinary click. Nothing fires when the
 * tile has no long action.
 */
function useLongPress(onLong: (() => void) | undefined) {
  const timer = useRef<number | null>(null);
  const fired = useRef(false);
  const clear = () => {
    if (timer.current != null) window.clearTimeout(timer.current);
    timer.current = null;
  };
  useEffect(() => clear, []);
  return {
    onPointerDown: (e: ReactPointerEvent) => {
      fired.current = false;
      if (!onLong || e.pointerType === 'mouse') return;
      clear();
      timer.current = window.setTimeout(() => {
        timer.current = null;
        fired.current = true;
        onLong();
      }, LONG_PRESS_MS);
    },
    onPointerUp: clear,
    onPointerLeave: clear,
    onPointerCancel: clear,
    onContextMenu: (e: ReactPointerEvent | React.MouseEvent) => {
      if (onLong) e.preventDefault();
    },
    /** Whether the click after a release is the long press's, to be dropped. */
    swallow: () => {
      const was = fired.current;
      fired.current = false;
      return was;
    },
  };
}

/** Figma "Index tab": the name, its value (flashing its tint as it ticks)
 * and the day's change. A white tile; the selected one is outlined and
 * tinted in accent. A long press takes the tile's other action (the ladder).
 * The stock tab ends in an × column, 44 px wide and the tile's height, to
 * take it off. */
function Tile({ tile }: { tile: HeaderTile }) {
  const flash = useTickFlash(tile.spot, true, tile.id);
  const press = useLongPress(tile.onAlt);
  return (
    <span className={`flex shrink-0 items-stretch overflow-hidden rounded-8 border transition-colors duration-120 ${tile.on ? 'border-accent bg-accent-subtle' : 'border-line bg-panel hover:bg-raised/40'}`}>
      <button
        type="button"
        role="tab"
        aria-selected={tile.on}
        title={tile.title}
        onPointerDown={press.onPointerDown}
        onPointerUp={press.onPointerUp}
        onPointerLeave={press.onPointerLeave}
        onPointerCancel={press.onPointerCancel}
        onContextMenu={press.onContextMenu}
        onClick={(e) => {
          if (press.swallow()) return;
          if (e.shiftKey && tile.onAlt) tile.onAlt();
          else tile.onSelect();
        }}
        className={`flex min-w-[124px] flex-col py-2 pl-3 text-left ${tile.onRemove ? 'pr-1' : 'pr-3'}`}
      >
        <span className="truncate type-caption text-fg-muted">{tile.label}</span>
        <span className="flex items-baseline gap-1.5">
          <span key={flash.key} className={`-mx-1 rounded-4 px-1 num-medium text-fg ${flash.className}`}>
            {tile.spot == null ? '—' : price(tile.spot)}
          </span>
          {tile.changePct != null && <span className={`num-small ${TONE_TEXT[toneOf(tile.changePct)]}`}>{signedPct(tile.changePct)}</span>}
        </span>
      </button>
      {tile.onRemove && (
        <button
          type="button"
          aria-label={`Take ${tile.label} off the tabs`}
          title={`Take ${tile.label} off the tabs`}
          onClick={tile.onRemove}
          className="grid w-11 shrink-0 place-items-center text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
        >
          <X size={14} />
        </button>
      )}
    </span>
  );
}

/** Scroll `el` sideways so its selected tab is in view — this box only:
 * scrollIntoView could move the page around it too. */
function keepSelectedInView(el: HTMLElement) {
  const tab = el.querySelector<HTMLElement>('[aria-selected="true"]');
  if (!tab) return;
  const item = tab.parentElement === el ? tab : (tab.parentElement ?? tab);
  const left = item.offsetLeft;
  const right = left + item.offsetWidth;
  if (left < el.scrollLeft) el.scrollLeft = left;
  else if (right > el.scrollLeft + el.clientWidth) el.scrollLeft = right - el.clientWidth;
}

/** On a new pick, and whenever the tabs change width (their prices arriving
 * can push the selected one out of view), keep the selected tab in view. */
function useKeepInView(box: React.RefObject<HTMLDivElement | null>, selected: string | null, count: number) {
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const keep = () => keepSelectedInView(el);
    keep();
    const ro = new ResizeObserver(keep);
    for (const tab of el.children) ro.observe(tab);
    return () => ro.disconnect();
  }, [box, selected, count]);
}

/** The phone's strip of tiles, scrolled to keep the selected one in view. */
export function HeaderTabs({ tiles, label }: { tiles: HeaderTile[]; label: string }) {
  const box = useRef<HTMLDivElement>(null);
  useKeepInView(box, tiles.find((t) => t.on)?.id ?? null, tiles.length);
  return (
    <div ref={box} role="tablist" aria-label={label} className="relative flex min-w-0 flex-1 gap-2 overflow-x-auto">
      {tiles.map((t) => (
        <Tile key={t.id} tile={t} />
      ))}
    </div>
  );
}

/** One tile on the desktop ticker: "NIFTY 24,986.40 −43.95 (−0.18%)", or
 * "−0.18%" alone where room is short (see `HeaderTile.points`), the value
 * flashing as it ticks; the selected one is underlined in accent. The stock
 * tab carries an × to take it off. */
function TickerItem({ tile }: { tile: HeaderTile }) {
  const flash = useTickFlash(tile.spot, true, tile.id);
  // All six with points need about 98rem; the three with points, the rest
  // with the percent, about 70rem (a 1440 px window with the nav rail).
  const points = tile.points === 'always' ? 'hidden @[70rem]:inline' : 'hidden @[98rem]:inline';
  return (
    <span className={`relative flex shrink-0 items-stretch ${tile.on ? 'after:absolute after:inset-x-3 after:bottom-0 after:h-0.5 after:rounded-full after:bg-accent' : ''}`}>
      <button
        type="button"
        role="tab"
        aria-selected={tile.on}
        title={tile.title}
        onClick={(e) => (e.shiftKey && tile.onAlt ? tile.onAlt() : tile.onSelect())}
        className={`flex items-center gap-1.5 whitespace-nowrap pl-3 transition-colors duration-120 hover:bg-raised/40 ${tile.onRemove ? 'pr-1' : 'pr-3'}`}
      >
        <span className={`type-label ${tile.on ? 'text-fg' : 'text-fg-muted'}`}>{tile.label}</span>
        <span key={flash.key} className={`-mx-0.5 rounded-4 px-0.5 num-small text-fg ${flash.className}`}>
          {tile.spot == null ? '—' : price(tile.spot)}
        </span>
        {tile.spot != null && tile.changePct != null && (
          <span className={`num-small ${TONE_TEXT[toneOf(tile.changePct)]}`}>
            <span className={points}>{signedPoints(tile.spot, tile.prevClose, tile.changePct)} (</span>
            {signedPct(tile.changePct)}
            <span className={points}>)</span>
          </span>
        )}
      </button>
      {tile.onRemove && (
        <button
          type="button"
          aria-label={`Take ${tile.label} off the bar`}
          title={`Take ${tile.label} off the bar`}
          onClick={tile.onRemove}
          className="grid w-6 shrink-0 place-items-center self-center rounded-4 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
        >
          <X size={12} />
        </button>
      )}
    </span>
  );
}

/** The desktop bar: every tile on one slim line, scrolling sideways when
 * the window is too narrow for all of them — with the mouse wheel too, and
 * always to keep the selected one in view. */
export function HeaderTicker({ tiles, label }: { tiles: HeaderTile[]; label: string }) {
  const box = useRef<HTMLDivElement>(null);
  useKeepInView(box, tiles.find((t) => t.on)?.id ?? null, tiles.length);
  return (
    <div
      ref={box}
      role="tablist"
      aria-label={label}
      onWheel={(e) => {
        if (e.deltaY !== 0 && e.deltaX === 0) e.currentTarget.scrollLeft += e.deltaY;
      }}
      className="@container relative flex min-w-0 flex-1 items-stretch overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
    >
      {tiles.map((t) => (
        <TickerItem key={t.id} tile={t} />
      ))}
      {tiles.length === 0 && <span className="flex items-center px-3 type-caption text-fg-subtle">Nothing on this watchlist yet — add instruments from a search or an instrument page.</span>}
    </div>
  );
}

/** Which of the person's lists the header shows: its name, and a menu of all of them. */
export function ListPicker({ lists, selected, onSelect, compact }: { lists: Watchlist[]; selected: number | null; onSelect: (id: number) => void; compact: boolean }) {
  const [open, setOpen] = useState(false);
  const current = lists.find((l) => l.id === selected) ?? lists[0];
  return (
    <div className="relative shrink-0">
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="Which watchlist"
        disabled={lists.length === 0}
        onClick={() => setOpen((o) => !o)}
        className={`flex items-center gap-1 whitespace-nowrap rounded-6 text-fg transition-colors duration-120 hover:bg-raised/40 disabled:opacity-50 ${compact ? 'h-11 px-2' : 'h-7 px-2'}`}
      >
        <Star size={compact ? 16 : 14} className="text-fg-muted" aria-hidden />
        <span className="max-w-[140px] truncate type-label">{current?.name ?? 'Watchlist'}</span>
        <ChevronDown size={14} className="text-fg-muted" />
      </button>
      <Popover open={open} onClose={() => setOpen(false)} backdrop={compact}>
        <div className="px-2 pt-1 pb-1.5 type-caption text-fg-muted">Watchlist</div>
        <div role="listbox" aria-label="Watchlist">
          {lists.map((l) => {
            const on = current?.id === l.id;
            return (
              <button
                key={l.id}
                type="button"
                role="option"
                aria-selected={on}
                onClick={() => {
                  onSelect(l.id);
                  setOpen(false);
                }}
                className={`flex h-10 w-full items-center gap-2 rounded-6 px-3 text-left type-label ${on ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}
              >
                <span className="min-w-0 flex-1 truncate">{l.name}</span>
                <span className="type-caption tnum">{l.items.length}</span>
              </button>
            );
          })}
        </div>
      </Popover>
    </div>
  );
}

/** The switch between the indices and a watchlist in the header: a Segment
 * on the desktop bar, a 44 px icon button on the phone's strip. */
export function HeaderSwitch({ mode, onChange, compact }: { mode: HeaderMode; onChange: (mode: HeaderMode) => void; compact: boolean }) {
  if (!compact) return <Segment label="Header" size="xs" value={mode} onChange={onChange} options={HEADER_MODES} />;
  const toWatchlist = mode === 'indices';
  const words = toWatchlist ? 'Show the watchlist here' : 'Show the indices here';
  return (
    <button
      type="button"
      aria-label={words}
      title={words}
      aria-pressed={!toWatchlist}
      onClick={() => onChange(toWatchlist ? 'watchlist' : 'indices')}
      className={`grid size-11 shrink-0 place-items-center rounded-8 border transition-colors duration-120 ${toWatchlist ? 'border-line bg-panel text-fg-muted hover:bg-raised/40' : 'border-accent bg-accent-subtle text-accent'}`}
    >
      {toWatchlist ? <Star size={18} /> : <LayoutGrid size={18} />}
    </button>
  );
}
