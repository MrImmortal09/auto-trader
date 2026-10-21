import type { ExecuteBoard, ExecutePosition, ExecuteQuote } from '../../types';
import { CONTRACT_STALE_MS, daysLeftLabel, expiryLabel } from '../../lib/execute';
import { level, price, signedMoney } from '../lib/format';
import { Button } from '../ui/Button';
import { LotsStepper } from '../ui/LotsStepper';

/** Why one side of a row can't be bought now, or null. */
function quoteBlocked(q: ExecuteQuote | null, blocked: string | null): string | null {
  if (blocked) return blocked;
  if (!q || q.ltp == null) return 'No price for this contract yet';
  if (q.age_ms != null && q.age_ms > CONTRACT_STALE_MS) return "This contract's price is stale";
  return null;
}

/**
 * Figma "Ladder row": the nearest expiry's strikes around ATM, a Buy each side
 * — one tap, a market order, no confirm — and "Add" on a contract already held.
 * Tapping a price charts that contract.
 */
export function Ladder({ board, held, lots, onLots, blocked, sending, onBuy, onChart, rows, desktop }: {
  board: ExecuteBoard;
  /** Scalps held, by trading symbol. */
  held: Map<string, ExecutePosition>;
  lots: number;
  onLots: (n: number) => void;
  /** Why nothing can be bought now (market closed, stale feed…), or null. */
  blocked: string | null;
  /** Symbols with a buy in flight. */
  sending: Record<string, boolean>;
  onBuy: (sym: string, label: string) => void;
  onChart: (strike: number, side: 'CE' | 'PE') => void;
  /** Strikes each side of ATM. */
  rows: number;
  desktop: boolean;
}) {
  const atmAt = board.rows.findIndex((r) => r.strike === board.atm);
  const centre = atmAt >= 0 ? atmAt : Math.floor(board.rows.length / 2);
  const shown = board.rows.slice(Math.max(0, centre - rows), centre + rows + 1);
  const spot = board.spot;

  const side = (q: ExecuteQuote | null, strike: number, kind: 'CE' | 'PE', itm: boolean) => {
    const label = `${board.index} ${strike} ${kind}`;
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
      <button type="button" onClick={() => onChart(strike, kind)} title={`Chart ${label}`} className={`flex min-w-0 flex-1 flex-col justify-center ${kind === 'CE' ? 'items-end text-right' : 'items-start text-left'}`}>
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

  const anyHeld = (strike: number) => shown.some((r) => r.strike === strike && [r.ce, r.pe].some((q) => q && held.has(q.sym)));
  // Where the spot line falls: before the first strike above it.
  const spotBefore = spot == null ? -1 : shown.findIndex((r) => r.strike > spot);

  return (
    <div className="flex min-h-0 flex-col">
      <div className={`flex items-center gap-3 ${desktop ? 'px-3 pb-3' : 'px-4 pb-2'}`}>
        <LotsStepper lots={lots} lotSize={board.lot_size} onChange={onLots} size={desktop ? 'sm' : 'lg'} />
        <span className="ml-auto type-caption text-fg-muted">
          {desktop ? `Lot size ${board.lot_size}` : `Expiry ${expiryLabel(board.expiry)} · ${daysLeftLabel(board.days_to_expiry)}`}
        </span>
      </div>
      {desktop && (
        <div className="flex px-2 pb-1 type-caption text-fg-muted">
          <span className="flex-1">Calls</span>
          <span className="w-16 text-center">Strike</span>
          <span className="flex-1 text-right">Puts</span>
        </div>
      )}
      <div className="flex flex-col">
        {shown.map((r, i) => (
          <div key={r.strike}>
            {i === spotBefore && spot != null && (
              <div className="relative my-0.5 flex items-center justify-center">
                <span className="absolute inset-x-0 top-1/2 border-t border-dashed border-line-strong" />
                <span className="relative bg-panel px-2 type-caption text-fg tnum">Spot {level(spot)}</span>
              </div>
            )}
            <div className={`flex items-stretch border-t border-line ${desktop ? 'h-10' : 'h-14'}`}>
              {side(r.ce, r.strike, 'CE', spot != null && r.strike < spot)}
              <span className={`grid w-16 shrink-0 place-items-center num-medium ${anyHeld(r.strike) ? 'm-1 rounded-6 border border-line-strong text-fg' : 'text-fg-muted'}`}>{r.strike}</span>
              {side(r.pe, r.strike, 'PE', spot != null && r.strike > spot)}
            </div>
          </div>
        ))}
      </div>
      {desktop && <p className="px-3 pt-3 type-caption text-fg-muted">One tap = market order, no confirm · tap a price to chart it</p>}
    </div>
  );
}
