import { useState } from 'react';
import { ChevronDown, ChevronUp } from 'lucide-react';
import type { ExecutePosition } from '../../../types';
import { contractLabel, expiryLabel } from '../../../lib/execute';
import { getPositionsRaw } from '../../lib/api';
import { money, plural, price, signedMoney, toneOf } from '../../lib/format';
import { usePoll } from '../../lib/usePoll';
import type { Call, DeskAnswer, FillMark } from '../../lib/types';
import { Button } from '../../ui/Button';
import { SourceChip } from '../../ui/Chip';
import { HoldButton } from '../../ui/HoldButton';
import { Stat } from '../../ui/Stat';
import type { LevelKind } from '../lines';
import type { PositionHandlers } from '../Positions';
import { busyNote, levelWords } from '../words';
import { activityEvents, type EngineError } from './activity';
import { ActivityList, HistoryList } from './lists';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;
type TabId = 'positions' | 'waiting' | 'orders' | 'history' | 'activity';

/** Figma "Panel tab": a label with its count. */
function PanelTab({ id, label, count, active, onSelect }: { id: TabId; label: string; count: number | null; active: boolean; onSelect: (id: TabId) => void }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={() => onSelect(id)}
      className={`flex h-8 items-center gap-1.5 rounded-6 px-3 type-label ${active ? 'bg-raised text-fg' : 'text-fg-muted hover:text-fg'}`}
    >
      {label}
      {count != null && <span className="type-caption tnum">{count}</span>}
    </button>
  );
}

/** The account column: who trades, funds, today's figures and what's open. */
export function AccountColumn({ mode, desk, openPnl, openCount, vertical }: { mode: string; desk: DeskAnswer | null; openPnl: number; openCount: number; vertical: boolean }) {
  const live = mode === 'LIVE';
  const balance = desk?.wallet.balance ?? null;
  const fundsNote = desk?.wallet.source === 'LIVE_UNAVAILABLE' ? "Kotak's limits unavailable" : live ? 'available at Kotak' : 'paper wallet';
  const kpis = desk?.kpis;
  return (
    <div className={vertical ? 'flex flex-col gap-4' : 'grid grid-cols-4 gap-3'}>
      <div className="flex flex-col gap-1">
        <span className="type-label whitespace-nowrap text-fg-muted">Account</span>
        <span className={`type-body-strong ${live ? 'text-live' : 'text-fg'}`}>{live ? 'LIVE · Kotak' : 'Paper · simulated'}</span>
      </div>
      <Stat label="Funds" size="medium" value={balance == null ? '—' : money(balance)} sub={fundsNote} />
      <Stat
        label="Today"
        size="medium"
        value={kpis ? signedMoney(kpis.net) : '—'}
        tone={kpis ? toneOf(kpis.net) : 'neutral'}
        sub={kpis ? `booked ${signedMoney(kpis.booked, 0)} · ${plural(kpis.calls, 'call')} closed` : undefined}
      />
      <Stat label="Open" size="medium" value={signedMoney(openPnl)} tone={toneOf(openPnl)} sub={plural(openCount, 'trade')} />
    </div>
  );
}

/**
 * Handoff "Trading panel (Vela style)": under the charts, the account column
 * and the tabs — Positions (every open manual trade with its stop and target,
 * −1 lot and Exit), Waiting (first buys still being placed), Orders (what
 * rests at Kotak), History (today's closed trades) and Activity (today's
 * fills and the engine's errors). Exit all is held.
 */
export function TradingPanel({ serverBase, mode, desk, scalps, errors, fills, history, h, canSet, onAdd, onRemove, open, onToggle }: {
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
  /** The body shown; the header row stays either way. */
  open: boolean;
  onToggle: () => void;
}) {
  const [tab, setTab] = useState<TabId>('positions');
  const active = scalps.filter((p) => !p.placing);
  const waiting = scalps.filter((p) => p.placing);
  const openPnl = active.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  const closed = history.filter((c) => c.channel === 'Scalps');
  const events = activityEvents(fills, errors);
  // Resting orders only matter while that tab is open.
  const raw = usePoll(() => getPositionsRaw(serverBase), 5_000, serverBase, open && tab === 'orders');
  const resting = (raw.data ?? []).filter((p) => p.manual && p.sl_order_id);

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

  const positions =
    active.length === 0 ? (
      <p className="py-3 type-body text-fg-muted">No open trade. Tap Buy on the ladder to open one.</p>
    ) : (
      <div className="flex flex-col">
        <div className="grid grid-cols-[minmax(200px,1.4fr)_64px_104px_80px_80px_minmax(120px,1fr)_minmax(120px,1fr)_132px] items-center gap-3 px-1 pb-1 type-caption text-fg-muted">
          <span>Instrument</span>
          <span className="text-right">Qty</span>
          <span className="text-right">Open P&L</span>
          <span className="text-right">Avg</span>
          <span className="text-right">LTP</span>
          <span>Stop</span>
          <span>Target</span>
          <span />
        </div>
        {active.map((p) => {
          const note = busyNote(p, !!h.sending[p.id]);
          const locked = note != null || p.qty <= 0;
          return (
            <div key={p.id} className="grid grid-cols-[minmax(200px,1.4fr)_64px_104px_80px_80px_minmax(120px,1fr)_minmax(120px,1fr)_132px] items-center gap-3 border-t border-line px-1 py-2">
              <span className="flex min-w-0 flex-wrap items-center gap-2">
                <span className="type-body-strong text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
                <SourceChip source={{ kind: 'scalp', title: 'Scalp' }} />
                {p.expiry && <span className="type-caption text-fg-muted">{expiryLabel(p.expiry)}</span>}
                {note && <span className={`type-caption ${p.halted ? 'text-loss' : 'text-fg'}`}>{note}</span>}
              </span>
              <span className="text-right num-small text-fg">{p.qty}</span>
              <span className={`text-right num-small ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</span>
              <span className="text-right num-small text-fg">{price(p.avg)}</span>
              <span className="text-right num-small text-fg">{p.ltp == null ? '—' : price(p.ltp)}</span>
              <span>{level(p, 'stop', locked)}</span>
              <span>{level(p, 'target', locked)}</span>
              <span className="flex gap-2">
                <Button pointerOnly kind="loss-outline" disabled={locked || p.qty < p.lot_size} onClick={() => h.onLessLot(p)} title={`Sell ${p.lot_size} at market`}>
                  −1 lot
                </Button>
                <Button pointerOnly kind="loss" disabled={locked} onClick={() => h.onExit(p)} title={`Sell all ${p.qty} at market`}>
                  Exit
                </Button>
              </span>
            </div>
          );
        })}
      </div>
    );

  const waitingRows =
    waiting.length === 0 ? (
      <p className="py-3 type-body text-fg-muted">No buy waiting. A tap on the ladder shows here until its order is sent.</p>
    ) : (
      <ul className="flex flex-col">
        {waiting.map((p) => (
          <li key={p.id} className="flex items-center gap-3 border-t border-line px-1 py-2">
            <span className="min-w-0 flex-1 type-body-strong text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
            <span className="type-caption text-fg">{p.halted ? 'Halted by the engine — see System' : 'Buying at market…'}</span>
            <Button pointerOnly kind="ghost" disabled={!!h.sending[p.id]} onClick={() => h.onCancel(p)}>
              Cancel
            </Button>
          </li>
        ))}
      </ul>
    );

  const orders =
    resting.length === 0 ? (
      <p className="py-3 type-body text-fg-muted">
        {raw.data ? 'Nothing rests at Kotak. Chart stops and targets are watched by the engine and sent at market when hit.' : 'Loading…'}
      </p>
    ) : (
      <ul className="flex flex-col">
        {resting.map((p) => (
          <li key={p.id} className="flex items-center gap-3 border-t border-line px-1 py-2">
            <span className="min-w-0 flex-1 type-body-strong text-fg">{contractLabel(p.signal.instrument_name, p.signal.strike, p.signal.option_type)}</span>
            <span className="type-caption text-fg-muted tnum">SL-M · trigger {price(p.sl_order_trigger ?? 0)} · {p.sl_order_qty ?? 0} qty</span>
            <span className="type-caption text-fg-muted tnum">#{p.sl_order_id}</span>
          </li>
        ))}
      </ul>
    );

  return (
    <section className="flex shrink-0 flex-col rounded-12 border border-line bg-panel px-4 py-3">
      <header className="flex items-center gap-3">
        <button type="button" onClick={onToggle} aria-expanded={open} className="flex items-center gap-2 type-title text-fg">
          Trading
          <span className="type-caption text-fg-muted tnum">{active.length}</span>
          {open ? <ChevronDown size={16} className="text-fg-muted" /> : <ChevronUp size={16} className="text-fg-muted" />}
        </button>
        {open && (
          <div role="tablist" aria-label="Trading panel" className="ml-2 flex gap-1 rounded-8 bg-inset p-1">
            <PanelTab id="positions" label="Positions" count={active.length} active={tab === 'positions'} onSelect={setTab} />
            <PanelTab id="waiting" label="Waiting" count={waiting.length} active={tab === 'waiting'} onSelect={setTab} />
            <PanelTab id="orders" label="Orders" count={null} active={tab === 'orders'} onSelect={setTab} />
            <PanelTab id="history" label="History" count={closed.length} active={tab === 'history'} onSelect={setTab} />
            <PanelTab id="activity" label="Activity" count={null} active={tab === 'activity'} onSelect={setTab} />
          </div>
        )}
        <span className="flex-1" />
        <span className="type-label text-fg-muted">Open</span>
        <span className={`num-medium ${TONE_TEXT[toneOf(openPnl)]}`}>{signedMoney(openPnl)}</span>
        <HoldButton size="sm" disabled={active.length === 0 || h.exitAllBusy} onFire={h.onExitAll}>
          {h.exitAllBusy ? 'Exiting…' : 'Hold to exit all'}
        </HoldButton>
      </header>
      {open && (
        <div className="mt-3 flex gap-6">
          <aside className="w-[200px] shrink-0 border-r border-line pr-5">
            <AccountColumn mode={mode} desk={desk} openPnl={openPnl} openCount={active.length} vertical />
          </aside>
          <div className="min-w-0 flex-1">
            {tab === 'positions' && positions}
            {tab === 'waiting' && waitingRows}
            {tab === 'orders' && orders}
            {tab === 'history' && <HistoryList calls={closed} />}
            {tab === 'activity' && <ActivityList events={events} />}
          </div>
        </div>
      )}
    </section>
  );
}
