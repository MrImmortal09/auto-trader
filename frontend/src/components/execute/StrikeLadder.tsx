import { useEffect, useRef, useState } from 'react';
import { LoaderCircle } from 'lucide-react';
import type { ExecuteBoard, ExecutePosition, ExecuteQuote, ExecuteRow } from '../../types';
import { CONTRACT_STALE_MS, contractLabel, fmtPnl, fmtSpot, fmtStrike, VISIBLE_HALF } from '../../lib/execute';
import { PriceCell } from './PriceCell';

/** How long a touch on the ladder holds its rows still. */
const TOUCH_HOLD_MS = 1_500;
/** A tap this soon after the rows moved is ignored: it was aimed at the old row. */
const SHIFT_GUARD_MS = 400;

/** The ladder's 11 strikes: ATM ± 5 of what the stream sent. */
function centeredStrikes(board: ExecuteBoard): number[] {
  let mid = board.rows.findIndex((r) => r.strike === board.atm);
  if (mid < 0) mid = Math.floor(board.rows.length / 2);
  const start = Math.max(0, Math.min(mid - VISIBLE_HALF, board.rows.length - (2 * VISIBLE_HALF + 1)));
  return board.rows.slice(start, start + 2 * VISIBLE_HALF + 1).map((r) => r.strike);
}

type Side = 'CE' | 'PE';

function BuyButton({ held, pending, blocked, onClick, label }: {
  held: boolean;
  pending: boolean;
  blocked: string | null;
  /** `at`: the click's timestamp, on the `performance.now()` clock. */
  onClick: (at: number) => void;
  label: string;
}) {
  const disabled = pending || blocked != null;
  return (
    <button
      type="button"
      onClick={(e) => onClick(e.timeStamp)}
      disabled={disabled}
      title={blocked ?? `${held ? 'Add' : 'Buy'} ${label} at market — one tap, no confirm`}
      aria-label={`${held ? 'Add' : 'Buy'} ${label}`}
      className={`h-10 md:h-8 min-w-12 md:min-w-15 px-2 md:px-3 rounded-md text-[11px] font-extrabold tracking-wider inline-flex items-center justify-center transition active:scale-95 ${
        disabled && !pending
          ? 'bg-surface-container-high text-outline cursor-not-allowed active:scale-100'
          : 'bg-secondary text-on-secondary shadow-sm hover:brightness-110'
      }`}
    >
      {pending ? <LoaderCircle size={14} className="animate-spin" /> : held ? 'ADD' : 'BUY'}
    </button>
  );
}

function HeldChip({ p, side, onExit, exitBusy }: { p: ExecutePosition; side: Side; onExit: () => void; exitBusy: boolean }) {
  const pnl = p.pnl;
  const busy = exitBusy || p.exiting;
  if (p.placing) {
    return (
      <span className="hidden sm:inline px-1.5 py-0.5 rounded-full text-[10px] font-bold bg-amber-100 text-amber-900 whitespace-nowrap">buying…</span>
    );
  }
  return (
    <span className={`hidden sm:inline-flex items-center gap-1 ${side === 'PE' ? 'flex-row-reverse' : ''}`}>
      <span
        className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded-full text-[10px] font-bold font-mono-code tabular-nums whitespace-nowrap ${
          pnl == null ? 'bg-surface-container-high text-on-surface-variant' : pnl >= 0 ? 'bg-secondary/12 text-secondary' : 'bg-error-container text-on-error-container'
        }`}
        title={`Holding ${p.qty} @ ${p.avg.toFixed(2)}`}
      >
        <span className="w-1.5 h-1.5 rounded-full bg-current" />
        {p.qty}
        {pnl != null && <span className="hidden lg:inline">{fmtPnl(pnl)}</span>}
      </span>
      <button
        type="button"
        onClick={onExit}
        disabled={busy}
        title="Sell the whole position at market"
        className="hidden sm:inline-flex h-7 px-2 rounded-md bg-tertiary text-on-tertiary text-[10px] font-extrabold tracking-wider items-center hover:bg-tertiary/90 active:scale-95 transition disabled:bg-surface-container-high disabled:text-outline"
      >
        {busy ? <LoaderCircle size={11} className="animate-spin" /> : 'EXIT'}
      </button>
    </span>
  );
}

function SideCell({ side, quote, outside, itm, held, label, pending, exitBusy, blocked, onBuy, onExit }: {
  side: Side;
  quote: ExecuteQuote | null;
  /** Held on screen while ATM moved past it: no live price streamed for it now. */
  outside: boolean;
  itm: boolean;
  held: ExecutePosition | undefined;
  label: string;
  pending: boolean;
  exitBusy: boolean;
  blocked: string | null;
  onBuy: (at: number) => void;
  onExit: () => void;
}) {
  if (!quote) {
    return (
      <div className={`min-w-0 px-1.5 md:px-3 py-1.5 flex items-center ${side === 'CE' ? 'justify-start' : 'justify-end'} text-[11px] text-outline`}>
        {outside ? 'move away to refresh' : 'not listed'}
      </div>
    );
  }
  const noPrice = quote.ltp == null;
  const stale = quote.age_ms == null || quote.age_ms > CONTRACT_STALE_MS;
  const button = (
    <BuyButton
      held={Boolean(held)}
      pending={pending || Boolean(held?.placing || held?.adding)}
      blocked={
        blocked ??
        (noPrice ? 'No live price for this strike yet' : stale ? "This strike's price hasn't updated in 10 s" : null)
      }
      onClick={onBuy}
      label={label}
    />
  );
  const price = (
    <PriceCell
      value={quote.ltp}
      arrow={side === 'CE' ? 'after' : 'before'}
      className="text-[13px] md:text-[15px] font-bold text-on-surface max-sm:[&>span]:hidden"
    />
  );
  const chip = held && <HeldChip p={held} side={side} onExit={onExit} exitBusy={exitBusy} />;
  return (
    <div className={`min-w-0 flex items-center gap-1.5 md:gap-2 px-1.5 md:px-3 py-1.5 transition-colors ${itm ? 'bg-amber-50/70' : ''} ${side === 'PE' ? 'flex-row-reverse' : ''}`}>
      {button}
      {chip}
      <div className="flex-1" />
      {price}
    </div>
  );
}

/**
 * Calls | strike | puts for ATM ± 5 strikes of the selected index. Each side
 * buys at market in one tap. While the pointer is over the ladder (or a
 * finger just touched it) the rows hold still even if ATM moves, so a strike
 * never slides under a tap.
 */
export function StrikeLadder({ board, nowMs, heldBySymbol, pendingSyms, busyIds, blocked, onBuy, onExit, onShiftedTap }: {
  board: ExecuteBoard;
  nowMs: number;
  heldBySymbol: Map<string, ExecutePosition>;
  pendingSyms: Record<string, boolean>;
  busyIds: Record<string, boolean>;
  blocked: string | null;
  onBuy: (sym: string, label: string) => void;
  onExit: (p: ExecutePosition) => void;
  /** A tap landed right as the rows moved and was ignored. */
  onShiftedTap: () => void;
}) {
  const [hovering, setHovering] = useState(false);
  const [touchHoldUntil, setTouchHoldUntil] = useState(0);
  // While frozen, the exact strikes on screen are pinned, so no row ever
  // moves under the pointer however far ATM runs.
  const [pinned, setPinned] = useState<number[] | null>(null);

  const frozen = hovering || touchHoldUntil > nowMs;
  const live = centeredStrikes(board);
  if (frozen && pinned == null) setPinned(live);
  if (!frozen && pinned != null) setPinned(null);
  const strikes = frozen && pinned ? pinned : live;

  const byStrike = new Map(board.rows.map((r) => [r.strike, r]));
  const rows: (ExecuteRow & { outside: boolean })[] = strikes.map((k) => {
    const row = byStrike.get(k);
    return row ? { ...row, outside: false } : { strike: k, ce: null, pe: null, outside: true };
  });
  const spot = board.spot;
  const atmMoved = frozen && board.atm != null && !strikes.includes(board.atm);

  // When the rows last moved — a tap right after it was aimed at the old row.
  const shiftedAt = useRef(0);
  const shownKey = strikes.join(',');
  useEffect(() => {
    shiftedAt.current = performance.now();
  }, [shownKey]);
  function buy(sym: string, label: string, at: number) {
    if (at - shiftedAt.current < SHIFT_GUARD_MS) {
      onShiftedTap();
      return;
    }
    onBuy(sym, label);
  }

  return (
    <div
      onPointerEnter={(e) => {
        if (e.pointerType === 'mouse') setHovering(true);
      }}
      onPointerLeave={(e) => {
        if (e.pointerType === 'mouse') setHovering(false);
      }}
      onTouchStart={() => setTouchHoldUntil(Date.now() + TOUCH_HOLD_MS)}
      className="relative"
    >
      <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] text-[10px] font-extrabold uppercase tracking-wider text-on-surface-variant bg-surface-container-low border-b border-outline-variant">
        <div className="px-3 py-2">Calls · CE</div>
        <div className="w-17 md:w-28 px-2 py-2 text-center">Strike</div>
        <div className="px-3 py-2 text-right">Puts · PE</div>
      </div>

      {frozen && board.atm != null && pinned != null && pinned[VISIBLE_HALF] !== board.atm && (
        <div className="absolute right-2 top-9 z-20 px-2 py-1 rounded-md bg-inverse-surface text-inverse-on-surface text-[10px] font-semibold shadow-lg pointer-events-none">
          ATM now {fmtStrike(board.atm)}{atmMoved ? ' (off this view)' : ''} — ladder held still while you aim
        </div>
      )}

      {rows.map((row, i) => {
        const isAtm = row.strike === board.atm;
        const next = rows[i + 1];
        const spotBelowNext = spot != null && next != null && spot >= row.strike && spot < next.strike;
        const ceHeld = row.ce ? heldBySymbol.get(row.ce.sym) : undefined;
        const peHeld = row.pe ? heldBySymbol.get(row.pe.sym) : undefined;
        const ceLabel = contractLabel(board.index, row.strike, 'CE');
        const peLabel = contractLabel(board.index, row.strike, 'PE');
        return (
          <div key={row.strike}>
            <div
              className={`grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-stretch border-b border-outline-variant/60 ${
                isAtm ? 'bg-primary/4' : ''
              }`}
            >
              <SideCell
                side="CE"
                quote={row.ce}
                outside={row.outside}
                itm={spot != null && row.strike < spot}
                held={ceHeld}
                label={ceLabel}
                pending={Boolean(row.ce && pendingSyms[row.ce.sym])}
                exitBusy={Boolean(ceHeld && busyIds[ceHeld.id])}
                blocked={blocked}
                onBuy={(at) => row.ce && buy(row.ce.sym, ceLabel, at)}
                onExit={() => ceHeld && onExit(ceHeld)}
              />
              <div
                className={`w-17 md:w-28 flex items-center justify-center gap-1 border-x border-outline-variant/60 font-mono-code text-[13px] md:text-sm font-bold tabular-nums ${
                  isAtm ? 'text-primary' : 'text-on-surface'
                }`}
              >
                {isAtm && <span className="px-1 py-px rounded bg-primary text-on-primary text-[8px] font-sans font-extrabold tracking-wider">ATM</span>}
                {fmtStrike(row.strike)}
              </div>
              <SideCell
                side="PE"
                quote={row.pe}
                outside={row.outside}
                itm={spot != null && row.strike > spot}
                held={peHeld}
                label={peLabel}
                pending={Boolean(row.pe && pendingSyms[row.pe.sym])}
                exitBusy={Boolean(peHeld && busyIds[peHeld.id])}
                blocked={blocked}
                onBuy={(at) => row.pe && buy(row.pe.sym, peLabel, at)}
                onExit={() => peHeld && onExit(peHeld)}
              />
            </div>
            {spotBelowNext && (
              <div className="relative h-0 z-10" aria-label={`Spot ${fmtSpot(spot!)}`}>
                <div className="absolute inset-x-0 -top-px border-t-2 border-primary/60" />
                <div className="absolute left-1/2 -translate-x-1/2 -translate-y-1/2 px-2 py-0.5 rounded-full bg-primary text-on-primary text-[10px] font-bold font-mono-code tabular-nums shadow">
                  {fmtSpot(spot!)}
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
