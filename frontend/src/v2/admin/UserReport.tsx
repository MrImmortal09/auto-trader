import { Fragment, useEffect, useRef, useState, type ReactNode } from 'react';
import { ArrowLeft, ChevronDown, ChevronRight, Download } from 'lucide-react';
import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { formatPhone } from '../auth/phone';
import { PnlChart } from '../desk/PnlChart';
import { getUserReport } from '../lib/api';
import { downloadCsv } from '../lib/csv';
import { axisMoney, dateRange, dayLabel, hhmm, hhmmss, indexName, money, pct, plural, price, shortDate, signedMoney, todayIst, toneOf } from '../lib/format';
import { reasonLabel } from '../lib/labels';
import { usePoll } from '../lib/usePoll';
import type { AdminUser, CurvePoint, LegRow, ReportDay, ReportTrade, UserReport as Report } from '../lib/types';
import { Button } from '../ui/Button';
import { Chip, ToneChip } from '../ui/Chip';
import { Segment } from '../ui/Segment';
import { Stat } from '../ui/Stat';
import { PaperLine } from './PaperActivity';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;
const DATE_INPUT = 'min-w-0 rounded-6 border border-line bg-inset px-2 type-body text-fg outline-none focus:border-line-strong tnum';
// The design's tokens as plain values: SVG attributes can't read CSS variables.
const C = { profit: '#22b47f', loss: '#ee705c', grid: '#2c3442', muted: '#8e97a7', raised: '#2c3442', line: '#353e4d' };

// `custom`: the dates typed in, with no preset lit.
type Preset = 'today' | '7d' | '30d' | 'all' | 'custom';
const PRESETS: { value: Preset; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '7d', label: '7D' },
  { value: '30d', label: '30D' },
  { value: 'all', label: 'All' },
];

type Tab = 'days' | 'fills' | 'contracts' | 'timing';
const TABS: { value: Tab; label: string }[] = [
  { value: 'days', label: 'Day by day' },
  { value: 'fills', label: 'Tradebook' },
  { value: 'contracts', label: 'By contract' },
  { value: 'timing', label: 'Timing' },
];
// The phone's tabs share 328 pt.
const PHONE_TABS: { value: Tab; label: string }[] = [
  { value: 'days', label: 'Days' },
  { value: 'fills', label: 'Fills' },
  { value: 'contracts', label: 'Contracts' },
  { value: 'timing', label: 'Hours' },
];

/** IST days, `YYYY-MM-DD`; `from` null runs from the first trade. */
interface Span {
  from: string | null;
  to: string;
}

/** `day` less `n` days. */
function daysBefore(day: string, n: number): string {
  const d = new Date(`${day}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() - n);
  return d.toISOString().slice(0, 10);
}

/** The days a preset covers: 7D and 30D include today. */
function presetSpan(p: Exclude<Preset, 'custom'>, today: string): Span {
  if (p === 'today') return { from: today, to: today };
  if (p === '7d') return { from: daysBefore(today, 6), to: today };
  if (p === '30d') return { from: daysBefore(today, 29), to: today };
  return { from: null, to: today };
}

const isBuy = (l: { action: string }) => l.action.toUpperCase() === 'BUY';
const nameOf = (u: AdminUser) => u.name?.trim() || formatPhone(u.phone);

/** "45s", "4m 05s", "2h 05m", "1d 3h". */
function held(secs: number | null | undefined): string {
  if (secs == null) return '—';
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

/** "09:20:05" on `day`; "27 Oct 09:20" on another. */
function timeOn(ts: string, day: string): string {
  return ts.startsWith(day) ? hhmmss(ts) : `${shortDate(ts.slice(0, 10))} ${hhmm(ts)}`;
}

/** Each sell's P&L by the server's rule: its net proceeds less the same
 * quantity at the trade's average buy cost, charges included. */
function sellPnls(legs: LegRow[]): Map<number, number> {
  const buys = legs.filter(isBuy);
  const qty = buys.reduce((a, l) => a + l.qty, 0);
  const out = new Map<number, number>();
  if (qty <= 0) return out;
  const cost = buys.reduce((a, l) => a + l.net, 0) / qty;
  for (const l of legs) if (!isBuy(l)) out.set(l.id, l.net - l.qty * cost);
  return out;
}

const soldOn = (t: ReportTrade, day: string) => t.legs.some((l) => !isBuy(l) && l.t.startsWith(day));
const spansDays = (t: ReportTrade, day: string) => t.legs.some((l) => !l.t.startsWith(day));

/** "150", or "150 · 75 sold" while part is held. */
function qtyText(t: ReportTrade): string {
  return t.sold_qty === t.bought_qty ? `${t.bought_qty}` : `${t.bought_qty} · ${t.sold_qty} sold`;
}

/** Seconds into the IST day of a timestamp. */
function secondsOf(ts: string): number {
  return Number(ts.slice(11, 13)) * 3600 + Number(ts.slice(14, 16)) * 60 + Number(ts.slice(17, 19));
}

const n2 = (v: number | null | undefined) => (v == null ? null : v.toFixed(2));

/** The open tab as a CSV download. */
function exportTab(r: Report, tab: Tab) {
  const who = (r.user.name?.trim() || r.user.phone).replace(/[^\w-]+/g, '-').toLowerCase();
  const file = (what: string) => `paper-${who}-${what}-${r.from ?? r.first_day ?? 'start'}-to-${r.to}.csv`;
  if (tab === 'days') {
    const head = ['Date', 'Contract', 'Opened', 'Closed', 'Held (s)', 'Bought qty', 'Sold qty', 'Avg buy', 'Avg sell', 'Gross that day', 'Charges that day', 'Net that day', 'Trade net', 'How it ended', 'Before reset'];
    const rows = [...r.days].reverse().flatMap((d) =>
      d.trades.map((t) => [d.date, t.contract, t.opened_at, t.closed_at, t.held_secs, t.bought_qty, t.sold_qty, n2(t.avg_buy), n2(t.avg_sell), n2(t.day_gross), n2(t.day_charges), n2(t.day_net), n2(t.pnl), t.outcome?.label, t.before_reset ? 'yes' : '']),
    );
    downloadCsv(file('trades'), [head, ...rows]);
  } else if (tab === 'fills') {
    const head = ['Time', 'Side', 'Contract', 'Qty', 'Price', 'Value', 'Charges', 'Net', 'P&L', 'Reason', 'Position', 'Before reset'];
    const rows = [...r.fills].reverse().map((f) => [f.t, f.action, f.contract, f.qty, n2(f.price), n2(f.value), n2(f.charges), n2(f.net), n2(f.pnl), f.reason, f.position_id, f.before_reset ? 'yes' : '']);
    downloadCsv(file('tradebook'), [head, ...rows]);
  } else if (tab === 'contracts') {
    const head = ['Contract', 'Trades', 'Bought qty', 'Buy value', 'Avg buy', 'Sold qty', 'Sell value', 'Avg sell', 'Gross', 'Charges', 'Net'];
    const rows = r.contracts.map((c) => [c.contract, c.trades, c.buy_qty, n2(c.buy_value), n2(c.avg_buy), c.sell_qty, n2(c.sell_value), n2(c.avg_sell), n2(c.gross), n2(c.charges), n2(c.net)]);
    downloadCsv(file('contracts'), [head, ...rows]);
  } else {
    const head = ['Hour (IST)', 'Closed trades', 'Won', 'Net'];
    const rows = r.hours.map((h) => [`${String(h.hour).padStart(2, '0')}:00`, h.closed, h.wins, n2(h.net)]);
    downloadCsv(file('timing'), [head, ...rows]);
  }
}

function Card({ title, note, right, children, padded = true }: { title: ReactNode; note?: ReactNode; right?: ReactNode; children: ReactNode; padded?: boolean }) {
  return (
    <section className={`flex min-w-0 flex-col rounded-12 bg-panel ${padded ? 'gap-3 p-4' : ''}`}>
      <div className={`flex flex-wrap items-baseline gap-x-2 gap-y-1 ${padded ? '' : 'px-4 pt-3.5 pb-2'}`}>
        <h2 className="type-title text-fg">{title}</h2>
        {note && <span className="type-caption text-fg-muted">{note}</span>}
        {right && <span className="ml-auto">{right}</span>}
      </div>
      {children}
    </section>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="rounded-12 bg-panel px-4 py-6 text-center type-body text-fg-muted">{children}</p>;
}

// ── Figures ──────────────────────────────────────────────────────────── //

function Kpis({ r, desktop }: { r: Report; desktop: boolean }) {
  const s = r.summary;
  const pf = s.profit_factor == null ? (s.wins > 0 ? 'No losses' : '—') : s.profit_factor.toFixed(2);
  return (
    <div className="flex flex-col gap-5 border-b border-line pb-5">
      <Stat label="Net P&L · after charges" value={signedMoney(s.net)} size="hero" tone={toneOf(s.net)} sub={<span className="tnum">Gross {signedMoney(s.gross)} · charges {money(s.charges)}</span>} />
      <div className={`grid gap-x-8 gap-y-5 ${desktop ? 'grid-cols-5' : 'grid-cols-2'}`}>
        <Stat size="medium" label="Win rate" value={s.win_rate == null ? '—' : pct(s.win_rate)} sub={`${s.wins} won · ${s.losses} lost`} />
        <Stat size="medium" label="Profit factor" value={pf} tone={s.profit_factor != null && s.profit_factor < 1 ? 'loss' : 'neutral'} sub="Won ÷ lost" />
        <Stat size="medium" label="Closed trades" value={s.closed} sub={`${plural(s.buys, 'buy')} · ${plural(s.sells, 'sell')}`} />
        <Stat
          size="medium"
          label="Average per trade"
          value={s.avg_per_trade == null ? '—' : signedMoney(s.avg_per_trade)}
          tone={toneOf(s.avg_per_trade)}
          sub={<span className="tnum">{[s.avg_win != null ? `win ${signedMoney(s.avg_win, 0)}` : null, s.avg_loss != null ? `loss ${signedMoney(s.avg_loss, 0)}` : null].filter(Boolean).join(' · ') || '—'}</span>}
        />
        <Stat size="medium" label="Max drawdown" value={signedMoney(-s.max_drawdown)} tone={s.max_drawdown > 0 ? 'loss' : 'neutral'} sub="From the peak, sell by sell" />
        <Stat size="medium" label="Best trade" value={s.best == null ? '—' : signedMoney(s.best)} tone={toneOf(s.best)} />
        <Stat size="medium" label="Worst trade" value={s.worst == null ? '—' : signedMoney(s.worst)} tone={toneOf(s.worst)} />
        <Stat size="medium" label="Average hold" value={held(s.avg_held_secs)} sub="First buy to last sell" />
        <Stat size="medium" label="Turnover" value={money(s.turnover)} sub={<span className="tnum">Charges paid {money(s.charges_paid)}</span>} />
        <Stat size="medium" label="Days traded" value={s.days_traded} sub={`${s.green_days} green · ${s.red_days} red`} />
      </div>
    </div>
  );
}

/** Booked P&L sell by sell through one day, on a clock axis. */
function IntradayChart({ curve, start, height, compact }: { curve: CurvePoint[]; start: string | null; height: number; compact: boolean }) {
  if (curve.length === 0) {
    return <div className="grid place-items-center type-body text-fg-muted" style={{ height }}>Nothing sold in this range yet.</div>;
  }
  const first = start ?? curve[0].t;
  const data = [{ x: secondsOf(first), t: first, pnl: 0 }, ...curve.map((p) => ({ x: secondsOf(p.t), t: p.t, pnl: p.pnl }))];
  const last = data[data.length - 1];
  const color = last.pnl < 0 ? C.loss : C.profit;
  const lo = data[0].x;
  const hi = Math.max(last.x, lo + 60);
  const ticks: number[] = [];
  const step = hi - lo > 3 * 3600 ? 3600 : hi - lo > 3600 ? 1800 : hi - lo > 900 ? 600 : 300;
  for (let t = Math.ceil(lo / step) * step; t <= hi; t += step) ticks.push(t);
  const clock = (x: number) => `${String(Math.floor(x / 3600)).padStart(2, '0')}:${String(Math.floor((x % 3600) / 60)).padStart(2, '0')}`;

  return (
    <div style={{ height }} role="img" aria-label={`Booked P&L through the day, ending at ${signedMoney(last.pnl)}`}>
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: 12, right: compact ? 4 : 0, bottom: 0, left: compact ? 4 : 0 }}>
          <defs>
            <linearGradient id="user-report-fill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity={0.28} />
              <stop offset="100%" stopColor={color} stopOpacity={0.04} />
            </linearGradient>
          </defs>
          <CartesianGrid vertical={false} stroke={C.grid} />
          <XAxis dataKey="x" type="number" domain={[lo, hi]} ticks={ticks} tickFormatter={clock} tick={{ fill: C.muted, fontSize: 11 }} tickLine={false} axisLine={false} />
          <YAxis orientation="right" hide={compact} tickFormatter={(v: number) => axisMoney(v)} tick={{ fill: C.muted, fontSize: 11 }} tickLine={false} axisLine={false} width={52} />
          <Tooltip
            cursor={{ stroke: C.line, strokeWidth: 1 }}
            content={({ active, payload }) => {
              const p = active && payload?.[0] ? (payload[0].payload as (typeof data)[number]) : null;
              if (!p) return null;
              return (
                <div className="rounded-8 border border-line bg-raised px-3 py-2 shadow-lg">
                  <div className="type-label text-fg">{hhmmss(p.t)}</div>
                  <div className="mt-0.5 type-caption text-fg-muted tnum">Booked so far {signedMoney(p.pnl)}</div>
                </div>
              );
            }}
          />
          <Area type="stepAfter" dataKey="pnl" stroke={color} strokeWidth={2} fill="url(#user-report-fill)" isAnimationActive={false} activeDot={{ r: 4, fill: color, stroke: C.raised, strokeWidth: 2 }} />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}

function Holding({ r }: { r: Report }) {
  if (r.open.length === 0) return null;
  const total = r.open.reduce((a, p) => a + (p.pnl ?? 0), 0);
  const today = todayIst();
  return (
    <Card padded={false} title="Holding now" note={`${plural(r.open.length, 'position')} · at the live price, before exit charges`} right={<span className={`num-medium ${TONE_TEXT[toneOf(total)]}`}>{signedMoney(total)}</span>}>
      {r.open.map((p) => (
        <div key={p.id} className="flex items-center gap-4 border-t border-line px-4 py-3">
          <div className="min-w-0 flex-1">
            <div className="type-body-strong text-fg">{p.contract}</div>
            <div className="mt-0.5 type-caption text-fg-muted tnum">{`${p.qty} qty · avg ${price(p.avg)} · since ${timeOn(p.opened_at, today)} · expires ${shortDate(p.expiry)}`}</div>
          </div>
          <div className="shrink-0 text-right">
            <div className={`num-medium ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</div>
            <div className="type-caption text-fg-muted tnum">{p.ltp == null ? 'No price yet' : `LTP ${price(p.ltp)}`}</div>
          </div>
        </div>
      ))}
    </Card>
  );
}

// ── Day by day ───────────────────────────────────────────────────────── //

/** A trade's fills: other days' greyed, each sell with its P&L. */
function Legs({ t, day }: { t: ReportTrade; day: string }) {
  const pnls = sellPnls(t.legs);
  const otherDay = t.legs.some((l) => !l.t.startsWith(day));
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[560px] type-caption tnum">
        <thead className="text-fg-muted">
          <tr className="text-left">
            <th className="pb-1.5 pr-3 font-normal">Time</th>
            <th className="pb-1.5 pr-3 font-normal">Side</th>
            <th className="pb-1.5 pr-3 text-right font-normal">Qty</th>
            <th className="pb-1.5 pr-3 text-right font-normal">Price</th>
            <th className="pb-1.5 pr-3 text-right font-normal">Value</th>
            <th className="pb-1.5 pr-3 text-right font-normal">Charges</th>
            <th className="pb-1.5 pr-3 text-right font-normal">P&amp;L</th>
            <th className="pb-1.5 font-normal">Reason</th>
          </tr>
        </thead>
        <tbody>
          {t.legs.map((l) => {
            const buy = isBuy(l);
            const other = !l.t.startsWith(day);
            const pnl = pnls.get(l.id);
            return (
              <tr key={l.id} className={other ? 'text-fg-muted' : 'text-fg'}>
                <td className="whitespace-nowrap py-1 pr-3">{timeOn(l.t, day)}</td>
                <td className={`py-1 pr-3 ${other ? '' : buy ? 'text-profit' : 'text-loss'}`}>{buy ? 'Buy' : 'Sell'}</td>
                <td className="py-1 pr-3 text-right">{l.qty}</td>
                <td className="py-1 pr-3 text-right">{price(l.price)}</td>
                <td className="py-1 pr-3 text-right">{money(l.qty * l.price)}</td>
                <td className="py-1 pr-3 text-right">{money(l.charges)}</td>
                <td className={`py-1 pr-3 text-right ${other ? '' : TONE_TEXT[toneOf(pnl)]}`}>{pnl == null ? '—' : signedMoney(pnl)}</td>
                <td className="py-1">{buy ? '' : reasonLabel(l.reason)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {otherDay && <p className="mt-2 type-caption text-fg-muted">Greyed fills are from another day; this day counts only its own sells.</p>}
    </div>
  );
}

function TradeTitle({ t }: { t: ReportTrade }) {
  return (
    <span className="flex flex-wrap items-center gap-1.5">
      <span className="type-body-strong text-fg">{t.contract}</span>
      {t.outcome && t.closed_at && <ToneChip tone={t.outcome.tone}>{t.outcome.label}</ToneChip>}
      {!t.closed_at && <Chip>Held</Chip>}
      {t.before_reset && <Chip>Before reset</Chip>}
    </span>
  );
}

/** The day's figure for a trade, and the whole trade's when it spans days. */
function TradeNet({ t, day, size = 'num-small' }: { t: ReportTrade; day: string; size?: string }) {
  const sold = soldOn(t, day);
  return (
    <>
      <div className={`${size} ${sold ? TONE_TEXT[toneOf(t.day_net)] : 'text-fg-muted'}`}>{sold ? signedMoney(t.day_net) : '—'}</div>
      {spansDays(t, day) && t.pnl != null && <div className="type-caption text-fg-muted tnum">{signedMoney(t.pnl)} overall</div>}
    </>
  );
}

function TradeTable({ d }: { d: ReportDay }) {
  const [open, setOpen] = useState<string | null>(null);
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[920px] type-caption tnum">
        <thead className="text-fg-muted">
          <tr className="border-b border-line text-left">
            <th className="py-2 pl-4 pr-3 font-normal">In</th>
            <th className="py-2 pr-3 font-normal">Out</th>
            <th className="py-2 pr-3 font-normal">Held</th>
            <th className="py-2 pr-3 font-normal">Contract</th>
            <th className="py-2 pr-3 text-right font-normal">Qty</th>
            <th className="py-2 pr-3 text-right font-normal">Avg buy</th>
            <th className="py-2 pr-3 text-right font-normal">Avg sell</th>
            <th className="py-2 pr-3 text-right font-normal">Gross</th>
            <th className="py-2 pr-3 text-right font-normal">Charges</th>
            <th className="py-2 pr-3 text-right font-normal">Net</th>
            <th className="w-8 py-2 pr-4 font-normal">
              <span className="sr-only">Fills</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {d.trades.map((t) => {
            const expanded = open === t.id;
            const sold = soldOn(t, d.date);
            const Chevron = expanded ? ChevronDown : ChevronRight;
            return (
              <Fragment key={t.id}>
                <tr aria-expanded={expanded} onClick={() => setOpen(expanded ? null : t.id)} className="cursor-pointer border-b border-line align-top text-fg hover:bg-raised/40">
                  <td className="whitespace-nowrap py-2.5 pl-4 pr-3">{timeOn(t.opened_at, d.date)}</td>
                  <td className="whitespace-nowrap py-2.5 pr-3">{t.closed_at ? timeOn(t.closed_at, d.date) : <span className="text-fg-muted">—</span>}</td>
                  <td className="whitespace-nowrap py-2.5 pr-3">{held(t.held_secs)}</td>
                  <td className="py-2 pr-3">
                    <TradeTitle t={t} />
                  </td>
                  <td className="whitespace-nowrap py-2.5 pr-3 text-right">{qtyText(t)}</td>
                  <td className="py-2.5 pr-3 text-right">{t.avg_buy == null ? '—' : price(t.avg_buy)}</td>
                  <td className="py-2.5 pr-3 text-right">{t.avg_sell == null ? '—' : price(t.avg_sell)}</td>
                  <td className={`py-2.5 pr-3 text-right ${sold ? TONE_TEXT[toneOf(t.day_gross)] : 'text-fg-muted'}`}>{sold ? signedMoney(t.day_gross) : '—'}</td>
                  <td className="py-2.5 pr-3 text-right text-fg-muted">{sold ? money(t.day_charges) : '—'}</td>
                  <td className="py-2 pr-3 text-right">
                    <TradeNet t={t} day={d.date} />
                  </td>
                  <td className="py-2.5 pr-4">
                    <Chevron size={14} className="text-fg-muted" />
                  </td>
                </tr>
                {expanded && (
                  <tr className="border-b border-line bg-inset/40">
                    <td colSpan={11} className="px-4 py-3">
                      <Legs t={t} day={d.date} />
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
        <tfoot>
          <tr className="text-fg">
            <td colSpan={7} className="py-2.5 pl-4 pr-3 text-fg-muted">
              Day total · booked by the day's sells
            </td>
            <td className={`py-2.5 pr-3 text-right ${TONE_TEXT[toneOf(d.gross)]}`}>{signedMoney(d.gross)}</td>
            <td className="py-2.5 pr-3 text-right text-fg-muted">{money(d.charges)}</td>
            <td className={`py-2.5 pr-3 text-right num-small ${TONE_TEXT[toneOf(d.net)]}`}>{signedMoney(d.net)}</td>
            <td />
          </tr>
        </tfoot>
      </table>
    </div>
  );
}

function TradeCards({ d }: { d: ReportDay }) {
  const [open, setOpen] = useState<string | null>(null);
  return (
    <div className="flex flex-col">
      {d.trades.map((t) => {
        const expanded = open === t.id;
        const sold = soldOn(t, d.date);
        const Chevron = expanded ? ChevronDown : ChevronRight;
        const when = `${timeOn(t.opened_at, d.date)} → ${t.closed_at ? timeOn(t.closed_at, d.date) : 'held'}`;
        return (
          <div key={t.id} className="border-b border-line">
            <button type="button" aria-expanded={expanded} onClick={() => setOpen(expanded ? null : t.id)} className="flex min-h-14 w-full items-center gap-3 px-4 py-3 text-left">
              <div className="min-w-0 flex-1">
                <TradeTitle t={t} />
                <div className="mt-0.5 type-caption text-fg-muted tnum">{[when, t.closed_at ? held(t.held_secs) : null, `${qtyText(t)} qty`].filter(Boolean).join(' · ')}</div>
              </div>
              <div className="shrink-0 text-right">
                <TradeNet t={t} day={d.date} size="num-medium" />
              </div>
              <Chevron size={16} className="shrink-0 text-fg-muted" />
            </button>
            {expanded && (
              <div className="flex flex-col gap-2 px-4 pb-4">
                <div className="type-caption text-fg-muted tnum">
                  {[
                    `avg buy ${t.avg_buy == null ? '—' : price(t.avg_buy)}`,
                    `avg sell ${t.avg_sell == null ? '—' : price(t.avg_sell)}`,
                    sold ? `gross ${signedMoney(t.day_gross)}` : null,
                    sold ? `charges ${money(t.day_charges)}` : null,
                  ]
                    .filter(Boolean)
                    .join(' · ')}
                </div>
                <Legs t={t} day={d.date} />
              </div>
            )}
          </div>
        );
      })}
      <div className="flex items-center justify-between gap-3 px-4 py-3 type-caption text-fg-muted tnum">
        <span>{`Gross ${signedMoney(d.gross)} · charges ${money(d.charges)}`}</span>
        <span className={`num-small ${TONE_TEXT[toneOf(d.net)]}`}>{signedMoney(d.net)}</span>
      </div>
    </div>
  );
}

function DayCard({ d, open, onToggle, desktop }: { d: ReportDay; open: boolean; onToggle: () => void; desktop: boolean }) {
  const Chevron = open ? ChevronDown : ChevronRight;
  const line = [
    plural(d.trades.length + d.unlisted, 'trade'),
    d.closed ? `${d.wins} won · ${d.losses} lost` : null,
    `${hhmm(d.first_at)}–${hhmm(d.last_at)}`,
    desktop ? `${plural(d.buys + d.sells, 'fill')} · turnover ${money(d.turnover)}` : null,
  ]
    .filter(Boolean)
    .join(' · ');
  return (
    <section className="overflow-hidden rounded-12 bg-panel">
      <button type="button" aria-expanded={open} onClick={onToggle} className="flex w-full items-center gap-4 px-4 py-3.5 text-left hover:bg-raised/30">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="type-body-strong text-fg">{dayLabel(d.date)}</span>
            {d.expiries.length > 0 && <Chip>{`${d.expiries.map(indexName).join(', ')} expiry`}</Chip>}
          </div>
          <div className="mt-0.5 type-caption text-fg-muted tnum">{line}</div>
        </div>
        <div className="shrink-0 text-right">
          <div className={`num-medium ${TONE_TEXT[toneOf(d.net)]}`}>{signedMoney(d.net)}</div>
          {desktop && <div className="type-caption text-fg-muted tnum">{`gross ${signedMoney(d.gross)} · charges ${money(d.charges)}`}</div>}
        </div>
        <Chevron size={16} className="shrink-0 text-fg-muted" />
      </button>
      {open && (
        <div className="border-t border-line">
          {d.trades.length > 0 && (desktop ? <TradeTable d={d} /> : <TradeCards d={d} />)}
          {d.unlisted > 0 && <p className="px-4 py-3 type-caption text-fg-muted">{`${plural(d.unlisted, 'trade')} not listed — pick a shorter range to see them.`}</p>}
        </div>
      )}
    </section>
  );
}

function DaysTab({ r, desktop }: { r: Report; desktop: boolean }) {
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  if (r.days.length === 0) return <Empty>No trades in this range.</Empty>;
  // A few days open at once; a long range opens on its summary lines.
  const byDefault = r.days.length <= 3;
  const isOpen = (date: string) => toggled[date] ?? byDefault;
  const allOpen = r.days.every((d) => isOpen(d.date));
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <p className="type-caption text-fg-muted">Each day books its own sells, after charges on both sides. Open a trade to see every fill.</p>
        <Button size="sm" kind="quiet" className="ml-auto" onClick={() => setToggled(Object.fromEntries(r.days.map((d) => [d.date, !allOpen])))}>
          {allOpen ? 'Collapse all' : 'Expand all'}
        </Button>
      </div>
      {r.days.map((d) => (
        <DayCard key={d.date} d={d} desktop={desktop} open={isOpen(d.date)} onToggle={() => setToggled((t) => ({ ...t, [d.date]: !isOpen(d.date) }))} />
      ))}
    </div>
  );
}

// ── Tradebook, contracts, timing ─────────────────────────────────────── //

function FillsTab({ r, desktop }: { r: Report; desktop: boolean }) {
  if (r.fills.length === 0) return <Empty>No fills in this range.</Empty>;
  const oneDay = r.days.length <= 1;
  const when = (t: string) => (oneDay ? hhmmss(t) : `${shortDate(t.slice(0, 10))} ${hhmmss(t)}`);
  const s = r.summary;
  const foot = `${plural(s.buys + s.sells, 'fill')} · turnover ${money(s.turnover)} · charges paid ${money(s.charges_paid)}${r.more_fills > 0 ? ` · the oldest ${r.more_fills} not listed` : ''}`;
  return (
    <section className="overflow-hidden rounded-12 bg-panel">
      {desktop ? (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[880px] type-caption tnum">
            <thead className="text-fg-muted">
              <tr className="border-b border-line text-left">
                <th className="py-2 pl-4 pr-3 font-normal">Time</th>
                <th className="py-2 pr-3 font-normal">Side</th>
                <th className="py-2 pr-3 font-normal">Contract</th>
                <th className="py-2 pr-3 text-right font-normal">Qty</th>
                <th className="py-2 pr-3 text-right font-normal">Price</th>
                <th className="py-2 pr-3 text-right font-normal">Value</th>
                <th className="py-2 pr-3 text-right font-normal">Charges</th>
                <th className="py-2 pr-3 text-right font-normal">P&amp;L</th>
                <th className="py-2 pr-4 font-normal">Reason</th>
              </tr>
            </thead>
            <tbody>
              {r.fills.map((f) => {
                const buy = f.action === 'BUY';
                return (
                  <tr key={f.id} className="border-b border-line text-fg">
                    <td className="whitespace-nowrap py-2 pl-4 pr-3">{when(f.t)}</td>
                    <td className={`py-2 pr-3 ${buy ? 'text-profit' : 'text-loss'}`}>{buy ? 'Buy' : 'Sell'}</td>
                    <td className="py-2 pr-3">
                      <span className="flex items-center gap-1.5 whitespace-nowrap">
                        {f.contract}
                        {f.before_reset && <Chip>Before reset</Chip>}
                      </span>
                    </td>
                    <td className="py-2 pr-3 text-right">{f.qty}</td>
                    <td className="py-2 pr-3 text-right">{price(f.price)}</td>
                    <td className="py-2 pr-3 text-right">{money(f.value)}</td>
                    <td className="py-2 pr-3 text-right text-fg-muted">{money(f.charges)}</td>
                    <td className={`py-2 pr-3 text-right ${TONE_TEXT[toneOf(f.pnl)]}`}>{f.pnl == null ? '—' : signedMoney(f.pnl)}</td>
                    <td className="py-2 pr-4 text-fg-muted">{reasonLabel(f.reason)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : (
        r.fills.map((f) => {
          const buy = f.action === 'BUY';
          return (
            <div key={f.id} className="flex items-center gap-3 border-b border-line px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-1.5">
                  <Chip kind={buy ? 'profit' : 'neutral'}>{buy ? 'Buy' : 'Sell'}</Chip>
                  <span className="type-body-strong text-fg">{f.contract}</span>
                  {f.before_reset && <Chip>Before reset</Chip>}
                </div>
                <div className="mt-0.5 type-caption text-fg-muted tnum">{[when(f.t), `${f.qty} @ ${price(f.price)}`, `charges ${money(f.charges)}`, reasonLabel(f.reason) || null].filter(Boolean).join(' · ')}</div>
              </div>
              <div className={`shrink-0 num-medium ${buy ? 'text-fg-muted' : TONE_TEXT[toneOf(f.pnl)]}`}>{buy ? money(f.value) : f.pnl == null ? '—' : signedMoney(f.pnl)}</div>
            </div>
          );
        })
      )}
      <p className="px-4 py-3 type-caption text-fg-muted tnum">{foot}</p>
    </section>
  );
}

function ContractsTab({ r, desktop }: { r: Report; desktop: boolean }) {
  if (r.contracts.length === 0) return <Empty>No trades in this range.</Empty>;
  const s = r.summary;
  return (
    <section className="overflow-hidden rounded-12 bg-panel">
      {desktop ? (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[860px] type-caption tnum">
            <thead className="text-fg-muted">
              <tr className="border-b border-line text-left">
                <th className="py-2 pl-4 pr-3 font-normal">Contract</th>
                <th className="py-2 pr-3 text-right font-normal">Trades</th>
                <th className="py-2 pr-3 text-right font-normal">Bought</th>
                <th className="py-2 pr-3 text-right font-normal">Avg buy</th>
                <th className="py-2 pr-3 text-right font-normal">Sold</th>
                <th className="py-2 pr-3 text-right font-normal">Avg sell</th>
                <th className="py-2 pr-3 text-right font-normal">Gross</th>
                <th className="py-2 pr-3 text-right font-normal">Charges</th>
                <th className="py-2 pr-4 text-right font-normal">Net</th>
              </tr>
            </thead>
            <tbody>
              {r.contracts.map((c) => (
                <tr key={c.contract} className="border-b border-line text-fg">
                  <td className="whitespace-nowrap py-2.5 pl-4 pr-3 type-body-strong">{c.contract}</td>
                  <td className="py-2.5 pr-3 text-right">{c.trades}</td>
                  <td className="py-2.5 pr-3 text-right">{c.buy_qty}</td>
                  <td className="py-2.5 pr-3 text-right">{c.avg_buy == null ? '—' : price(c.avg_buy)}</td>
                  <td className="py-2.5 pr-3 text-right">{c.sell_qty}</td>
                  <td className="py-2.5 pr-3 text-right">{c.avg_sell == null ? '—' : price(c.avg_sell)}</td>
                  <td className={`py-2.5 pr-3 text-right ${TONE_TEXT[toneOf(c.gross)]}`}>{signedMoney(c.gross)}</td>
                  <td className="py-2.5 pr-3 text-right text-fg-muted">{money(c.charges)}</td>
                  <td className={`py-2.5 pr-4 text-right num-small ${TONE_TEXT[toneOf(c.net)]}`}>{signedMoney(c.net)}</td>
                </tr>
              ))}
            </tbody>
            <tfoot>
              <tr className="text-fg">
                <td colSpan={6} className="py-2.5 pl-4 pr-3 text-fg-muted">
                  Total
                </td>
                <td className={`py-2.5 pr-3 text-right ${TONE_TEXT[toneOf(s.gross)]}`}>{signedMoney(s.gross)}</td>
                <td className="py-2.5 pr-3 text-right text-fg-muted">{money(s.charges)}</td>
                <td className={`py-2.5 pr-4 text-right num-small ${TONE_TEXT[toneOf(s.net)]}`}>{signedMoney(s.net)}</td>
              </tr>
            </tfoot>
          </table>
        </div>
      ) : (
        r.contracts.map((c) => (
          <div key={c.contract} className="flex items-center gap-3 border-b border-line px-4 py-3">
            <div className="min-w-0 flex-1">
              <div className="type-body-strong text-fg">{c.contract}</div>
              <div className="mt-0.5 type-caption text-fg-muted tnum">
                {[plural(c.trades, 'trade'), `bought ${c.buy_qty}${c.avg_buy == null ? '' : ` @ ${price(c.avg_buy)}`}`, `sold ${c.sell_qty}${c.avg_sell == null ? '' : ` @ ${price(c.avg_sell)}`}`, `charges ${money(c.charges)}`].join(' · ')}
              </div>
            </div>
            <div className={`shrink-0 num-medium ${TONE_TEXT[toneOf(c.net)]}`}>{signedMoney(c.net)}</div>
          </div>
        ))
      )}
      <p className="px-4 py-3 type-caption text-fg-muted">Bought and sold in the range; net is what its sells booked, after charges.</p>
    </section>
  );
}

function TimingTab({ r, desktop }: { r: Report; desktop: boolean }) {
  if (r.hours.length === 0) return <Empty>No trades closed in this range.</Empty>;
  const max = Math.max(1, ...r.hours.map((h) => Math.abs(h.net)));
  return (
    <section className="overflow-hidden rounded-12 bg-panel">
      <p className="px-4 pt-3.5 pb-2 type-caption text-fg-muted">Trades closed in the range, by the IST hour they were entered.</p>
      {r.hours.map((h) => {
        const tone = toneOf(h.net);
        const hour = String(h.hour).padStart(2, '0');
        return (
          <div key={h.hour} className="flex min-h-12 items-center gap-4 border-t border-line px-4 py-3">
            <span className={`shrink-0 type-body-strong text-fg tnum ${desktop ? 'w-32' : 'w-24'}`}>{`${hour}:00–${String(h.hour + 1).padStart(2, '0')}:00`}</span>
            <span className={`shrink-0 type-caption text-fg-muted tnum ${desktop ? 'w-44' : 'min-w-0 flex-1 truncate'}`}>{`${plural(h.closed, 'trade')} · ${pct((h.wins / h.closed) * 100)} won`}</span>
            {desktop && (
              <span className="flex-1">
                <span className={`block h-1 rounded-full ${tone === 'loss' ? 'bg-loss' : 'bg-profit'}`} style={{ width: `${Math.max(2, (Math.abs(h.net) / max) * 100) * 0.6}%`, minWidth: 4 }} />
              </span>
            )}
            <span className={`num-medium ${TONE_TEXT[tone]}`}>{signedMoney(h.net)}</span>
          </div>
        );
      })}
    </section>
  );
}

// ── The report ───────────────────────────────────────────────────────── //

/**
 * Admin: one user's paper P&L report — totals, the P&L curve, every day with
 * each trade's entry and exit times and fills, the tradebook, each contract
 * and the hours they trade — over Today, 7D, 30D, All or any dates.
 * Read-only.
 */
export function UserReport({ serverBase, userId, desktop, onBack }: { serverBase: string; userId: number; desktop: boolean; onBack: () => void }) {
  const today = todayIst();
  const [preset, setPreset] = useState<Preset>('today');
  const [custom, setCustom] = useState<Span>({ from: today, to: today });
  const [tab, setTab] = useState<Tab>('days');
  const span = preset === 'custom' ? custom : presetSpan(preset, today);
  const invalid = span.from != null && span.from > span.to;
  const live = span.to >= today;
  const report = usePoll(
    () => getUserReport(serverBase, userId, span.from, span.to),
    live ? 15_000 : 300_000,
    `${serverBase}|${userId}|${span.from ?? 'first'}|${span.to}`,
    !invalid,
  );
  const r = report.data;
  const top = useRef<HTMLDivElement>(null);
  useEffect(() => top.current?.scrollIntoView({ block: 'start' }), [userId]);

  function pickDate(which: 'from' | 'to', value: string) {
    setCustom({ ...span, [which]: value || (which === 'to' ? today : null) });
    setPreset('custom');
  }

  const user = r?.user;
  const firstDay = r?.first_day ?? null;
  const spanLabel = r ? (r.from ? dateRange(r.from, r.to) : firstDay && firstDay <= r.to ? dateRange(firstDay, r.to) : shortDate(r.to)) : '';

  const head = (
    <div className="flex flex-col gap-2">
      <div>
        <Button size={desktop ? 'sm' : 'md'} kind="quiet" icon={<ArrowLeft size={14} />} onClick={onBack} className="-ml-2.5">
          Users
        </Button>
      </div>
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h1 className="type-title text-fg">{user ? `${nameOf(user)} · P&L report` : 'P&L report'}</h1>
        {user && <span className="type-caption text-fg-muted tnum">+91 {formatPhone(user.phone)}</span>}
        {user && !user.enabled && <Chip kind="loss">Disabled</Chip>}
      </div>
      {user?.paper && (
        <div className="type-caption text-fg-muted">
          <PaperLine p={user.paper} />
        </div>
      )}
    </div>
  );

  // Desktop: inline after the presets. Phone: two columns, each caption over its field.
  const field = desktop ? 'flex items-center gap-1.5' : 'flex min-w-0 flex-col gap-1';
  const input = `${DATE_INPUT} ${desktop ? 'h-8' : 'h-11 w-full'}`;
  const dates = (
    <div className={desktop ? 'flex items-center gap-3' : 'grid grid-cols-2 gap-2'}>
      <label className={`${field} type-caption text-fg-muted`}>
        From
        <input type="date" className={input} value={span.from ?? firstDay ?? ''} min={firstDay ?? undefined} max={span.to} onChange={(e) => pickDate('from', e.target.value)} />
      </label>
      <label className={`${field} type-caption text-fg-muted`}>
        To
        <input type="date" className={input} value={span.to} min={span.from ?? undefined} max={today} onChange={(e) => pickDate('to', e.target.value)} />
      </label>
    </div>
  );

  const exportButton = (
    <Button size={desktop ? 'md' : 'lg'} icon={<Download size={14} />} disabled={!r} onClick={() => r && exportTab(r, tab)} title="Downloads the open tab" className={desktop ? '' : 'w-full'}>
      Export CSV
    </Button>
  );

  const controls = desktop ? (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
      <Segment label="Range" options={PRESETS} value={preset} size="sm" onChange={setPreset} />
      {dates}
      {r && <span className="type-caption text-fg-muted tnum">{[spanLabel, plural(r.summary.days_traded, 'trading day')].join(' · ')}</span>}
      <span className="flex-1" />
      {exportButton}
    </div>
  ) : (
    <div className="flex flex-col gap-3">
      <Segment label="Range" options={PRESETS} value={preset} size="lg" stretch onChange={setPreset} />
      {dates}
      {r && <span className="type-caption text-fg-muted tnum">{[spanLabel, plural(r.summary.days_traded, 'trading day')].join(' · ')}</span>}
    </div>
  );

  const multiDay = (r?.days.length ?? 0) > 1;
  const chart = r && (
    <Card title={multiDay ? 'Cumulative P&L' : 'P&L through the day'} note="Booked by sells, after charges">
      {multiDay ? (
        <PnlChart
          days={r.days.map((d) => ({ date: d.date, calls: d.closed, wins: d.wins, net: d.net, expiries: d.expiries }))}
          curve={r.curve}
          maxDrawdown={r.summary.max_drawdown}
          height={desktop ? 260 : 170}
          compact={!desktop}
        />
      ) : (
        <IntradayChart curve={r.curve} start={r.days[0]?.first_at ?? null} height={desktop ? 260 : 170} compact={!desktop} />
      )}
    </Card>
  );

  const nothing = r && r.summary.buys + r.summary.sells === 0;
  const body = !r ? (
    <p className={`type-body ${report.error ? 'text-loss' : 'text-fg-muted'}`}>{invalid ? 'From must be on or before To.' : report.error ? `Couldn't load the report: ${report.error}` : 'Loading…'}</p>
  ) : (
    <>
      <Kpis r={r} desktop={desktop} />
      {nothing ? (
        <Empty>{firstDay ? `No paper trades in this range. Their first trade was on ${dayLabel(firstDay)}.` : "They haven't traded on paper yet."}</Empty>
      ) : (
        chart
      )}
      <Holding r={r} />
      {!nothing && (
        <>
          <Segment label="Report view" options={desktop ? TABS : PHONE_TABS} value={tab} onChange={setTab} size={desktop ? 'sm' : 'lg'} stretch={!desktop} />
          {tab === 'days' ? <DaysTab r={r} desktop={desktop} /> : tab === 'fills' ? <FillsTab r={r} desktop={desktop} /> : tab === 'contracts' ? <ContractsTab r={r} desktop={desktop} /> : <TimingTab r={r} desktop={desktop} />}
          {!desktop && exportButton}
        </>
      )}
    </>
  );

  return (
    <div ref={top} className={`${desktop ? 'px-10 pt-6 pb-24' : 'p-4 pb-12'}`}>
      <div className={`flex flex-col gap-5 ${desktop ? 'max-w-[1120px]' : ''}`}>
        {head}
        {controls}
        {body}
      </div>
    </div>
  );
}
