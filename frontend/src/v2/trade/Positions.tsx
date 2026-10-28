import { useState } from 'react';
import type { ChartLevel, ExecutePosition } from '../../types';
import { contractLabel, expiryLabel } from '../../lib/execute';
import { hhmmss, money, price, shortDate, signedMoney, signedPct, todayIst, toneOf } from '../lib/format';
import type { DeskPosition, PaperActivity } from '../lib/types';
import { PositionRail } from '../desk/PositionRail';
import { railFor } from '../desk/rail';
import { Button } from '../ui/Button';
import { SourceChip } from '../ui/Chip';
import { HoldButton } from '../ui/HoldButton';
import { Modal } from '../ui/Modal';
import { Segment } from '../ui/Segment';
import type { LevelKind } from './lines';
import { approxMoney, levelPnl } from './model';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** What a scalp is doing, if anything. */
function busyNote(p: ExecutePosition, sending: boolean): string | null {
  if (p.halted) return 'Halted by the engine — see System';
  if (p.placing) return 'Buying…';
  if (p.adding) return 'Adding…';
  if (p.exiting) return 'Exiting…';
  return sending ? 'Sending…' : null;
}

/** "NIFTY ≤ 25,060.00" or "₹92.50". */
function levelWords(p: ExecutePosition, kind: LevelKind, lvl: ChartLevel): string {
  if (lvl.trigger === 'premium') return `₹${price(lvl.price)}`;
  const falls = (kind === 'stop') === (p.option_type === 'CE');
  return `${p.index} ${falls ? '≤' : '≥'} ${price(lvl.price)}`;
}

/** A user's own paper portfolio, as the Trade screen shows it beside the
 * positions: the wallet, today's P&L and the recent activity. */
export interface PaperAccount {
  balance: number | null;
  /** Booked plus open, today. */
  today: number | null;
  /** Newest first; null until loaded. */
  activity: PaperActivity[] | null;
}

type Tab = 'positions' | 'activity';

const ACTIVITY_TEXT: Record<PaperActivity['kind'], string> = { buy: 'text-fg', sell: 'text-fg', levels: 'text-fg-muted', reset: 'text-fg-muted' };

/** "10:42:05", or "27 Oct 10:42:05" on another day. */
function when(ts: string): string {
  return ts.startsWith(todayIst()) ? hhmmss(ts) : `${shortDate(ts.slice(0, 10))} ${hhmmss(ts)}`;
}

/** The user's last fills, levels and resets, newest first. */
function ActivityList({ items }: { items: PaperActivity[] | null }) {
  if (items == null) return <p className="py-2 type-body text-fg-muted">Loading…</p>;
  if (items.length === 0) {
    return <p className="py-2 type-body text-fg-muted">Nothing yet. Buys, sells, stops and targets show here until the server next restarts.</p>;
  }
  return (
    <div className="flex flex-col">
      {items.map((a, i) => (
        <div key={`${a.t}-${i}`} className="flex items-baseline gap-3 border-t border-line py-1.5">
          <span className="min-w-[64px] shrink-0 whitespace-nowrap type-caption text-fg-muted tnum">{when(a.t)}</span>
          <span className={`min-w-0 type-body ${ACTIVITY_TEXT[a.kind]}`}>{a.message}</span>
        </div>
      ))}
    </div>
  );
}

/** "Wallet ₹9,88,000.00 · today +₹1,500.00". */
function WalletLine({ paper }: { paper: PaperAccount }) {
  return (
    <span className="type-caption text-fg-muted tnum">
      Wallet {paper.balance == null ? '—' : money(paper.balance)} · today{' '}
      <span className={TONE_TEXT[toneOf(paper.today)]}>{paper.today == null ? '—' : signedMoney(paper.today)}</span>
    </span>
  );
}

export interface PositionHandlers {
  /** Positions with a sell in flight from here. */
  sending: Record<string, boolean>;
  onLessLot: (p: ExecutePosition) => void;
  onExit: (p: ExecutePosition) => void;
  onExitAll: () => void;
  exitAllBusy: boolean;
}

/** Desktop: the open manual trades, each with its stop and target (added or
 * removed from here, dragged on the charts), −1 lot and Exit; Exit all held. */
export function ScalpPositionsPanel({ scalps, h, canSet, onAdd, onRemove, paper }: {
  scalps: ExecutePosition[];
  h: PositionHandlers;
  /** Levels can be set (`chart_stops` on); removing works either way. */
  canSet: boolean;
  /** "+ Stop": put the level on a chart with the next click. */
  onAdd: (p: ExecutePosition, kind: LevelKind) => void;
  onRemove: (p: ExecutePosition, kind: LevelKind) => void;
  /** A user's paper wallet and activity; the admin's desk has none. */
  paper?: PaperAccount;
}) {
  const [tab, setTab] = useState<Tab>('positions');
  const open = scalps.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  const showActivity = !!paper && tab === 'activity';
  return (
    <section className="shrink-0 rounded-12 border border-line bg-panel px-4 py-3">
      <header className="flex items-center gap-3 pb-1">
        {paper ? (
          <Segment
            label="Show"
            size="sm"
            value={tab}
            onChange={setTab}
            options={[
              { value: 'positions', label: `Positions ${scalps.length}` },
              { value: 'activity', label: 'Activity' },
            ]}
          />
        ) : (
          <h2 className="flex items-baseline gap-2 type-title text-fg">
            Positions <span className="type-caption text-fg-muted tnum">{scalps.length}</span>
          </h2>
        )}
        <span className="flex-1" />
        {paper && (
          <>
            <WalletLine paper={paper} />
            <span aria-hidden className="h-5 w-px bg-line" />
          </>
        )}
        <span className="type-label text-fg-muted">Open</span>
        <span className={`num-medium ${TONE_TEXT[toneOf(open)]}`}>{signedMoney(open)}</span>
        <HoldButton size="sm" disabled={scalps.length === 0 || h.exitAllBusy} onFire={h.onExitAll}>
          {h.exitAllBusy ? 'Exiting…' : 'Hold to exit all'}
        </HoldButton>
      </header>
      {showActivity && (
        <div className="max-h-[220px] overflow-y-auto">
          <ActivityList items={paper.activity} />
        </div>
      )}
      {!showActivity && scalps.length === 0 && <p className="py-2 type-body text-fg-muted">No open trade. Tap Buy on the ladder to open one.</p>}
      {!showActivity && scalps.map((p) => {
        const note = busyNote(p, !!h.sending[p.id]);
        const locked = note != null || p.qty <= 0;
        const meta = [p.expiry ? expiryLabel(p.expiry) : null, `${p.qty} qty`, `avg ${price(p.avg)}`].filter(Boolean).join(' · ');
        const level = (kind: LevelKind) => {
          const lvl = p.chart?.[kind] ?? null;
          const word = kind === 'stop' ? 'Stop' : 'Target';
          if (lvl) {
            return (
              <span className={`inline-flex items-center gap-1 type-caption tnum ${kind === 'stop' ? 'text-loss' : 'text-profit'}`}>
                {word.toLowerCase()} {levelWords(p, kind, lvl)}
                <button type="button" disabled={locked} title={`Remove the ${kind}`} aria-label={`Remove the ${kind}`} onClick={() => onRemove(p, kind)} className="grid size-5 place-items-center rounded-4 border border-current/40 hover:bg-raised disabled:opacity-40">
                  ×
                </button>
              </span>
            );
          }
          if (!canSet) return <span className="type-caption text-fg-muted">no {kind}</span>;
          return (
            <button type="button" disabled={locked} onClick={() => onAdd(p, kind)} className={`type-caption hover:underline disabled:opacity-40 ${kind === 'stop' ? 'text-loss' : 'text-profit'}`}>
              + {word}
            </button>
          );
        };
        return (
          <div key={p.id} className="flex items-center gap-6 border-t border-line py-2.5">
            <span className="w-44 shrink-0 type-body-strong text-fg">{contractLabel(p.index, p.strike, p.option_type)}</span>
            <span className="flex min-w-0 flex-1 items-center gap-3 type-caption text-fg-muted tnum">
              <span className="truncate">
                {meta}
                {note && <span className={p.halted ? 'text-loss' : 'text-fg'}> · {note}</span>}
              </span>
              {level('stop')}
              {level('target')}
            </span>
            <span className={`w-28 shrink-0 text-right num-medium ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</span>
            <div className="flex shrink-0 gap-2">
              <Button pointerOnly kind="loss-outline" disabled={locked || p.qty < p.lot_size} onClick={() => h.onLessLot(p)} title={`Sell ${p.lot_size} at market`}>
                −1 lot
              </Button>
              <Button pointerOnly kind="loss" disabled={locked} onClick={() => h.onExit(p)} title={`Sell all ${p.qty} at market`}>
                Exit
              </Button>
            </div>
          </div>
        );
      })}
    </section>
  );
}

/** Phone: the bar under the ladder — open P&L (and a user's wallet), and
 * Exit all held. A tap opens the sheet; a user's opens even with nothing
 * held, for the activity. */
export function ScalpBar({ scalps, h, onOpen, paper }: { scalps: ExecutePosition[]; h: PositionHandlers; onOpen: () => void; paper?: PaperAccount }) {
  const open = scalps.reduce((sum, p) => sum + (p.pnl ?? 0), 0);
  return (
    <div className="flex shrink-0 items-center gap-3 border-t border-line bg-inset px-4 py-2">
      <button type="button" onClick={onOpen} disabled={scalps.length === 0 && !paper} className="min-w-0 flex-1 text-left disabled:opacity-60">
        <div className="type-caption text-fg-muted">Open · {scalps.length === 1 ? '1 trade' : `${scalps.length} trades`}</div>
        <div className={`num-medium ${TONE_TEXT[toneOf(open)]}`}>{signedMoney(open)}</div>
        {paper && <WalletLine paper={paper} />}
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

/** Figma "Scalp · position sheet": each scalp, its stop and target, −1 lot and Exit. */
export function PositionSheet({ scalps, h, indexNow, deltaOf, since, canSet, onRemove, onAdd, onClose, paper }: {
  scalps: ExecutePosition[];
  h: PositionHandlers;
  indexNow: (index: string) => number | null;
  deltaOf: (p: ExecutePosition) => { value: number; assumed: boolean };
  /** When each was first bought today, IST `HH:MM`. */
  since: (p: ExecutePosition) => string | null;
  canSet: boolean;
  onRemove: (p: ExecutePosition, kind: LevelKind) => void;
  /** Put a level on the chart: the sheet closes so the chart can be tapped. */
  onAdd: (p: ExecutePosition, kind: LevelKind) => void;
  onClose: () => void;
  /** A user's paper wallet and activity; the admin's desk has none. */
  paper?: PaperAccount;
}) {
  const [tab, setTab] = useState<Tab>('positions');
  const showActivity = !!paper && tab === 'activity';
  return (
    <Modal open onClose={onClose} label="Positions">
      <div className="flex flex-col gap-5 overflow-y-auto px-4 pt-3 pb-4">
        {paper && (
          <div className="flex flex-col gap-2">
            <Segment
              label="Show"
              size="lg"
              stretch
              value={tab}
              onChange={setTab}
              options={[
                { value: 'positions', label: `Positions ${scalps.length}` },
                { value: 'activity', label: 'Activity' },
              ]}
            />
            <WalletLine paper={paper} />
          </div>
        )}
        {showActivity && <ActivityList items={paper.activity} />}
        {!showActivity && scalps.length === 0 && <p className="type-body text-fg-muted">No open trade.</p>}
        {!showActivity && scalps.map((p) => {
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
            </article>
          );
        })}
      </div>
    </Modal>
  );
}
