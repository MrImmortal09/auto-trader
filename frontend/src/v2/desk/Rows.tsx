import { useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { hhmm, level, money, price, signedMoney, signedPct, toneOf } from '../lib/format';
import { buyLabel, callContract, grossOf, reasonLabel, sourceOfChannel } from '../lib/labels';
import type { Call, ClosedCall, DeskPosition } from '../lib/types';
import { Button } from '../ui/Button';
import { SourceChip, ToneChip } from '../ui/Chip';
import { LotsStepper } from '../ui/LotsStepper';
import { PositionRail } from './PositionRail';
import { railFor } from './rail';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

const BUSY: Record<NonNullable<DeskPosition['busy']>, string> = {
  exiting: 'Exiting…',
  selling: 'Selling…',
  buying: 'Buying…',
  halted: 'Halted by the engine — see System',
};

/** What's happening to a row right now, if anything. */
function busyNote(p: DeskPosition, sending: boolean): string | null {
  if (p.busy) return BUSY[p.busy];
  return sending ? 'Sending…' : null;
}

function openMeta(p: DeskPosition): string {
  return [`${p.qty} qty`, `avg ${price(p.avg)}`, p.since ? `since ${p.since}` : null].filter(Boolean).join(' · ');
}

// ── Figma "Position row / Desktop" and "Position card / Phone" ────────── //

export function OpenRow({ p, desktop, sending, levelsOn, onLessLot, onExit }: {
  p: DeskPosition;
  desktop: boolean;
  /** An action on it is in flight. */
  sending: boolean;
  /** A scalp's stop and target can be set from the charts. */
  levelsOn: boolean;
  onLessLot: () => void;
  onExit: () => void;
}) {
  const tone = toneOf(p.pnl);
  const note = busyNote(p, sending);
  const locked = sending || p.busy != null;
  // −1 lot sells exactly one lot: the least the exchange takes.
  const lessLot = (
    <Button
      pointerOnly
      kind="loss-outline"
      size={desktop ? 'sm' : 'lg'}
      disabled={locked || p.qty < p.lot_size}
      onClick={onLessLot}
      title={`Sell ${p.lot_size} at market`}
      className={desktop ? '' : 'w-full'}
    >
      −1 lot
    </Button>
  );
  const exit = (
    <Button pointerOnly kind="loss" size={desktop ? 'sm' : 'lg'} disabled={locked} onClick={onExit} title={`Sell all ${p.qty} at market`} className={desktop ? '' : 'w-full'}>
      Exit
    </Button>
  );
  const pnl = (
    <div className="shrink-0 text-right">
      <div className={`num-large ${TONE_TEXT[tone]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</div>
      <div className={`type-caption tnum ${TONE_TEXT[tone]}`}>{p.pnl_pct == null ? 'No price yet' : signedPct(p.pnl_pct)}</div>
    </div>
  );
  const head = (
    <div className="min-w-0">
      <div className="flex flex-wrap items-center gap-2">
        <span className="type-body-strong text-fg">{p.contract}</span>
        <SourceChip source={p.source} />
      </div>
      <div className="mt-1 type-caption text-fg-muted tnum">
        {openMeta(p)}
        {note && <span className={p.busy === 'halted' ? 'text-loss' : 'text-fg'}> · {note}</span>}
      </div>
    </div>
  );

  if (desktop) {
    return (
      <div className="flex items-center gap-6 border-b border-line py-3.5">
        <div className="w-[260px] shrink-0">{head}</div>
        <div className="min-w-[200px] max-w-[440px] flex-1">
          <PositionRail spec={railFor(p, levelsOn)} />
        </div>
        <div className="ml-auto w-[130px]">{pnl}</div>
        <div className="flex shrink-0 gap-2">
          {lessLot}
          {exit}
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-3 rounded-12 bg-panel p-4">
      <div className="flex items-start justify-between gap-3">
        {head}
        {pnl}
      </div>
      <PositionRail spec={railFor(p, levelsOn)} />
      <div className="grid grid-cols-2 gap-2">
        {lessLot}
        {exit}
      </div>
    </div>
  );
}

// ── Figma "Waiting row / Desktop" and "Waiting card / Phone" ──────────── //

/** "Buys above 210 · zone 210–214 · valid till 12:10". */
function waitingLine(p: DeskPosition): string {
  const parts: string[] = [];
  if (p.entry_condition.toUpperCase() === 'MARKET') parts.push('Buys at market');
  else if (p.entry_max != null) parts.push(`Buys ${level(p.entry)}–${level(p.entry_max)}`);
  else parts.push(`Buys ${p.entry_condition.toLowerCase()} ${level(p.entry)}`);
  if (p.zone) parts.push(`zone ${level(p.zone[0])}–${level(p.zone[1])}`);
  if (p.valid_till) parts.push(`valid till ${p.valid_till}`);
  return parts.join(' · ');
}

export function WaitingRow({ p, desktop, lots, sending, onLots, onCancel }: {
  p: DeskPosition;
  desktop: boolean;
  /** The lots shown: one just set, or what the call will buy. */
  lots: number;
  sending: boolean;
  onLots: (lots: number) => void;
  onCancel: () => void;
}) {
  const note = busyNote(p, sending);
  const locked = sending || p.busy != null;
  const head = (
    <div className="min-w-0">
      <div className="flex flex-wrap items-center gap-2">
        <span className="type-body-strong text-fg">{p.contract}</span>
        <SourceChip source={p.source} />
      </div>
      <div className="mt-1 type-caption text-fg-muted tnum">
        {waitingLine(p)}
        {note && <span className="text-fg"> · {note}</span>}
      </div>
    </div>
  );
  // A scalp's first buy is already at the broker: nothing to size or cancel.
  const scalp = p.source.kind === 'scalp';
  const stepper = !scalp && <LotsStepper lots={lots} lotSize={p.lot_size} onChange={onLots} disabled={locked} size={desktop ? 'sm' : 'lg'} />;
  const cancel = !scalp && (
    <Button size={desktop ? 'sm' : 'lg'} disabled={locked} onClick={onCancel} className={desktop ? '' : 'flex-1'}>
      Cancel
    </Button>
  );

  if (desktop) {
    return (
      <div className="flex items-center gap-6 border-b border-line py-3.5">
        <div className="w-[260px] shrink-0">{head}</div>
        <div className="min-w-[200px] max-w-[440px] flex-1">
          <PositionRail spec={railFor(p)} />
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-4">
          {stepper}
          {cancel}
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-3 rounded-12 bg-panel p-4">
      {head}
      <PositionRail spec={railFor(p)} />
      {!scalp && (
        <div className="flex items-center gap-3">
          {stepper}
          {cancel}
        </div>
      )}
    </div>
  );
}

// ── Figma "Call row / Desktop" and "/ Phone" ──────────────────────────── //

function CallDetail({ call, messageAt, desktop }: { call: Call; messageAt: string | null; desktop: boolean }) {
  const scalp = call.channel === 'Scalps';
  const gross = grossOf(call);
  const legs = (
    <div className="min-w-0">
      <table className="w-full type-caption tnum">
        <thead className="text-fg-muted">
          <tr className="text-left">
            <th className="pb-1.5 font-normal">Time</th>
            <th className="pb-1.5 font-normal">Side</th>
            <th className="pb-1.5 font-normal">Qty</th>
            <th className="pb-1.5 font-normal">Price</th>
            <th className="pb-1.5 font-normal">Reason</th>
          </tr>
        </thead>
        <tbody className="text-fg">
          {call.legs.map((l) => {
            const buy = l.action.toUpperCase() === 'BUY';
            return (
              <tr key={l.id}>
                <td className="py-1 pr-3">{hhmm(l.t)}</td>
                <td className={`py-1 pr-3 ${buy ? 'text-profit' : 'text-loss'}`}>{buy ? 'Buy' : 'Sell'}</td>
                <td className="py-1 pr-3">{l.qty}</td>
                <td className="py-1 pr-3">{price(l.price)}</td>
                <td className="py-1">{buy ? buyLabel(l, call.message, scalp) : reasonLabel(l.reason)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="mt-2 type-caption text-fg-muted tnum">
        Gross {signedMoney(gross)} · charges {money(call.charges)} · net {call.pnl == null ? '—' : signedMoney(call.pnl)}
      </p>
    </div>
  );
  const message = call.message && (
    <div className="min-w-0">
      <div className="mb-1.5 type-caption text-fg-muted">
        Original message · {scalp ? 'Scalp' : call.channel}
        {messageAt ? ` · ${hhmm(messageAt)}` : ''}
      </div>
      <div className="whitespace-pre-wrap rounded-8 bg-inset px-3 py-2.5 type-body text-fg">{call.message}</div>
    </div>
  );
  return <div className={`pb-4 ${desktop ? 'grid grid-cols-2 gap-8 pl-14' : 'flex flex-col gap-3'}`}>{legs}{message}</div>;
}

export function CallRow({ call, desktop }: { call: Call | ClosedCall; desktop: boolean }) {
  const [open, setOpen] = useState(false);
  const tone = toneOf(call.pnl);
  const source = sourceOfChannel(call.channel);
  const pnl = <span className={`num-medium ${TONE_TEXT[tone]}`}>{call.pnl == null ? '—' : signedMoney(call.pnl)}</span>;
  const Chevron = open ? ChevronDown : ChevronRight;
  const messageAt = 'message_at' in call ? call.message_at : null;

  return (
    <div className="border-b border-line">
      {desktop ? (
        <button type="button" aria-expanded={open} onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-4 py-3 text-left">
          <span className="w-10 shrink-0 num-small text-fg-muted">{hhmm(call.opened_at)}</span>
          <span className="type-body-strong text-fg">{callContract(call)}</span>
          <SourceChip source={source} />
          {call.outcome && <ToneChip tone={call.outcome.tone}>{call.outcome.label}</ToneChip>}
          <span className="flex-1" />
          {pnl}
          <Chevron size={16} className="shrink-0 text-fg-muted" />
        </button>
      ) : (
        <button type="button" aria-expanded={open} onClick={() => setOpen((o) => !o)} className="flex min-h-14 w-full items-center gap-3 py-3 text-left">
          <div className="min-w-0 flex-1">
            <div className="type-body-strong text-fg">{callContract(call)}</div>
            <div className="mt-0.5 truncate type-caption text-fg-muted tnum">
              {[hhmm(call.opened_at), call.outcome?.label, source.title].filter(Boolean).join(' · ')}
            </div>
          </div>
          {pnl}
          <Chevron size={16} className="shrink-0 text-fg-muted" />
        </button>
      )}
      {open && <CallDetail call={call} messageAt={messageAt} desktop={desktop} />}
    </div>
  );
}
