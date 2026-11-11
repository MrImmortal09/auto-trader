import { useState } from 'react';
import { ArrowDown, ArrowUp, ArrowUpToLine, ChartLine, Ellipsis, Rows3, Trash2 } from 'lucide-react';
import { price, signedPct, signedPrice, toneOf } from '../lib/format';
import type { Quote, WatchlistItem } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { derivativeNote, rowSymbol, rowTitle } from '../shell/searchModel';
import { KindChips } from '../shell/searchParts';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { Popover } from '../ui/Popover';

// One instrument on a watchlist. Desktop: a 44 px hairline row — the symbol
// with its exchange and kind chips, a contract's expiry and strike or the
// name, the LTP and the day's change (flashing as they tick), and on hover
// Chart, Chain and Buy with a menu for moving and removing. Phone: a 56 px
// row (symbol and chips, then the name; the price stacked at the right) that
// opens the instrument, and a "⋯" that opens a sheet with the same actions.
// Nothing here places an order: Buy opens the ladder on the Trade screen.

/** A live price older than this reads as stale during hours. */
const STALE_MS = 30_000;

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;

const COMING = {
  chart: 'Charts for stocks come in Phase 3',
  chain: 'Option chains come in Phase 3',
  buy: 'Stock options on the ladder come in Phase 4',
} as const;

/** Where a row's actions go; `null` means "coming" (disabled, with the reason). */
export interface RowActions {
  /** The row itself: the instrument page, or the Trade screen for an index. */
  onOpen: () => void;
  onChart: (() => void) | null;
  onBuy: (() => void) | null;
  onMove: (to: 'top' | 'up' | 'down') => void;
  onRemove: () => void;
}

/** The change against the previous close, from the quote or worked out. */
function changeOf(q: Quote): { points: number | null; pct: number | null } {
  const points = q.change ?? (q.close != null ? q.ltp - q.close : null);
  const pct = q.per_change ?? (q.close ? ((q.ltp - q.close) / q.close) * 100 : null);
  return { points, pct };
}

/** The LTP flashing its tint as it ticks, grey when nothing has ticked for
 * a while during hours; the change toned under or beside it. */
function Figures({ quote, identity, staleSecs, stacked, size }: {
  quote: Quote | null;
  identity: string;
  /** How long nothing has ticked, when that is long enough to grey the price. */
  staleSecs: number | null;
  stacked: boolean;
  size: 'medium' | 'large';
}) {
  const flash = useTickFlash(quote?.ltp, true, identity);
  const num = size === 'large' ? 'num-large' : 'num-medium';
  // In a row the two sit in the head's columns: LTP 96 px, change 120 px.
  const ltpBox = stacked ? '' : 'w-[96px] text-right';
  const changeBox = stacked ? '' : 'w-[120px] text-right';
  if (!quote) {
    return (
      <span className={`flex ${stacked ? 'flex-col items-end' : 'items-baseline justify-end gap-3'}`}>
        <span className={`${ltpBox} ${num} text-fg-subtle`}>—</span>
        <span className={`${changeBox} num-small text-fg-subtle`}>{stacked ? 'waiting for a price' : 'waiting'}</span>
      </span>
    );
  }
  const { points, pct } = changeOf(quote);
  const tone = TONE_TEXT[toneOf(points)];
  return (
    <span className={`flex ${stacked ? 'flex-col items-end' : 'items-baseline justify-end gap-3'}`}>
      <span className={ltpBox}>
        <span key={flash.key} title={staleSecs != null ? `No tick for ${staleSecs} s` : undefined} className={`-mx-1 inline-block rounded-4 px-1 ${num} ${staleSecs != null ? 'text-fg-subtle' : 'text-fg'} ${flash.className}`}>
          {price(quote.ltp)}
        </span>
      </span>
      <span className={`${changeBox} num-small ${tone}`}>
        {points != null ? signedPrice(points) : '—'}
        {pct != null && <span className="ml-1.5">{signedPct(pct)}</span>}
      </span>
    </span>
  );
}

const MENU_ITEM = 'flex h-9 w-full items-center gap-2.5 rounded-6 px-2 text-left type-body transition-colors duration-120 hover:bg-raised disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent';
const ICON_BUTTON = 'grid size-7 place-items-center rounded-6 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-fg-muted';

/** The desktop column heads, over the rows. */
export function RowHead() {
  return (
    <div className="flex h-9 items-center gap-3 border-b border-line type-eyebrow text-fg-muted">
      <span className="min-w-0 flex-1">Instrument</span>
      <span className="w-[96px] text-right">LTP</span>
      <span className="w-[120px] text-right">Change</span>
      <span className="w-[150px]" aria-hidden />
    </div>
  );
}

export function Row({ item, quote, desktop, first, last, marketOpen, now, actions }: {
  item: WatchlistItem;
  quote: Quote | null;
  desktop: boolean;
  first: boolean;
  last: boolean;
  /** Whether a live price can be stale (after hours the close is the price). */
  marketOpen: boolean;
  /** Unix ms, ticking every few seconds, to age the price against. */
  now: number;
  actions: RowActions;
}) {
  const [menu, setMenu] = useState(false);
  const [sheet, setSheet] = useState(false);
  const row = item.instrument;
  const title = row ? rowTitle(row) : item.key;
  const note = row ? derivativeNote(row) : null;
  const staleSecs = marketOpen && quote && quote.source !== 'close' && now - quote.at > STALE_MS ? Math.round((now - quote.at) / 1000) : null;
  const identity = item.key;

  const name = row ? (
    <span className="flex min-w-0 items-center gap-2">
      <span className="shrink-0 whitespace-nowrap type-body-strong text-fg">{rowSymbol(row)}</span>
      <KindChips row={row} />
      {note && <span className="shrink-0 whitespace-nowrap num-small text-fg-muted">{note}</span>}
    </span>
  ) : (
    <span className="flex min-w-0 items-center gap-2">
      <span className="truncate type-body-strong text-fg">{item.key}</span>
    </span>
  );
  const caption = row ? row.name : 'No longer listed — an expired contract, or the catalog is not built yet';

  if (desktop) {
    return (
      <div className={`group flex h-11 items-stretch gap-3 border-line transition-colors duration-120 hover:bg-raised/40 ${last ? '' : 'border-b'}`}>
        <button type="button" onClick={actions.onOpen} disabled={!row} title={row ? `Open ${title}` : undefined} className="flex min-w-0 flex-1 items-center gap-3 text-left disabled:cursor-default">
          {/* Room for "BANKNIFTY · NSE · CE · 24 Nov · 57,900". */}
          <span className="w-[280px] shrink-0">{name}</span>
          <span className={`min-w-0 flex-1 truncate type-caption ${row ? 'text-fg-subtle' : 'text-loss'}`}>{caption}</span>
        </button>
        <span className="flex w-[228px] shrink-0 items-center justify-end">
          <Figures quote={quote} identity={identity} staleSecs={staleSecs} stacked={false} size="medium" />
        </span>
        <span className="flex w-[150px] shrink-0 items-center justify-end gap-1">
          {/* The actions show on hover and on keyboard focus; the menu stays put. */}
          <span className="flex items-center gap-1 opacity-0 transition-opacity duration-120 group-focus-within:opacity-100 group-hover:opacity-100">
            <button type="button" aria-label={`Chart ${title}`} title={actions.onChart ? 'Open the chart' : COMING.chart} disabled={!actions.onChart} onClick={actions.onChart ?? undefined} className={ICON_BUTTON}>
              <ChartLine size={14} />
            </button>
            <button type="button" aria-label={`Option chain for ${title}`} title={COMING.chain} disabled className={ICON_BUTTON}>
              <Rows3 size={14} />
            </button>
            <Button size="sm" kind="profit" disabled={!actions.onBuy} title={actions.onBuy ? 'Open the ladder' : COMING.buy} onClick={actions.onBuy ?? undefined} className="w-12">
              Buy
            </Button>
          </span>
          <span className="relative">
            <button type="button" aria-label={`${title}: move or remove`} aria-expanded={menu} aria-haspopup="menu" onClick={() => setMenu((o) => !o)} className={ICON_BUTTON}>
              <Ellipsis size={14} />
            </button>
            <Popover open={menu} onClose={() => setMenu(false)} align="right">
              <div role="menu" aria-label={title} className="flex flex-col">
                <button type="button" role="menuitem" className={MENU_ITEM} disabled={first} onClick={() => { setMenu(false); actions.onMove('top'); }}>
                  <ArrowUpToLine size={14} className="text-fg-muted" /> Move to top
                </button>
                <button type="button" role="menuitem" className={MENU_ITEM} disabled={first} onClick={() => { setMenu(false); actions.onMove('up'); }}>
                  <ArrowUp size={14} className="text-fg-muted" /> Move up
                </button>
                <button type="button" role="menuitem" className={MENU_ITEM} disabled={last} onClick={() => { setMenu(false); actions.onMove('down'); }}>
                  <ArrowDown size={14} className="text-fg-muted" /> Move down
                </button>
                <div aria-hidden className="my-1 h-px bg-line" />
                <button type="button" role="menuitem" className={`${MENU_ITEM} text-loss hover:bg-loss-subtle`} onClick={() => { setMenu(false); actions.onRemove(); }}>
                  <Trash2 size={14} /> Remove
                </button>
              </div>
            </Popover>
          </span>
        </span>
      </div>
    );
  }

  return (
    <div className={`flex h-14 items-stretch border-line ${last ? '' : 'border-b'}`}>
      <button type="button" onClick={actions.onOpen} disabled={!row} className="flex min-w-0 flex-1 items-center gap-3 pl-4 text-left transition-colors duration-120 active:bg-raised/40 disabled:cursor-default">
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          {name}
          <span className={`truncate type-caption ${row ? 'text-fg-subtle' : 'text-loss'}`}>{caption}</span>
        </span>
        <Figures quote={quote} identity={identity} staleSecs={staleSecs} stacked size="medium" />
      </button>
      <button type="button" aria-label={`${title}: actions`} aria-haspopup="dialog" onClick={() => setSheet(true)} className="grid w-11 shrink-0 place-items-center text-fg-muted transition-colors duration-120 active:bg-raised/40">
        <Ellipsis size={18} />
      </button>
      {sheet && (
        <Modal open onClose={() => setSheet(false)} label={title}>
          <div className="flex flex-col gap-4 p-6">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <h2 className="type-title text-fg">{title}</h2>
                <p className={`mt-0.5 truncate type-caption ${row ? 'text-fg-subtle' : 'text-loss'}`}>{caption}</p>
              </div>
              <Figures quote={quote} identity={`${identity}|sheet`} staleSecs={staleSecs} stacked size="large" />
            </div>
            <div className="grid grid-cols-3 gap-2">
              <Button size="lg" icon={<ChartLine size={18} />} disabled={!actions.onChart} title={actions.onChart ? undefined : COMING.chart} onClick={() => { setSheet(false); actions.onChart?.(); }}>
                Chart
              </Button>
              <Button size="lg" icon={<Rows3 size={18} />} disabled title={COMING.chain}>
                Chain
              </Button>
              <Button size="lg" kind="profit" disabled={!actions.onBuy} title={actions.onBuy ? undefined : COMING.buy} onClick={() => { setSheet(false); actions.onBuy?.(); }}>
                Buy
              </Button>
            </div>
            <div className="flex flex-col gap-2">
              <Button size="lg" icon={<ArrowUp size={18} />} disabled={first} onClick={() => { setSheet(false); actions.onMove('up'); }}>
                Move up
              </Button>
              <Button size="lg" icon={<ArrowDown size={18} />} disabled={last} onClick={() => { setSheet(false); actions.onMove('down'); }}>
                Move down
              </Button>
              <Button size="lg" kind="loss-outline" icon={<Trash2 size={18} />} onClick={() => { setSheet(false); actions.onRemove(); }}>
                Remove from the list
              </Button>
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}
