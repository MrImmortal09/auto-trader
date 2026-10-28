import { useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react';
import { ChevronDown, ChevronUp } from 'lucide-react';
import type { ExecutePosition } from '../../../types';
import { contractLabel, expiryLabel } from '../../../lib/execute';
import { getPositionsRaw } from '../../lib/api';
import { money, plural, price, signedMoney, signedPct, toneOf } from '../../lib/format';
import { usePoll } from '../../lib/usePoll';
import type { Call, DeskAnswer, FillMark } from '../../lib/types';
import { Button } from '../../ui/Button';
import { Chip } from '../../ui/Chip';
import { HoldButton } from '../../ui/HoldButton';
import type { LevelKind } from '../lines';
import type { PaperAccount, PositionHandlers } from '../Positions';
import { busyNote, levelWords } from '../words';
import { activityEvents, type EngineError } from './activity';
import { clampPanelHeight } from './height';
import { ActivityList, HistoryList, PaperActivityList } from './lists';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;
/** What a user's paper portfolio starts with, and starts over with. */
const PAPER_START = 1_000_000;

type TabId = 'positions' | 'waiting' | 'orders' | 'history' | 'activity';

/** A panel tab: its label and count, underlined while open. */
function PanelTab({ id, label, count, active, onSelect }: { id: TabId; label: string; count: number | null; active: boolean; onSelect: (id: TabId) => void }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={() => onSelect(id)}
      className={`-mb-px flex h-11 shrink-0 items-center gap-2 border-b-2 px-3 type-body ${active ? 'border-fg text-fg' : 'border-transparent text-fg-muted hover:text-fg'}`}
    >
      {label}
      {count != null && (
        <span className={`grid h-5 min-w-5 place-items-center rounded-full px-1.5 type-caption tnum ${active ? 'bg-fg text-canvas' : 'bg-raised text-fg-muted'}`}>{count}</span>
      )}
    </button>
  );
}

/** One block of the account column: a label, then its figure or lines. */
function Block({ label, right, children }: { label: ReactNode; right?: ReactNode; children?: ReactNode }) {
  return (
    <section className="flex flex-col gap-1 border-t border-line px-4 py-2.5 first:border-t-0">
      <div className="flex items-baseline justify-between gap-3">
        <span className="type-label text-fg-muted">{label}</span>
        {right}
      </div>
      {children}
    </section>
  );
}

/**
 * The account column: whose money, what it is worth now and how today went,
 * what is open, and (for a user) the cash. A user's equity is their paper
 * cash plus their positions at the last price, measured against the
 * ₹10,00,000 they started with; the owner's desk shows its funds — Kotak's
 * in LIVE, the paper wallet otherwise.
 */
export function AccountColumn({ mode, desk, positions, paper, vertical }: {
  mode: string;
  desk: DeskAnswer | null;
  /** The open positions (not a buy still being placed). */
  positions: ExecutePosition[];
  paper?: PaperAccount;
  vertical: boolean;
}) {
  const live = !paper && mode === 'LIVE';
  const cash = desk?.wallet.balance ?? null;
  const openPnl = positions.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  const held = positions.reduce((sum, p) => sum + (p.ltp ?? p.avg) * p.qty, 0);
  const equity = paper && cash != null ? cash + held : null;
  const since = equity == null ? null : equity - PAPER_START;
  const closed = desk?.closed.length ?? 0;
  const booked = desk?.kpis.booked ?? 0;
  const funds = paper ? equity : cash;
  const fundsLabel = paper ? 'Equity' : live ? 'Funds at Kotak' : 'Paper wallet';
  const fundsNote = paper ? (
    since == null ? null : (
      <>
        <span className={TONE_TEXT[toneOf(since)]}>
          {signedMoney(since)} · {signedPct((since / PAPER_START) * 100)}
        </span>{' '}
        since ₹10,00,000
      </>
    )
  ) : desk?.wallet.source === 'LIVE_UNAVAILABLE' ? (
    "Kotak's balance isn't read yet"
  ) : null;
  const todayLine = closed === 0 && booked === 0 ? 'No trades closed today.' : `${plural(closed, 'trade')} closed`;

  if (!vertical) {
    // Phone: the same figures, two by two.
    return (
      <div className="grid grid-cols-2 gap-3">
        <div className="flex flex-col gap-0.5">
          <span className="type-label text-fg-muted">{fundsLabel}</span>
          <span className="num-medium text-fg">{funds == null ? '—' : money(funds)}</span>
          {fundsNote && <span className="type-caption text-fg-muted tnum">{fundsNote}</span>}
        </div>
        <div className="flex flex-col gap-0.5">
          <span className="type-label text-fg-muted">Today</span>
          <span className={`num-medium ${TONE_TEXT[toneOf(booked)]}`}>{signedMoney(booked)}</span>
          <span className="type-caption text-fg-muted">{todayLine}</span>
        </div>
        <div className="flex flex-col gap-0.5">
          <span className="type-label text-fg-muted">Open · {plural(positions.length, 'position')}</span>
          <span className={`num-medium ${TONE_TEXT[toneOf(openPnl)]}`}>{signedMoney(openPnl)}</span>
        </div>
        {paper && (
          <div className="flex flex-col gap-0.5">
            <span className="type-label text-fg-muted">Cash</span>
            <span className="num-medium text-fg">{cash == null ? '—' : money(cash)}</span>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2.5">
        <span className="type-body-strong text-fg">{paper ? 'Paper portfolio' : 'Owner desk'}</span>
        <Chip kind={live ? 'live' : 'profit'}>{live ? 'LIVE' : 'Paper'}</Chip>
      </div>
      <Block label={fundsLabel}>
        <span className="num-large text-fg">{funds == null ? '—' : money(funds)}</span>
        {fundsNote && <span className="type-caption text-fg-muted tnum">{fundsNote}</span>}
      </Block>
      <Block label="Today" right={closed > 0 || booked !== 0 ? <span className={`num-small ${TONE_TEXT[toneOf(booked)]}`}>{signedMoney(booked)}</span> : undefined}>
        <span className="type-body text-fg">{todayLine}</span>
      </Block>
      <Block label={`Open · ${plural(positions.length, 'position')}`} right={<span className={`num-small ${TONE_TEXT[toneOf(openPnl)]}`}>{signedMoney(openPnl)}</span>}>
        {positions.map((p) => (
          <span key={p.id} className="flex items-baseline gap-2 type-caption">
            <span className="text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
            <span className="text-fg-muted tnum">Long · {p.qty}</span>
          </span>
        ))}
      </Block>
      {paper && <Block label="Cash" right={<span className="num-small text-fg">{cash == null ? '—' : money(cash)}</span>} />}
    </div>
  );
}

/**
 * The trading panel under the Trade charts: the account column, and the
 * tabs — Positions (every open trade with its stop and target, −1 lot and
 * Close), Waiting (first buys still being placed) and Orders (what rests at
 * Kotak) for the owner's desk, History (today's closed trades) and Activity.
 * Exit all is held. Its top edge drags to resize it; the chevron folds it.
 */
export function TradingPanel({ serverBase, mode, desk, scalps, errors, fills, history, h, canSet, onAdd, onRemove, open, onToggle, height, onHeight, paper }: {
  serverBase: string;
  mode: string;
  desk: DeskAnswer | null;
  /** Every manual position the stream carries, placing ones included. */
  scalps: ExecutePosition[];
  errors: EngineError[];
  fills: FillMark[];
  /** Today's closed calls, every channel. */
  history: Call[];
  h: PositionHandlers;
  canSet: boolean;
  onAdd: (p: ExecutePosition, kind: LevelKind) => void;
  onRemove: (p: ExecutePosition, kind: LevelKind) => void;
  /** The body shown; the tab row stays either way. */
  open: boolean;
  onToggle: () => void;
  /** Height while open, in px. */
  height: number;
  onHeight: (h: number) => void;
  /** A user's paper portfolio: no Waiting or Orders, their own activity. */
  paper?: PaperAccount;
}) {
  const [tab, setTab] = useState<TabId>('positions');
  // The height under a drag, kept here until the pointer lifts.
  const [dragged, setDragged] = useState<number | null>(null);
  const drag = useRef<{ y: number; h: number } | null>(null);
  const active = scalps.filter((p) => !p.placing);
  const waiting = scalps.filter((p) => p.placing);
  const openPnl = active.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  const closed = history.filter((c) => c.channel === 'Scalps');
  const shown: TabId = paper && (tab === 'waiting' || tab === 'orders') ? 'positions' : tab;
  // Resting orders only matter while that tab is open.
  const raw = usePoll(() => getPositionsRaw(serverBase), 5_000, serverBase, !paper && open && shown === 'orders');
  // Every order of a manual trade that is at Kotak right now.
  const atKotak = (raw.data ?? [])
    .filter((p) => p.manual && p.state !== 'Closed')
    .flatMap((p) => {
      const contract = contractLabel(p.signal.instrument_name, p.signal.strike, p.signal.option_type);
      const rows: { key: string; contract: string; what: string; id: string }[] = [];
      if (p.entry_order_id && p.executed_qty <= 0) rows.push({ key: `${p.id}:entry`, contract, what: 'Buy · filling', id: p.entry_order_id });
      if (p.pending_exit_order_id) rows.push({ key: `${p.id}:exit`, contract, what: 'Sell · filling', id: p.pending_exit_order_id });
      if (p.sl_order_id) rows.push({ key: `${p.id}:sl`, contract, what: `SL-M · trigger ${price(p.sl_order_trigger ?? 0)} · ${p.sl_order_qty ?? 0} qty`, id: p.sl_order_id });
      return rows;
    });

  const onDragStart = (e: ReactPointerEvent<HTMLDivElement>) => {
    drag.current = { y: e.clientY, h: height };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onDragMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (d) setDragged(clampPanelHeight(d.h + (d.y - e.clientY)));
  };
  const onDragEnd = () => {
    if (drag.current && dragged != null) onHeight(dragged);
    drag.current = null;
    setDragged(null);
  };

  const level = (p: ExecutePosition, kind: LevelKind, locked: boolean) => {
    const lvl = p.chart?.[kind] ?? null;
    const word = kind === 'stop' ? 'Stop' : 'Target';
    if (lvl) {
      return (
        <span className={`inline-flex items-center gap-1 type-caption tnum ${kind === 'stop' ? 'text-loss' : 'text-profit'}`}>
          {levelWords(p, kind, lvl)}
          <button type="button" disabled={locked} title={`Remove the ${kind}`} aria-label={`Remove the ${kind}`} onClick={() => onRemove(p, kind)} className="grid size-5 place-items-center rounded-4 border border-current/40 hover:bg-raised disabled:opacity-40">
            ×
          </button>
        </span>
      );
    }
    if (!canSet) return <span className="type-caption text-fg-muted">—</span>;
    return (
      <button type="button" disabled={locked} onClick={() => onAdd(p, kind)} className={`type-caption hover:underline disabled:opacity-40 ${kind === 'stop' ? 'text-loss' : 'text-profit'}`}>
        + {word}
      </button>
    );
  };

  const GRID = 'grid grid-cols-[minmax(170px,1.3fr)_110px_120px_88px_88px_minmax(110px,1fr)_minmax(110px,1fr)_150px] items-center gap-3';
  const positions =
    active.length === 0 ? (
      <p className="px-1 py-3 type-body text-fg-muted">No open trade. Tap Buy on the ladder to open one.</p>
    ) : (
      <div className="flex flex-col">
        <div className={`${GRID} sticky top-0 border-b border-line bg-panel px-1 py-2 type-caption text-fg-muted`}>
          <span>Symbol</span>
          <span className="text-right">Qty</span>
          <span className="text-right">Open P&L</span>
          <span className="text-right">Avg entry</span>
          <span className="text-right">Last</span>
          <span>Stop</span>
          <span>Target</span>
          <span />
        </div>
        {active.map((p) => {
          const note = busyNote(p, !!h.sending[p.id]);
          const locked = note != null || p.qty <= 0;
          const pct = p.pnl != null && p.avg > 0 && p.qty > 0 ? (p.pnl / (p.avg * p.qty)) * 100 : null;
          return (
            <div key={p.id} className={`${GRID} border-b border-line px-1 py-2.5`}>
              <span className="flex min-w-0 flex-col">
                <span className="truncate type-body-strong text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
                <span className="truncate type-caption text-fg-muted">
                  {p.expiry ? expiryLabel(p.expiry) : 'Scalp'}
                  {note && <span className={p.halted ? 'text-loss' : 'text-fg'}> · {note}</span>}
                </span>
              </span>
              <span className="flex items-center justify-end gap-2">
                <Chip kind="profit">Long</Chip>
                <span className="num-medium text-fg">{p.qty}</span>
              </span>
              <span className="flex flex-col items-end">
                <span className={`num-medium ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</span>
                {pct != null && <span className={`type-caption tnum ${TONE_TEXT[toneOf(pct)]}`}>{signedPct(pct)}</span>}
              </span>
              <span className="text-right num-medium text-fg">{price(p.avg)}</span>
              <span className="text-right num-medium text-fg">{p.ltp == null ? '—' : price(p.ltp)}</span>
              <span>{level(p, 'stop', locked)}</span>
              <span>{level(p, 'target', locked)}</span>
              <span className="flex justify-end gap-2">
                <Button pointerOnly kind="ghost" disabled={locked || p.qty < p.lot_size} onClick={() => h.onLessLot(p)} title={`Sell ${p.lot_size} at market`}>
                  −1 lot
                </Button>
                <Button pointerOnly kind="loss-outline" disabled={locked} onClick={() => h.onExit(p)} title={`Sell all ${p.qty} at market`}>
                  Close
                </Button>
              </span>
            </div>
          );
        })}
      </div>
    );

  const waitingRows =
    waiting.length === 0 ? (
      <p className="px-1 py-3 type-body text-fg-muted">No buy waiting. A tap on the ladder shows here until its order is sent.</p>
    ) : (
      <ul className="flex flex-col">
        {waiting.map((p) => (
          <li key={p.id} className="flex items-center gap-3 border-b border-line px-1 py-2.5">
            <span className="min-w-0 flex-1 type-body-strong text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
            <span className="type-caption text-fg">{p.halted ? 'Halted by the engine — see System' : 'Buying at market…'}</span>
            {mode === 'LIVE' ? (
              // In LIVE the row can't be dropped safely while the engine may be mid-send:
              // the engine abandons a buy it couldn't send within 15 s.
              <span className="type-caption text-fg-muted">Dropped by the engine if not sent in 15 s</span>
            ) : (
              <Button pointerOnly kind="ghost" disabled={!!h.sending[p.id]} onClick={() => h.onCancel(p)}>
                Cancel
              </Button>
            )}
          </li>
        ))}
      </ul>
    );

  const orders =
    atKotak.length === 0 ? (
      <p className="px-1 py-3 type-body text-fg-muted">
        {raw.data
          ? mode === 'LIVE'
            ? 'Nothing is at Kotak right now. A buy or an exit shows here while it fills; chart stops and targets are watched by the engine and sent at market when hit.'
            : 'Paper trades place no orders at Kotak.'
          : 'Loading…'}
      </p>
    ) : (
      <ul className="flex flex-col">
        {atKotak.map((o) => (
          <li key={o.key} className="flex items-center gap-3 border-b border-line px-1 py-2.5">
            <span className="min-w-0 flex-1 type-body-strong text-fg">{o.contract}</span>
            <span className="type-caption text-fg-muted tnum">{o.what}</span>
            <span className="type-caption text-fg-muted tnum">#{o.id}</span>
          </li>
        ))}
      </ul>
    );

  const header = (
    <header className="flex shrink-0 items-center gap-3 border-b border-line pr-3 pl-1">
      {open ? (
        <div role="tablist" aria-label="Trading panel" className="flex min-w-0 items-stretch overflow-x-auto">
          <PanelTab id="positions" label="Positions" count={active.length} active={shown === 'positions'} onSelect={setTab} />
          {!paper && <PanelTab id="waiting" label="Waiting" count={waiting.length} active={shown === 'waiting'} onSelect={setTab} />}
          {!paper && <PanelTab id="orders" label="Orders" count={null} active={shown === 'orders'} onSelect={setTab} />}
          <PanelTab id="history" label="History" count={closed.length} active={shown === 'history'} onSelect={setTab} />
          <PanelTab id="activity" label="Activity" count={null} active={shown === 'activity'} onSelect={setTab} />
        </div>
      ) : (
        <span className="flex h-11 items-center gap-2 px-3 type-body-strong text-fg">
          Trading <span className="type-caption text-fg-muted tnum">{plural(active.length, 'open position')}</span>
        </span>
      )}
      <span className="flex-1" />
      <span className="type-label text-fg-muted">Open</span>
      <span className={`num-medium ${TONE_TEXT[toneOf(openPnl)]}`}>{signedMoney(openPnl)}</span>
      <HoldButton size="sm" disabled={active.length === 0 || h.exitAllBusy} onFire={h.onExitAll}>
        {h.exitAllBusy ? 'Exiting…' : 'Hold to exit all'}
      </HoldButton>
      <button type="button" onClick={onToggle} aria-expanded={open} aria-label={open ? 'Fold the trading panel' : 'Open the trading panel'} className="grid size-8 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg">
        {open ? <ChevronDown size={16} /> : <ChevronUp size={16} />}
      </button>
    </header>
  );

  if (!open) {
    return <section className="flex shrink-0 flex-col rounded-12 border border-line bg-panel">{header}</section>;
  }

  return (
    <section style={{ height: dragged ?? height }} className="relative flex shrink-0 overflow-hidden rounded-12 border border-line bg-panel">
      {/* The top edge: drag to make the panel taller or shorter. */}
      <div
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize the trading panel"
        onPointerDown={onDragStart}
        onPointerMove={onDragMove}
        onPointerUp={onDragEnd}
        onPointerCancel={onDragEnd}
        className="group absolute inset-x-0 top-0 z-10 flex h-2 cursor-row-resize justify-center"
      >
        <span className="mt-0.5 h-1 w-10 rounded-full bg-line group-hover:bg-line-strong" />
      </div>
      <aside className="w-[260px] shrink-0 overflow-y-auto border-r border-line">
        <AccountColumn mode={mode} desk={desk} positions={active} paper={paper} vertical />
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        {header}
        <div className="min-h-0 flex-1 overflow-y-auto px-3">
          {shown === 'positions' && positions}
          {shown === 'waiting' && waitingRows}
          {shown === 'orders' && orders}
          {shown === 'history' && <HistoryList calls={closed} />}
          {shown === 'activity' && (paper ? <PaperActivityList items={paper.activity} /> : <ActivityList events={activityEvents(fills, errors)} />)}
        </div>
      </div>
    </section>
  );
}
