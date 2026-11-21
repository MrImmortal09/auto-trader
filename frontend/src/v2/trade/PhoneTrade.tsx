import { useState } from 'react';
import { ChevronLeft, ChevronRight, Link2, Maximize2, Minimize2 } from 'lucide-react';
import type { ExecuteBoard, ExecutePosition } from '../../types';
import { buyMaxAgeMs, expiryLabel } from '../../lib/execute';
import { level, money, price, signedMoney, toneOf } from '../lib/format';
import { useTickFlash } from '../lib/useTickFlash';
import { Button } from '../ui/Button';
import { LotsStepper } from '../ui/LotsStepper';
import { Modal } from '../ui/Modal';
import { Toggle } from '../ui/Toggle';
import { boardKind, quoteBlocked } from './model';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

export type OptionSide = 'CE' | 'PE';

/**
 * Phone, over the chart's bottom-left: the board's underlying (the chart can
 * show another), expiry and ATM strike, with Call and Put. Each opens the buy
 * sheet for that side — nothing is sent from here. Bottom-right, clear of the
 * price axis: full screen.
 */
export function ChartQuickBuy({ board, fullscreen, showButtons, onPick, onFullscreen }: {
  board: ExecuteBoard | null;
  fullscreen: boolean;
  /** Call / Put shown: off under Scalper, where the ladder below has every Buy. */
  showButtons: boolean;
  onPick: (side: OptionSide) => void;
  onFullscreen: (on: boolean) => void;
}) {
  const atm = board?.atm ?? null;
  const ready = showButtons && board != null && atm != null;
  return (
    <>
      {ready && (
        <div className="pointer-events-none absolute bottom-9 left-2 z-10 flex flex-col items-start gap-1.5">
          <span className="rounded-6 bg-canvas/85 px-1.5 py-0.5 type-caption text-fg-muted tnum">
            {board.index} · {expiryLabel(board.expiry)} · {level(atm)}
          </span>
          <div className="pointer-events-auto flex gap-2">
            <button
              type="button"
              onClick={() => onPick('CE')}
              className="h-10 min-w-[68px] rounded-8 bg-profit-subtle px-3.5 type-button-lg text-profit shadow-sm active:brightness-95"
            >
              Call
            </button>
            <button
              type="button"
              onClick={() => onPick('PE')}
              className="h-10 min-w-[68px] rounded-8 bg-loss-subtle px-3.5 type-button-lg text-loss shadow-sm active:brightness-95"
            >
              Put
            </button>
          </div>
        </div>
      )}
      <button
        type="button"
        aria-label={fullscreen ? 'Exit full screen' : 'Full screen'}
        title={fullscreen ? 'Exit full screen' : 'Full screen'}
        onClick={() => onFullscreen(!fullscreen)}
        className="absolute right-[72px] bottom-9 z-10 grid size-10 place-items-center rounded-full border border-line bg-panel text-fg-muted shadow-sm active:bg-raised"
      >
        {fullscreen ? <Minimize2 size={16} /> : <Maximize2 size={16} />}
      </button>
    </>
  );
}

/**
 * The Call / Put sheet: one contract of the board (ATM to start, the
 * strikes either side a tap away), its price, the lots, what that costs at
 * the last price, and one Buy. It refuses exactly when the ladder's Buy on
 * that contract would, and sends what the ladder's Buy sends (`onBuy`).
 */
export function BuySheet({ board, side, strike, onStrike, lots, onLots, held, blocked, readOnly, lineMode, sending, mode, onBuy, onClose }: {
  board: ExecuteBoard;
  side: OptionSide;
  strike: number;
  onStrike: (strike: number) => void;
  lots: number;
  onLots: (lots: number) => void;
  /** Positions held, by trading symbol. */
  held: Map<string, ExecutePosition>;
  /** Why nothing can be bought now (market closed, stale feed…), or null. */
  blocked: string | null;
  /** Why this board can't be bought from at all, or null. */
  readOnly: string | null;
  /** "Buy on a line" is on: Buy places a line order instead. */
  lineMode: boolean;
  sending: Record<string, boolean>;
  mode: string;
  /** What the ladder's Buy does; true once it was sent (or the line placed). */
  onBuy: (sym: string, label: string, strike: number, side: OptionSide) => Promise<boolean>;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const strikes = board.rows.map((r) => r.strike);
  const at = strikes.indexOf(strike);
  const row = board.rows[at];
  const q = row ? (side === 'CE' ? row.ce : row.pe) : null;
  const label = `${board.index} ${strike} ${side} (${expiryLabel(board.expiry)})`;
  const position = q ? held.get(q.sym) : undefined;
  const why =
    readOnly ??
    (position?.placing && position.line ? 'A buy line is armed on it — cancel it from its × on the chart, or the Waiting tab' : null) ??
    (lineMode && position ? 'Held already — turn off “Buy on a line” to add at market' : null) ??
    quoteBlocked(q, blocked, buyMaxAgeMs(board), boardKind(board));
  const inFlight = busy || (q ? !!sending[q.sym] : false);
  const qty = lots * board.lot_size;
  const cost = q?.ltp != null ? q.ltp * qty : null;
  const call = side === 'CE';

  async function send() {
    if (!q || why != null || inFlight) return;
    setBusy(true);
    try {
      if (await onBuy(q.sym, label, strike, side)) onClose();
    } finally {
      setBusy(false);
    }
  }

  const verb = position ? 'Add' : lineMode ? 'Place a buy line for' : 'Buy';
  return (
    <Modal open onClose={onClose} label={`${call ? 'Call' : 'Put'} · ${label}`} locked={busy}>
      <div className="flex flex-col gap-4 overflow-y-auto px-4 pt-3 pb-4">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className={`type-label ${call ? 'text-profit' : 'text-loss'}`}>{call ? 'Call' : 'Put'}</span>
              <span className={`rounded-4 px-1.5 type-caption ${mode === 'LIVE' ? 'bg-loss-subtle text-loss' : 'bg-raised text-fg-muted'}`}>{mode === 'LIVE' ? 'Live · Kotak' : 'Paper'}</span>
            </div>
            <div className="mt-0.5 type-title text-fg">
              {board.index} {level(strike)} {side}
            </div>
            <div className="type-caption text-fg-subtle">Expires {expiryLabel(board.expiry)}{strike === board.atm ? ' · ATM' : ''}</div>
          </div>
          <div className="shrink-0 text-right">
            <div className="num-large text-fg">{q?.ltp == null ? '—' : price(q.ltp)}</div>
            <div className="type-caption text-fg-subtle">last price</div>
          </div>
        </div>

        <div className="flex items-center justify-between gap-3">
          <span className="type-label text-fg-muted">Strike</span>
          <div className="flex items-center gap-1">
            <Button size="lg" kind="neutral" aria-label="Lower strike" disabled={at <= 0 || busy} onClick={() => onStrike(strikes[at - 1])}>
              <ChevronLeft size={18} />
            </Button>
            <span className="w-24 text-center num-medium text-fg">{level(strike)}</span>
            <Button size="lg" kind="neutral" aria-label="Higher strike" disabled={at < 0 || at >= strikes.length - 1 || busy} onClick={() => onStrike(strikes[at + 1])}>
              <ChevronRight size={18} />
            </Button>
          </div>
        </div>

        <LotsStepper lots={lots} lotSize={board.lot_size} onChange={onLots} size="lg" disabled={busy} />

        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-8 bg-inset px-3 py-2.5 type-body">
          <dt className="text-fg-muted">Order</dt>
          <dd className="text-right text-fg">{lineMode && !position ? 'Buy line on the chart' : 'Market buy'}</dd>
          <dt className="text-fg-muted">Approx. cost</dt>
          <dd className="text-right tnum text-fg">{cost == null ? '—' : money(cost)}</dd>
          {position && (
            <>
              <dt className="text-fg-muted">Held</dt>
              <dd className="text-right tnum text-fg">{position.qty} @ {price(position.avg)}</dd>
            </>
          )}
        </dl>

        {why && <p className="type-caption text-loss">{why}</p>}
        <p className="type-caption text-fg-subtle">A market order fills at the exchange's price, which can differ from the last price. Before charges.</p>

        <Button pointerOnly size="lg" kind="profit" disabled={!q || why != null || inFlight} onClick={() => void send()} className="w-full">
          {inFlight ? 'Sending…' : `${verb} ${qty} ${board.index} ${level(strike)} ${side}${lineMode && !position ? '' : ' at market'}`}
        </Button>
      </div>
    </Modal>
  );
}

/**
 * Phone, under the chart: the open trades' P&L (opens the positions sheet),
 * the option chain (the ladder, in a sheet), and Scalper — the ladder shown
 * under the chart, as the screen was before.
 */
export function PhoneTradeBar({ scalps, onOpen, onChain, chainReady, scalper, onScalper, paper }: {
  scalps: ExecutePosition[];
  onOpen: () => void;
  onChain: () => void;
  chainReady: boolean;
  scalper: boolean;
  onScalper: (on: boolean) => void;
  paper?: { balance: number | null };
}) {
  const open = scalps.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  const flash = useTickFlash(open, true, scalps.map((p) => p.id).join());
  return (
    <div className="flex shrink-0 items-center gap-2 border-t border-line bg-panel px-3 py-2">
      <button type="button" onClick={onOpen} className="min-w-0 flex-1 text-left" aria-label="Open trades">
        <div className="type-caption text-fg-muted">
          F&amp;O · {scalps.length === 1 ? '1 trade' : `${scalps.length} trades`}
          {paper?.balance != null && <span className="text-fg-subtle"> · {money(paper.balance)}</span>}
        </div>
        <div key={flash.key} className={`-mx-1 w-fit rounded-4 px-1 num-medium ${TONE_TEXT[toneOf(open)]} ${flash.className}`}>
          {signedMoney(open)}
        </div>
      </button>
      <button
        type="button"
        onClick={onChain}
        disabled={!chainReady}
        className="flex h-10 shrink-0 items-center gap-1.5 rounded-full border border-line px-3.5 type-button text-fg disabled:opacity-40"
      >
        <Link2 size={16} />
        Chain
      </button>
      <label className="flex h-10 shrink-0 items-center gap-2 rounded-full border border-line pr-2 pl-3.5 type-button text-fg">
        Scalper
        <Toggle checked={scalper} onChange={onScalper} label="Scalper: show the strike ladder under the chart" />
      </label>
    </div>
  );
}
