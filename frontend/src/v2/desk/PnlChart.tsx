import { Area, AreaChart, CartesianGrid, ReferenceArea, ReferenceDot, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { axisMoney, dateRange, dayLabel, shortDate, signedMoney } from '../lib/format';
import type { CurvePoint, DayRow } from '../lib/types';

// The design's tokens as plain values: SVG attributes can't read CSS variables.
const C = { profit: '#22b47f', loss: '#ee705c', grid: '#2c3442', muted: '#8e97a7', fg: '#e8eaee', raised: '#2c3442', line: '#353e4d' };

interface DayPoint {
  i: number;
  date: string;
  /** Realised P&L to the end of the day. */
  pnl: number;
  /** That day's own. */
  net: number;
}

/** Realised P&L to the end of each day, oldest first. */
function byDay(days: DayRow[]): DayPoint[] {
  const out: DayPoint[] = [];
  let running = 0;
  for (const d of [...days].reverse()) {
    running += d.net;
    out.push({ i: out.length, date: d.date, pnl: running, net: d.net });
  }
  return out;
}

/** The deepest drawdown: from its peak to its trough, by day. */
function drawdownSpan(curve: CurvePoint[]): { peak: string; trough: string } | null {
  let worst = -1;
  for (let i = 0; i < curve.length; i++) if (curve[i].drawdown > 0 && (worst < 0 || curve[i].drawdown > curve[worst].drawdown)) worst = i;
  if (worst < 0) return null;
  let peak = worst;
  while (peak > 0 && curve[peak].drawdown > 0) peak--;
  return { peak: curve[peak].t.slice(0, 10), trough: curve[worst].t.slice(0, 10) };
}

/** Cumulative realised P&L by trading day, with the deepest drawdown shaded. */
export function PnlChart({ days, curve, maxDrawdown, height, compact = false }: {
  /** Newest first, as the history answers. */
  days: DayRow[];
  curve: CurvePoint[];
  maxDrawdown: number;
  height: number;
  /** Phone: no money axis, only the first and last dates. */
  compact?: boolean;
}) {
  const data = byDay(days);
  if (data.length === 0) {
    return <div className="grid place-items-center type-body text-fg-muted" style={{ height }}>No closed calls in this range.</div>;
  }
  const last = data[data.length - 1];
  const color = last.pnl < 0 ? C.loss : C.profit;
  const span = maxDrawdown > 0 ? drawdownSpan(curve) : null;
  const at = (date: string) => data.find((d) => d.date === date)?.i;
  const from = span ? at(span.peak) : undefined;
  const to = span ? at(span.trough) : undefined;
  const band = from != null && to != null ? (from === to ? { x1: from - 0.4, x2: to + 0.4 } : { x1: from, x2: to }) : null;
  const bandLabel = span
    ? `${compact ? '' : 'Drawdown '}${signedMoney(-maxDrawdown, 0)}${compact ? '' : ` · ${dateRange(span.peak, span.trough)}`}`
    : '';
  const ticks = compact
    ? [0, data.length - 1]
    : Array.from(new Set(Array.from({ length: Math.min(5, data.length) }, (_, k) => Math.round((k * (data.length - 1)) / Math.max(1, Math.min(5, data.length) - 1)))));

  return (
    <div style={{ height }} role="img" aria-label={`Cumulative realised P&L, ending at ${signedMoney(last.pnl)}`}>
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: 22, right: compact ? 4 : 0, bottom: 0, left: compact ? 4 : 0 }}>
          <defs>
            <linearGradient id="pnl-fill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity={0.28} />
              <stop offset="100%" stopColor={color} stopOpacity={0.04} />
            </linearGradient>
          </defs>
          <CartesianGrid vertical={false} stroke={C.grid} />
          <XAxis
            dataKey="i"
            type="number"
            domain={[0, Math.max(1, data.length - 1)]}
            ticks={ticks}
            tick={({ x, y, payload }) => {
              // The end dates sit inside the plot, not centred off its edges.
              const i = Number(payload.value);
              const anchor = i === 0 ? 'start' : i === data.length - 1 ? 'end' : 'middle';
              return (
                <text x={Number(x)} y={Number(y) + 12} fill={C.muted} fontSize={11} textAnchor={anchor}>
                  {data[i] ? shortDate(data[i].date) : ''}
                </text>
              );
            }}
            tickLine={false}
            axisLine={false}
            interval={0}
          />
          <YAxis
            orientation="right"
            hide={compact}
            tickFormatter={(v: number) => axisMoney(v)}
            tick={{ fill: C.muted, fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={52}
          />
          {band && (
            <ReferenceArea
              x1={band.x1}
              x2={band.x2}
              fill={C.loss}
              fillOpacity={0.16}
              stroke="none"
              label={{ value: bandLabel, position: 'insideTopLeft', fill: C.loss, fontSize: 11 }}
            />
          )}
          <Tooltip
            cursor={{ stroke: C.line, strokeWidth: 1 }}
            content={({ active, payload }) => {
              const p = active && payload?.[0] ? (payload[0].payload as DayPoint) : null;
              if (!p) return null;
              return (
                <div className="rounded-8 border border-line bg-raised px-3 py-2 shadow-lg">
                  <div className="type-label text-fg">{dayLabel(p.date)}</div>
                  <div className="mt-0.5 type-caption text-fg-muted tnum">
                    To date {signedMoney(p.pnl)} · that day {signedMoney(p.net)}
                  </div>
                </div>
              );
            }}
          />
          <Area type="linear" dataKey="pnl" stroke={color} strokeWidth={2} fill="url(#pnl-fill)" isAnimationActive={false} activeDot={{ r: 4, fill: color, stroke: C.raised, strokeWidth: 2 }} />
          <ReferenceDot
            x={last.i}
            y={last.pnl}
            r={4}
            fill={color}
            stroke={C.raised}
            strokeWidth={2}
            label={
              compact
                ? undefined
                : ({ viewBox }: { viewBox?: { x?: number; y?: number } }) => (
                    // Up and to the left of the last point, clear of the axis.
                    <text x={(viewBox?.x ?? 0) - 6} y={(viewBox?.y ?? 0) - 8} fill={color} fontSize={11} textAnchor="end">
                      {signedMoney(last.pnl, 0)}
                    </text>
                  )
            }
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}
