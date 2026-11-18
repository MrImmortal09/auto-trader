import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import type { ExecuteBoard, ExecutePosition, ExecuteQuote } from '../../types';
import { buyMaxAgeMs, expiryLabel, fmtAgeLimit } from '../../lib/execute';
import { level, price, signedMoney } from '../lib/format';
import type { ChainAnswer } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { Button } from '../ui/Button';
import { LotsStepper } from '../ui/LotsStepper';
import { boardKind, fmtAge, polledMarker, quotePolled } from './model';

/** Why one side of a row can't be bought now, or null. */
function quoteBlocked(q: ExecuteQuote | null, blocked: string | null, maxAgeMs: number, ladder: 'index' | 'stock'): string | null {
  if (blocked) return blocked;
  if (!q || q.ltp == null) return 'No price for this contract yet';
  // The hub's REST quote stands in for a strike past the feed budget: good to
  // look at, but no buy route accepts a price the feed never carried. An
  // index ladder keeps its wording from before stocks joined, so index
  // trading reads exactly as it did; a stock's row says it the short way.
  if (quotePolled(q)) return ladder === 'stock' ? 'No live tick for this strike' : 'Polled price — this strike is past the feed budget, so it can’t be bought yet';
  if (q.age_ms != null && q.age_ms > maxAgeMs) return `This contract's price hasn't updated in ${fmtAgeLimit(maxAgeMs)}`;
  return null;
}

/** How long the ladder rests after a scroll before asking for prices there. */
const SCROLL_SETTLE_MS = 400;

interface LadderRow {
  strike: number;
  ce: ExecuteQuote | null;
  pe: ExecuteQuote | null;
}

/**
 * Every strike of the chain, with live prices from the board's subscribed
 * window merged in by strike. Before the chain answers, the board's own
 * rows stand in.
 */
function ladderRows(chain: ChainAnswer | undefined, board: ExecuteBoard): LadderRow[] {
  const quotes = new Map(board.rows.map((r) => [r.strike, r]));
  if (!chain || chain.expiry !== board.expiry) return board.rows;
  return chain.strikes.map((s) => {
    const q = quotes.get(s.strike);
    return {
      strike: s.strike,
      ce: q?.ce ?? (s.ce ? { sym: s.ce.symbol, ltp: null, age_ms: null } : null),
      pe: q?.pe ?? (s.pe ? { sym: s.pe.symbol, ltp: null, age_ms: null } : null),
    };
  });
}

/**
 * A side's price, tapped to chart the contract, and under it, on a contract
 * held, the quantity and its P&L. The price flashes its tint as it ticks —
 * only near ATM and on held strikes: a 40-row chain at 09:15 would strobe.
 */
function Quote({ q, kind, label, position, flashes, marketOpen, onChart }: {
  q: ExecuteQuote | null;
  kind: 'CE' | 'PE';
  label: string;
  position: ExecutePosition | undefined;
  flashes: boolean;
  /** Closed, a polled price is the last session's close (`polledMarker`). */
  marketOpen: boolean;
  onChart: () => void;
}) {
  const flash = useTickFlash(q?.ltp, flashes, q?.sym);
  const polled = q != null && quotePolled(q);
  // A price past the feed budget is the quote hub's, read every ~2 s; the
  // marker says so, and how long ago it was read once that is worth seeing.
  const title = !polled
    ? `Chart ${label}`
    : marketOpen
      ? `Chart ${label} · polled: Kotak's REST quote, read ${fmtAge(q.age_ms) || 'just now'} ago (this strike is past the feed budget)`
      : `Chart ${label} · polled: Kotak's REST quote at the last session's close (this strike is past the feed budget)`;
  return (
    <button type="button" disabled={!q} onClick={onChart} title={title} className={`flex min-w-0 flex-1 flex-col justify-center ${kind === 'CE' ? 'items-end text-right' : 'items-start text-left'}`}>
      <span key={flash.key} className={`-mx-1 rounded-4 px-1 num-medium ${q?.ltp == null ? 'text-fg-muted' : 'text-fg'} ${flash.className}`}>
        {q?.ltp == null ? '—' : price(q.ltp)}
      </span>
      {/* One line each, cut with an ellipsis when the column is narrow: the
       * row's height is fixed, so a wrapped second line spilled into the
       * next row. The button's title carries the whole of it. */}
      {position ? (
        <span className={`max-w-full truncate type-caption tnum ${position.pnl != null && position.pnl < 0 ? 'text-loss' : 'text-profit'}`}>
          {position.qty} · {position.pnl == null ? '—' : signedMoney(position.pnl, 0)}
        </span>
      ) : (
        polled && <span className="max-w-full truncate type-caption text-fg-muted tnum">{polledMarker(q.age_ms, marketOpen)}</span>
      )}
    </button>
  );
}

/**
 * Figma "Ladder row": every strike of the chosen expiry, a Buy each side —
 * one tap, a market order, no confirm — and "Add" on a contract already held.
 * Tapping a price charts that contract. Prices flow for the strikes around
 * ATM; scrolling away asks the stream to follow, so any strike can be bought
 * once its price shows.
 */
export function Ladder({ board, chain, held, lots, onLots, blocked, readOnly = null, marketOpen = true, sending, onBuy, onChart, onCentre, desktop, header }: {
  board: ExecuteBoard;
  /** The full strike list of the board's expiry, once loaded. */
  chain: ChainAnswer | undefined;
  /** Positions held, by trading symbol. */
  held: Map<string, ExecutePosition>;
  lots: number;
  onLots: (n: number) => void;
  /** Why nothing can be bought now (market closed, stale feed…), or null. */
  blocked: string | null;
  /** Why no row of this ladder can be bought at all — a stock's: never
   * from the owner's desk, on a paper book while the server's `stock_paper`
   * switch is off — or null. Every Buy is disabled with it as its title; the
   * prices still chart. */
  readOnly?: string | null;
  /** The market is open (the stream's `market_open`): closed, a polled row's
   * price is the last close, and its marker says so. */
  marketOpen?: boolean;
  /** Symbols with a buy in flight. */
  sending: Record<string, boolean>;
  onBuy: (sym: string, label: string) => void;
  onChart: (strike: number, side: 'CE' | 'PE') => void;
  /** The ladder rests on a strike outside the priced window: stream prices
   * around it. `null` is back at ATM. */
  onCentre: (strike: number | null) => void;
  desktop: boolean;
  /** Phone: next to the lots stepper (the expiry picker). */
  header?: ReactNode;
}) {
  const rows = ladderRows(chain, board);
  const ladderKind = boardKind(board);
  const spot = board.spot;
  const atm = board.atm;
  const list = useRef<HTMLDivElement>(null);
  const rowEls = useRef(new Map<number, HTMLDivElement>());
  const settle = useRef<number | null>(null);
  const [atmVisible, setAtmVisible] = useState(true);
  // Centred once per ladder (index + expiry), when ATM is first known.
  const centredFor = useRef<string | null>(null);
  // Centred again when the full chain replaces the board's own rows.
  const fromChain = chain != null && chain.expiry === board.expiry;
  const ladderKey = `${board.index}:${board.expiry}:${fromChain ? 'chain' : rows.length > 0 ? 'board' : 'none'}`;

  const rowOf = (strike: number) => rowEls.current.get(strike) ?? null;

  const scrollToAtm = useCallback(
    (behavior: ScrollBehavior) => {
      const box = list.current;
      const el = atm != null ? rowEls.current.get(atm) : null;
      if (!box || !el) return false;
      box.scrollTo({ top: el.offsetTop - box.clientHeight / 2 + el.offsetHeight / 2, behavior });
      return true;
    },
    [atm],
  );

  useLayoutEffect(() => {
    if (centredFor.current === ladderKey) return;
    if (scrollToAtm('instant')) centredFor.current = ladderKey;
  }, [ladderKey, scrollToAtm]);

  /** The strike at the middle of the visible rows, if any. */
  const middleStrike = (): number | null => {
    const box = list.current;
    if (!box) return null;
    const mid = box.scrollTop + box.clientHeight / 2;
    let best: number | null = null;
    let dist = Infinity;
    for (const [strike, el] of rowEls.current) {
      const d = Math.abs(el.offsetTop + el.offsetHeight / 2 - mid);
      if (d < dist) {
        dist = d;
        best = strike;
      }
    }
    return best;
  };

  const onScroll = () => {
    const box = list.current;
    const atmEl = atm != null ? rowOf(atm) : null;
    if (box && atmEl) {
      const top = atmEl.offsetTop - box.scrollTop;
      setAtmVisible(top >= 0 && top + atmEl.offsetHeight <= box.clientHeight);
    }
    if (settle.current != null) window.clearTimeout(settle.current);
    settle.current = window.setTimeout(() => {
      settle.current = null;
      const m = middleStrike();
      if (m == null) return;
      const { from, to } = board.window;
      // Inside the priced window nothing is asked; outside, prices follow.
      if (m < from || m > to) onCentre(atm != null && m === atm ? null : m);
    }, SCROLL_SETTLE_MS);
  };

  useEffect(
    () => () => {
      if (settle.current != null) window.clearTimeout(settle.current);
    },
    [],
  );

  const side = (q: ExecuteQuote | null, strike: number, kind: 'CE' | 'PE', itm: boolean, near: boolean) => {
    const label = `${board.index} ${strike} ${kind} (${expiryLabel(board.expiry)})`;
    const position = q ? held.get(q.sym) : undefined;
    const why = readOnly ?? quoteBlocked(q, blocked, buyMaxAgeMs(board), ladderKind);
    const busy = q ? !!sending[q.sym] : false;
    const button = (
      <Button
        pointerOnly
        kind="profit"
        size={desktop ? 'sm' : 'lg'}
        disabled={!q || why != null || busy}
        title={why ?? (position ? `Add ${lots * board.lot_size} at market` : `Buy ${lots * board.lot_size} at market`)}
        onClick={() => q && onBuy(q.sym, label)}
        className={desktop ? 'w-12' : 'w-16'}
      >
        {position ? 'Add' : 'Buy'}
      </Button>
    );
    const quote = <Quote q={q} kind={kind} label={label} position={position} flashes={near || position != null} marketOpen={marketOpen} onChart={() => onChart(strike, kind)} />;
    // A held contract is tinted profit; one in the money is shaded.
    const shade = position ? 'bg-profit-subtle' : itm ? 'bg-itm' : '';
    return (
      // Desktop's narrower column keeps the gaps tight so "polled · close" fits on its line.
      <div className={`flex min-w-0 flex-1 items-center ${desktop ? 'gap-2 px-1.5' : 'gap-3 px-2'} ${shade}`}>
        {kind === 'CE' ? (
          <>
            {button}
            {quote}
          </>
        ) : (
          <>
            {quote}
            {button}
          </>
        )}
      </div>
    );
  };

  const anyHeld = (strike: number) => rows.some((r) => r.strike === strike && [r.ce, r.pe].some((q) => q && held.has(q.sym)));
  // Where the spot line falls: before the first strike above it.
  const spotBefore = spot == null ? -1 : rows.findIndex((r) => r.strike > spot);
  const atmAt = atm == null ? -1 : rows.findIndex((r) => r.strike === atm);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {desktop ? (
        <div className="flex items-center gap-3 px-3 pb-3">
          <LotsStepper lots={lots} lotSize={board.lot_size} onChange={onLots} size="sm" />
          <span className="ml-auto type-caption text-fg-subtle">Lot size {board.lot_size}</span>
        </div>
      ) : (
        // One row, so the strikes get the room; the qty beside the stepper says the lot size.
        <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 px-4 pb-2">
          <LotsStepper lots={lots} lotSize={board.lot_size} onChange={onLots} size="lg" label={false} />
          {header && <div className="ml-auto">{header}</div>}
        </div>
      )}
      {desktop && (
        <div className="flex px-2 pb-1 type-eyebrow text-fg-muted">
          <span className="flex-1">Calls</span>
          <span className="w-16 text-center">Strike</span>
          <span className="flex-1 text-right">Puts</span>
        </div>
      )}
      <div className="relative min-h-0 flex-1">
        <div ref={list} onScroll={onScroll} className="relative h-full overflow-y-auto">
          <div className="flex flex-col">
            {rows.map((r, i) => (
              <div
                key={r.strike}
                ref={(el) => {
                  if (el) rowEls.current.set(r.strike, el);
                  else rowEls.current.delete(r.strike);
                }}
              >
                {/* Within three strikes of ATM the prices flash as they tick. */}
                {i === spotBefore && spot != null && (
                  <div className="relative my-0.5 flex items-center justify-center">
                    <span className="absolute inset-x-0 top-1/2 border-t border-dashed border-line-strong" />
                    <span className="relative bg-panel px-2 type-caption text-fg tnum">Spot {level(spot)}</span>
                  </div>
                )}
                <div className={`flex items-stretch border-t border-line ${desktop ? 'h-10' : 'h-14'}`}>
                  {side(r.ce, r.strike, 'CE', spot != null && r.strike < spot, atmAt >= 0 && Math.abs(i - atmAt) <= 3)}
                  <span className={`grid w-16 shrink-0 place-items-center num-medium ${anyHeld(r.strike) ? 'm-1 rounded-6 border border-line-strong text-fg' : r.strike === atm ? 'text-fg' : 'text-fg-subtle'}`}>{r.strike}</span>
                  {side(r.pe, r.strike, 'PE', spot != null && r.strike > spot, atmAt >= 0 && Math.abs(i - atmAt) <= 3)}
                </div>
              </div>
            ))}
          </div>
        </div>
        {!atmVisible && atm != null && (
          <button
            type="button"
            onClick={() => {
              scrollToAtm('smooth');
              onCentre(null);
            }}
            className="absolute bottom-2 left-1/2 -translate-x-1/2 rounded-full border border-line bg-panel px-3 py-1 type-label text-fg shadow-md hover:bg-raised"
          >
            ATM {atm}
          </button>
        )}
      </div>
      {desktop && (
        <p className="shrink-0 px-3 pt-3 type-caption text-fg-subtle">
          {readOnly ? `${readOnly} · tap a price to chart it · prices follow where you scroll` : 'One tap = market order, no confirm · tap a price to chart it · prices follow where you scroll'}
        </p>
      )}
    </div>
  );
}
