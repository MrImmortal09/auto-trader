import { useState } from 'react';
import type { ExecutePosition } from '../../types';
import { contractLabel, expiryLabel } from '../../lib/execute';
import { money, price, signedMoney, signedPct, toneOf } from '../lib/format';
import type { Call, DeskAnswer, DeskPosition, FillMark, PaperActivity } from '../lib/types';
import { PositionRail } from '../desk/PositionRail';
import { railFor } from '../desk/rail';
import { Button } from '../ui/Button';
import { SourceChip } from '../ui/Chip';
import { HoldButton } from '../ui/HoldButton';
import { Modal } from '../ui/Modal';
import { Segment } from '../ui/Segment';
import type { LevelKind } from './lines';
import { approxMoney, levelPnl } from './model';
import { AccountColumn } from './panel/TradingPanel';
import { activityEvents, type EngineError } from './panel/activity';
import { ActivityList, HistoryList, PaperActivityList } from './panel/lists';
import { busyNote, levelWords } from './words';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** A user's own paper portfolio, beside the positions: the wallet, today's
 * P&L and the recent activity. The owner's desk has none. */
export interface PaperAccount {
  balance: number | null;
  /** Booked plus open, today. */
  today: number | null;
  /** Newest first; null until loaded. */
  activity: PaperActivity[] | null;
}

export interface PositionHandlers {
  /** Positions with a sell in flight from here. */
  sending: Record<string, boolean>;
  onLessLot: (p: ExecutePosition) => void;
  onExit: (p: ExecutePosition) => void;
  /** A first buy still being placed: abandon it. */
  onCancel: (p: ExecutePosition) => void;
  onExitAll: () => void;
  exitAllBusy: boolean;
}

/** Phone: the bar under the ladder — open P&L (and a user's wallet), and
 * Exit all held. A tap opens the sheet. */
export function ScalpBar({ scalps, h, onOpen, paper }: { scalps: ExecutePosition[]; h: PositionHandlers; onOpen: () => void; paper?: PaperAccount }) {
  const open = scalps.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  return (
    <div className="flex shrink-0 items-center gap-3 border-t border-line bg-inset px-4 py-2">
      <button type="button" onClick={onOpen} className="min-w-0 flex-1 text-left">
        <div className="type-caption text-fg-muted">Open · {scalps.length === 1 ? '1 trade' : `${scalps.length} trades`}</div>
        <div className={`num-medium ${TONE_TEXT[toneOf(open)]}`}>{signedMoney(open)}</div>
        {paper && (
          <div className="type-caption text-fg-muted tnum">
            Wallet {paper.balance == null ? '—' : money(paper.balance)} · today{' '}
            <span className={TONE_TEXT[toneOf(paper.today)]}>{paper.today == null ? '—' : signedMoney(paper.today)}</span>
          </div>
        )}
      </button>
      <HoldButton size="lg" disabled={scalps.length === 0 || h.exitAllBusy} onFire={h.onExitAll}>
        {h.exitAllBusy ? 'Exiting…' : 'Hold to exit all'}
      </HoldButton>
    </div>
  );
}

/** The rail a scalp shows, by the Desk's rules. */
function railOf(p: ExecutePosition, indexNow: number | null, levelsOn: boolean) {
  return railFor({
    id: p.id,
    contract: contractLabel(p.index, p.strike, p.option_type),
    index: p.index,
    strike: p.strike,
    side: p.option_type,
    source: { kind: 'scalp', title: 'Scalp' },
    state: 'active',
    qty: p.qty,
    lot_size: p.lot_size,
    lots: 0,
    lots_set: false,
    avg: p.avg,
    since: null,
    ltp: p.ltp,
    pnl: p.pnl,
    pnl_pct: null,
    entry: p.avg,
    entry_max: null,
    entry_condition: 'MARKET',
    zone: null,
    valid_till: null,
    stop: 0,
    signal_stop: 0,
    targets: [],
    next_target: null,
    chart: p.chart ?? { stop: null, target: null },
    index_ltp: indexNow,
    busy: null,
  } satisfies DeskPosition, levelsOn);
}

type SheetTab = 'positions' | 'history' | 'activity';

/**
 * Phone: the trading panel as a bottom sheet — the account's figures, then
 * Positions (each trade, its stop and target, −1 lot and Exit), History
 * (today's closed trades) and Activity (fills and engine errors).
 */
export function PositionSheet({ scalps, h, mode, desk, history, fills, errors, indexNow, deltaOf, since, canSet, onRemove, onAdd, onClose, paper }: {
  scalps: ExecutePosition[];
  h: PositionHandlers;
  mode: string;
  desk: DeskAnswer | null;
  /** Today's closed calls, every channel. */
  history: Call[];
  fills: FillMark[];
  errors: EngineError[];
  indexNow: (index: string) => number | null;
  deltaOf: (p: ExecutePosition) => { value: number; assumed: boolean };
  /** When each was first bought today, IST `HH:MM`. */
  since: (p: ExecutePosition) => string | null;
  canSet: boolean;
  onRemove: (p: ExecutePosition, kind: LevelKind) => void;
  /** Put a level on the chart: the sheet closes so the chart can be tapped. */
  onAdd: (p: ExecutePosition, kind: LevelKind) => void;
  onClose: () => void;
  /** A user's paper portfolio: their figures and their own activity. */
  paper?: PaperAccount;
}) {
  const [tab, setTab] = useState<SheetTab>('positions');
  const active = scalps.filter((p) => !p.placing);
  const closed = history.filter((c) => c.channel === 'Scalps');
  return (
    <Modal open onClose={onClose} label="Trading">
      <div className="flex flex-col gap-4 overflow-y-auto px-4 pt-3 pb-4">
        <AccountColumn mode={mode} desk={desk} positions={active} paper={paper} vertical={false} />
        <Segment
          label="Trading panel"
          size="lg"
          stretch
          value={tab}
          onChange={setTab}
          options={[
            { value: 'positions', label: `Positions · ${active.length}` },
            { value: 'history', label: `History · ${closed.length}` },
            { value: 'activity', label: 'Activity' },
          ]}
        />
        {tab === 'history' && <HistoryList calls={closed} large />}
        {tab === 'activity' && (paper ? <PaperActivityList items={paper.activity} large /> : <ActivityList events={activityEvents(fills, errors)} large />)}
        {tab === 'positions' && scalps.length === 0 && <p className="type-body text-fg-muted">No open trade.</p>}
        {tab === 'positions' &&
          scalps.map((p) => {
            const note = busyNote(p, !!h.sending[p.id]);
            const locked = note != null || p.qty <= 0;
            const delta = deltaOf(p);
            const pct = p.ltp != null && p.avg > 0 ? ((p.ltp - p.avg) / p.avg) * 100 : null;
            const lots = p.lot_size > 0 ? Math.round(p.qty / p.lot_size) : 0;
            const bought = since(p);
            const row = (kind: LevelKind) => {
              const lvl = p.chart?.[kind] ?? null;
              const word = kind === 'stop' ? 'Stop' : 'Target';
              return (
                <div className="flex items-center gap-3 border-t border-line py-3">
                  <div className="min-w-0 flex-1">
                    <div className="type-label text-fg-muted">{word}</div>
                    {lvl ? (
                      <div className={`type-body-strong tnum ${kind === 'stop' ? 'text-loss' : 'text-profit'}`}>
                        {levelWords(p, kind, lvl)} · {approxMoney(levelPnl(lvl, p, indexNow(p.index), delta.value))}
                      </div>
                    ) : (
                      <div className="type-body text-fg-muted">None</div>
                    )}
                  </div>
                  {lvl ? (
                    <Button size="lg" kind="ghost" disabled={locked} onClick={() => onRemove(p, kind)}>
                      Remove
                    </Button>
                  ) : (
                    canSet && (
                      <Button size="lg" kind="ghost" disabled={locked} onClick={() => onAdd(p, kind)}>
                        Add {kind}
                      </Button>
                    )
                  )}
                </div>
              );
            };
            return (
              <article key={p.id} className="flex flex-col gap-3">
                <header className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="type-body-strong text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
                      <SourceChip source={{ kind: 'scalp', title: 'Scalp' }} />
                    </div>
                    <div className="mt-1 type-caption text-fg-muted tnum">
                      {[p.expiry ? expiryLabel(p.expiry) : null, `${p.qty} qty (${lots} ${lots === 1 ? 'lot' : 'lots'})`, `avg ${price(p.avg)}`, bought ? `since ${bought}` : null].filter(Boolean).join(' · ')}
                      {note && <span className={p.halted ? 'text-loss' : 'text-fg'}> · {note}</span>}
                    </div>
                  </div>
                  <div className="shrink-0 text-right">
                    <div className={`num-large ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</div>
                    {pct != null && <div className={`type-caption tnum ${TONE_TEXT[toneOf(pct)]}`}>{signedPct(pct)}</div>}
                  </div>
                </header>
                {p.placing ? (
                  mode === 'LIVE' ? (
                    <p className="type-caption text-fg-muted">Buying at market. The engine drops a buy it couldn't send within 15 s.</p>
                  ) : (
                    <Button pointerOnly size="lg" kind="ghost" disabled={!!h.sending[p.id]} onClick={() => h.onCancel(p)}>
                      Cancel the buy
                    </Button>
                  )
                ) : (
                  <>
                    <PositionRail spec={railOf(p, indexNow(p.index), canSet)} />
                    <div>
                      {row('stop')}
                      {row('target')}
                    </div>
                    <p className="type-caption text-fg-muted">
                      Drag the lines on the {p.index} chart to move them. ≈ amounts use{' '}
                      {delta.assumed ? 'an assumed option delta (0.50) — Upstox has none for this contract' : `the option delta (${Math.abs(delta.value).toFixed(2)})`}.
                    </p>
                    <div className="grid grid-cols-2 gap-2">
                      <Button pointerOnly size="lg" kind="loss-outline" disabled={locked || p.qty < p.lot_size} onClick={() => h.onLessLot(p)}>
                        −1 lot ({p.lot_size})
                      </Button>
                      <Button pointerOnly size="lg" kind="loss" disabled={locked} onClick={() => h.onExit(p)}>
                        Exit {p.qty}
                      </Button>
                    </div>
                  </>
                )}
              </article>
            );
          })}
      </div>
    </Modal>
  );
}
