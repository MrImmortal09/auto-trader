import { useState, type ReactNode } from 'react';
import { ChevronRight, X } from 'lucide-react';
import { getHistory, getMyHistory } from '../lib/api';
import { downloadCsv } from '../lib/csv';
import { dateRange, dayLabel, indexName, pct, plural, signedMoney, toneOf } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { Call, GroupId, GroupRow, HistoryAnswer, RangeId } from '../lib/types';
import { Button } from '../ui/Button';
import { Segment } from '../ui/Segment';
import { Stat } from '../ui/Stat';
import { PnlChart } from './PnlChart';
import { CallRow } from './Rows';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

const RANGE_NAME: Record<Exclude<RangeId, 'today'>, string> = { '7d': '7 days', '30d': '30 days', all: 'all time' };

/** A row that opens its calls: a hairline under it, tinted on hover. */
const PICK_ROW = 'border-b border-line text-left transition-colors duration-120 hover:bg-raised/40';

/** A source the way the table names it: the Execute screen's scalps are yours. */
function sourceName(label: string, group: GroupId): string {
  if (group === 'channel' && label === 'Scalps') return 'Scalp (you)';
  return label;
}

/** The calls a source row or a day row stands for. */
type Drill = { kind: 'source'; label: string } | { kind: 'day'; date: string };

function matches(c: Call, pick: Drill, group: GroupId): boolean {
  if (pick.kind === 'day') return c.closed_at?.slice(0, 10) === pick.date;
  if (group === 'channel') return c.channel === pick.label;
  if (group === 'index') return c.index === pick.label;
  return (c.side ?? 'Other') === pick.label;
}

/** The range's calls as a CSV download. */
function exportCsv(h: HistoryAnswer) {
  const head = ['Closed', 'Opened', 'Channel', 'Index', 'Strike', 'Side', 'Bought qty', 'Sold qty', 'Net P&L', 'Charges', 'How it ended'];
  const rows = h.calls.map((c) => [c.closed_at, c.opened_at, c.channel, c.index, c.strike, c.side, c.bought_qty, c.sold_qty, c.pnl == null ? null : c.pnl.toFixed(2), c.charges.toFixed(2), c.outcome?.label ?? c.exit_reason]);
  downloadCsv(`auto-trader-${h.mode.toLowerCase()}-${h.range}-${h.to}.csv`, [head, ...rows]);
}

/** A white card with a hairline; `bare` drops its fill and padding (phone's By source list). */
function Card({ children, className = '', bare = false }: { children: ReactNode; className?: string; bare?: boolean }) {
  return <section className={`flex min-w-0 flex-col gap-3 ${bare ? '' : 'rounded-12 border border-line bg-panel p-4'} ${className}`}>{children}</section>;
}

function SourceTable({ groups, group, rangeName, onPick, desktop }: {
  groups: GroupRow[];
  group: GroupId;
  rangeName: string;
  onPick: (label: string) => void;
  desktop: boolean;
}) {
  if (groups.length === 0) return <p className="py-2 type-body text-fg-subtle">No closed calls in this range.</p>;
  if (!desktop) {
    return (
      <div className="flex flex-col">
        {groups.map((g) => {
          const losing = g.net < 0;
          return (
            <button key={g.label} type="button" onClick={() => onPick(g.label)} className={`flex min-h-14 items-center gap-3 py-3 ${PICK_ROW}`}>
              <div className="min-w-0 flex-1">
                <div className="type-body-strong text-fg">{sourceName(g.label, group)}</div>
                <div className={`mt-0.5 type-caption tnum ${losing ? 'text-loss' : 'text-fg-subtle'}`}>
                  {[plural(g.calls, group === 'channel' && g.label === 'Scalps' ? 'trade' : 'call'), `${pct((g.wins / g.calls) * 100)} win`, `PF ${g.profit_factor == null ? '—' : g.profit_factor.toFixed(2)}`, losing ? 'losing money' : null]
                    .filter(Boolean)
                    .join(' · ')}
                </div>
              </div>
              <span className={`num-medium ${TONE_TEXT[toneOf(g.net)]}`}>{signedMoney(g.net)}</span>
              <ChevronRight size={16} className="shrink-0 text-fg-muted" />
            </button>
          );
        })}
      </div>
    );
  }
  return (
    <table className="w-full">
      <thead>
        <tr className="border-b border-line text-fg-muted">
          <th className="py-2 text-left type-eyebrow">Source</th>
          <th className="py-2 text-right type-eyebrow">Calls</th>
          <th className="py-2 text-right type-eyebrow">Win</th>
          <th className="py-2 text-right type-eyebrow">PF</th>
          <th className="py-2 text-right type-eyebrow">Net</th>
        </tr>
      </thead>
      <tbody>
        {groups.map((g) => {
          const losing = g.net < 0;
          return (
            <tr key={g.label} onClick={() => onPick(g.label)} className={`cursor-pointer ${PICK_ROW}`}>
              <td className="py-2.5">
                <div className="type-body-strong text-fg">{sourceName(g.label, group)}</div>
                {losing && <div className="type-caption text-loss">Losing money over {rangeName}</div>}
              </td>
              <td className="py-2.5 text-right num-medium text-fg">{g.calls}</td>
              <td className="py-2.5 text-right num-medium text-fg">{pct((g.wins / g.calls) * 100)}</td>
              <td className={`py-2.5 text-right num-medium ${g.profit_factor != null && g.profit_factor < 1 ? 'text-loss' : 'text-fg'}`}>
                {g.profit_factor == null ? '—' : g.profit_factor.toFixed(2)}
              </td>
              <td className={`py-2.5 text-right num-medium ${TONE_TEXT[toneOf(g.net)]}`}>{signedMoney(g.net)}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/** Desk over 7D, 30D or All: the range's KPIs, P&L curve, P&L by source and
 * its trading days — the owner's, or with `paper` the user's own. */
export function DeskRange({ serverBase, range, desktop, rangeSwitch, paper = false }: {
  serverBase: string;
  range: Exclude<RangeId, 'today'>;
  desktop: boolean;
  rangeSwitch: ReactNode;
  paper?: boolean;
}) {
  const [group, setGroup] = useState<GroupId>('channel');
  const [pick, setPick] = useState<Drill | null>(null);
  const history = usePoll(
    () => (paper ? getMyHistory(serverBase, range, group) : getHistory(serverBase, range, group)),
    30_000,
    `${serverBase}|${range}|${group}|${paper ? 'paper' : 'owner'}`,
  );
  const h = history.data;
  const k = h?.kpis;
  const rangeName = RANGE_NAME[range];
  const span = h ? (h.from ? dateRange(h.from, h.to) : h.days.length ? dateRange(h.days[h.days.length - 1].date, h.to) : '') : '';
  const maxDay = Math.max(1, ...(h?.days.map((d) => Math.abs(d.net)) ?? []));
  const picked = h && pick ? h.calls.filter((c) => matches(c, pick, group)) : [];

  const groupSwitch = (
    <Segment
      label="Group by"
      value={group}
      onChange={(g) => {
        setGroup(g);
        setPick(null);
      }}
      options={[
        { value: 'channel', label: 'Channel' },
        { value: 'index', label: 'Index' },
        { value: 'cepe', label: 'CE / PE' },
      ]}
    />
  );

  const header = desktop ? (
    <div className="flex items-center gap-4">
      {rangeSwitch}
      {h && <span className="type-caption text-fg-subtle tnum">{[span, plural(h.days.length, 'trading day')].filter(Boolean).join(' · ')}</span>}
      <span className="flex-1" />
      <Button size="md" disabled={!h || h.calls.length === 0} onClick={() => h && exportCsv(h)} title={h && h.more_calls > 0 ? `Exports the newest ${h.calls.length} calls` : undefined}>
        Export CSV
      </Button>
    </div>
  ) : (
    rangeSwitch
  );

  const kpis = k && (
    desktop ? (
      <div className="flex flex-wrap items-end gap-x-8 gap-y-4 border-b border-line pb-5">
        <Stat label={`Net · ${rangeName}`} value={signedMoney(k.net)} size="hero" tone={toneOf(k.net)} />
        <Stat label="Win rate" value={k.win_rate == null ? '—' : pct(k.win_rate)} />
        <Stat label="Profit factor" value={k.profit_factor == null ? '—' : k.profit_factor.toFixed(2)} tone={k.profit_factor != null && k.profit_factor < 1 ? 'loss' : 'neutral'} />
        <Stat label="Max drawdown" value={signedMoney(-k.max_drawdown)} tone={k.max_drawdown > 0 ? 'loss' : 'neutral'} />
        <Stat label="Calls" value={k.calls} />
        <Stat label="Average per call" value={k.avg_per_call == null ? '—' : signedMoney(k.avg_per_call)} tone={toneOf(k.avg_per_call)} />
      </div>
    ) : (
      <div className="flex flex-col gap-3">
        <Stat label={span ? `Net · ${span}` : `Net · ${rangeName}`} value={signedMoney(k.net)} size="hero" tone={toneOf(k.net)} />
        {/* Two by two on a phone: four 83 px columns can't hold "MAX DRAWDOWN" over "−₹5,295.67" in Inter. */}
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-4 sm:gap-2">
          <Stat label="Win rate" value={k.win_rate == null ? '—' : pct(k.win_rate)} size="medium" />
          <Stat label="Profit factor" value={k.profit_factor == null ? '—' : k.profit_factor.toFixed(2)} size="medium" />
          <Stat label="Max drawdown" value={signedMoney(-k.max_drawdown)} size="medium" tone={k.max_drawdown > 0 ? 'loss' : 'neutral'} />
          <Stat label="Calls" value={k.calls} size="medium" />
        </div>
      </div>
    )
  );

  const chart = h && (
    <Card className={desktop ? 'lg:col-span-3' : ''}>
      <div className="flex items-baseline gap-2">
        <h2 className="type-title text-fg">Cumulative P&amp;L</h2>
        {desktop && <span className="type-caption text-fg-subtle">Realised, after charges</span>}
      </div>
      <PnlChart days={h.days} curve={h.curve} maxDrawdown={h.kpis.max_drawdown} height={desktop ? 270 : 170} compact={!desktop} />
    </Card>
  );

  const sources = h && (
    <Card className={desktop ? 'lg:col-span-2' : ''} bare={!desktop}>
      <div className="flex items-center gap-3">
        <h2 className="type-title text-fg">By source</h2>
        <span className="flex-1" />
        {groupSwitch}
      </div>
      <SourceTable groups={h.groups} group={group} rangeName={rangeName} desktop={desktop} onPick={(label) => setPick({ kind: 'source', label })} />
      {desktop && h.groups.length > 0 && <p className="type-caption text-fg-subtle">Net after charges. Select a source to see its calls.</p>}
    </Card>
  );

  const days = h && (
    <section className="flex flex-col">
      <h2 className="mb-1 type-title text-fg">Days</h2>
      {h.days.length === 0 && <p className="py-2 type-body text-fg-subtle">No closed calls in this range.</p>}
      {h.days.map((d) => {
        const tone = toneOf(d.net);
        const note = [plural(d.calls, 'call'), d.expiries.length ? `${d.expiries.map(indexName).join(', ')} expiry` : null].filter(Boolean).join(' · ');
        return (
          <button key={d.date} type="button" onClick={() => setPick({ kind: 'day', date: d.date })} className={`flex min-h-12 items-center gap-4 py-3 ${PICK_ROW}`}>
            <span className={`shrink-0 type-body-strong text-fg ${desktop ? 'w-36' : 'w-24'}`}>{dayLabel(d.date)}</span>
            <span className={`shrink-0 type-caption text-fg-subtle ${desktop ? 'w-44' : 'min-w-0 flex-1 truncate'}`}>{note}</span>
            {desktop && (
              <span className="flex-1">
                {/* The day's net against the range's biggest day, across the free span. */}
                <span
                  className={`block h-1 rounded-full ${tone === 'loss' ? 'bg-loss' : 'bg-profit'}`}
                  style={{ width: `${Math.max(2, (Math.abs(d.net) / maxDay) * 100)}%`, minWidth: 4 }}
                />
              </span>
            )}
            <span className={`num-medium ${TONE_TEXT[tone]}`}>{signedMoney(d.net)}</span>
            <ChevronRight size={16} className="shrink-0 text-fg-muted" />
          </button>
        );
      })}
    </section>
  );

  const pickedList = h && pick && (
    <section className="flex flex-col">
      <div className="mb-1 flex items-center gap-2">
        <h2 className="type-title text-fg">{pick.kind === 'day' ? dayLabel(pick.date) : sourceName(pick.label, group)}</h2>
        <span className="type-caption text-fg-subtle tnum">{plural(picked.length, 'call')}</span>
        <button
          type="button"
          aria-label="Show all"
          onClick={() => setPick(null)}
          className="ml-auto grid size-7 place-items-center rounded-6 text-fg-muted transition-colors duration-120 hover:bg-raised hover:text-fg"
        >
          <X size={16} />
        </button>
      </div>
      {picked.map((c) => (
        <CallRow key={c.key} call={c} desktop={desktop} />
      ))}
      {h.more_calls > 0 && <p className="pt-2 type-caption text-fg-subtle">From the newest {h.calls.length} calls in this range.</p>}
      {picked.length === 0 && <p className="py-2 type-body text-fg-subtle">None of the newest {h.calls.length} calls.</p>}
    </section>
  );

  const loading = !h && (
    <p className={`type-body ${history.error ? 'text-loss' : 'text-fg-subtle'}`}>{history.error ? `Couldn't load the history: ${history.error}` : 'Loading…'}</p>
  );

  return (
    <div className={`flex flex-col gap-5 ${desktop ? 'px-6 py-5' : 'px-4 py-4'}`}>
      {header}
      {kpis}
      {loading}
      {h && (desktop ? (
        <div className="grid gap-4 lg:grid-cols-5">
          {chart}
          {sources}
        </div>
      ) : (
        <>
          {chart}
          {sources}
        </>
      ))}
      {pickedList}
      {days}
    </div>
  );
}
