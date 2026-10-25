import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import type { ExecuteBoard, ExecutePosition, ExecuteQuote } from '../../types';
import { CONTRACT_STALE_MS, expiryLabel } from '../../lib/execute';
import { level, price, signedMoney } from '../lib/format';
import type { ChainAnswer } from '../lib/types';
import { Button } from '../ui/Button';
import { LotsStepper } from '../ui/LotsStepper';

/** Why one side of a row can't be bought now, or null. */
function quoteBlocked(q: ExecuteQuote | null, blocked: string | null): string | null {
  if (blocked) return blocked;
  if (!q || q.ltp == null) return 'No price for this contract yet';
  if (q.age_ms != null && q.age_ms > CONTRACT_STALE_MS) return "This contract's price is stale";
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
 * Figma "Ladder row": every strike of the chosen expiry, a Buy each side —
 * one tap, a market order, no confirm — and "Add" on a contract already held.
 * Tapping a price charts that contract. Prices flow for the strikes around
 * ATM; scrolling away asks the stream to follow, so any strike can be bought
 * once its price shows.
 */
export function Ladder({ board, chain, held, lots, onLots, blocked, sending, onBuy, onChart, onCentre, desktop, header }: {
  board: ExecuteBoard;
  /** The full strike list of the board's expiry, once loaded. */
  chain: ChainAnswer | undefined;
  /** Positions held, by trading symbol. */
  held: Map<string, ExecutePosition>;
  lots: number;
  onLots: (n: number) => void;
  /** Why nothing can be bought now (market closed, stale feed…), or null. */
  blocked: string | null;
  /** Symbols with a buy in flight. */
  sending: Record<string, boolean>;
  onBuy: (sym: string, label: string) => void;
  onChart: (strike: number, side: 'CE' | 'PE') => void;
  /** The ladder rests on a strike outside the priced window: stream prices
   * around it. `null` is back at ATM. */
  onCentre: (strike: number | null) => void;
  desktop: boolean;
  /** Phone: a row under the lots stepper (the expiry picker). */
  header?: ReactNode;
}) {
  const rows = ladderRows(chain, board);
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

  const side = (q: ExecuteQuote | null, strike: number, kind: 'CE' | 'PE', itm: boolean) => {
    const label = `${board.index} ${strike} ${kind} (${expiryLabel(board.expiry)})`;
    const position = q ? held.get(q.sym) : undefined;
    const why = quoteBlocked(q, blocked);
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
    const quote = (
      <button type="button" disabled={!q} onClick={() => onChart(strike, kind)} title={`Chart ${label}`} className={`flex min-w-0 flex-1 flex-col justify-center ${kind === 'CE' ? 'items-end text-right' : 'items-start text-left'}`}>
        <span className={`num-medium ${q?.ltp == null ? 'text-fg-muted' : 'text-fg'}`}>{q?.ltp == null ? '—' : price(q.ltp)}</span>
        {position && (
          <span className={`type-caption tnum ${position.pnl != null && position.pnl < 0 ? 'text-loss' : 'text-profit'}`}>
            {position.qty} · {position.pnl == null ? '—' : signedMoney(position.pnl, 0)}
          </span>
        )}
      </button>
    );
    const shade = position ? 'bg-profit-subtle' : itm ? 'bg-raised/50' : '';
    return (
      <div className={`flex min-w-0 flex-1 items-center gap-3 px-2 ${shade}`}>
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

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className={`flex items-center gap-3 ${desktop ? 'px-3 pb-3' : 'px-4 pb-2'}`}>
        <LotsStepper lots={lots} lotSize={board.lot_size} onChange={onLots} size={desktop ? 'sm' : 'lg'} />
        <span className="ml-auto type-caption text-fg-muted">Lot size {board.lot_size}</span>
      </div>
      {header && <div className="flex items-center px-4 pb-2">{header}</div>}
      {desktop && (
        <div className="flex px-2 pb-1 type-caption text-fg-muted">
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
                {i === spotBefore && spot != null && (
                  <div className="relative my-0.5 flex items-center justify-center">
                    <span className="absolute inset-x-0 top-1/2 border-t border-dashed border-line-strong" />
                    <span className="relative bg-panel px-2 type-caption text-fg tnum">Spot {level(spot)}</span>
                  </div>
                )}
                <div className={`flex items-stretch border-t border-line ${desktop ? 'h-10' : 'h-14'}`}>
                  {side(r.ce, r.strike, 'CE', spot != null && r.strike < spot)}
                  <span className={`grid w-16 shrink-0 place-items-center num-medium ${anyHeld(r.strike) ? 'm-1 rounded-6 border border-line-strong text-fg' : r.strike === atm ? 'text-fg' : 'text-fg-muted'}`}>{r.strike}</span>
                  {side(r.pe, r.strike, 'PE', spot != null && r.strike > spot)}
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
            className="absolute bottom-2 left-1/2 -translate-x-1/2 rounded-full border border-line bg-panel px-3 py-1 type-label text-fg shadow-lg hover:bg-raised"
          >
            ATM {atm}
          </button>
        )}
      </div>
      {desktop && <p className="shrink-0 px-3 pt-3 type-caption text-fg-muted">One tap = market order, no confirm · tap a price to chart it · prices follow where you scroll</p>}
    </div>
  );
}
