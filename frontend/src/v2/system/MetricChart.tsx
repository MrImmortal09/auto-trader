import { Area, AreaChart, CartesianGrid, ReferenceLine, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';

// The design's tokens as plain values: SVG attributes can't read CSS variables.
const C = { indicator: '#79a7f2', loss: '#ee705c', grid: '#2c3442', muted: '#8e97a7', line: '#353e4d', raised: '#2c3442' };

const CLOCK = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const CLOCK_S = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });

export interface MetricPoint {
  /** Unix ms. */
  ts: number;
  v: number;
}

/** A round top for the axis: 1, 2 or 5 × a power of ten. */
function niceTop(n: number): number {
  if (!(n > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(n));
  const m = n / p;
  return (m <= 1 ? 1 : m <= 2 ? 2 : m <= 5 ? 5 : 10) * p;
}

/**
 * One series over the last 15 minutes, in the indicator blue over a light
 * wash: a time axis with its ends, a value axis from zero, and the value
 * and time under the pointer. `reference` draws a dashed limit (a budget).
 */
export function MetricChart({ points, height, format, value = format, label, reference, atLeast = 0 }: {
  points: MetricPoint[];
  height: number;
  /** An axis tick. */
  format: (v: number) => string;
  /** A value under the pointer, with its unit. */
  value?: (v: number) => string;
  /** What a screen reader hears, with the latest value. */
  label: string;
  reference?: { value: number; text: string };
  /** The axis reaches at least this high. */
  atLeast?: number;
}) {
  const data = points.filter((p) => Number.isFinite(p.v));
  if (data.length < 2) {
    return (
      <div className="grid place-items-center type-caption text-fg-muted" style={{ height }}>
        Collecting samples…
      </div>
    );
  }
  const top = niceTop(Math.max(atLeast, reference?.value ?? 0, ...data.map((d) => d.v)));
  const first = data[0].ts;
  const last = data[data.length - 1].ts;
  return (
    <div style={{ height }} role="img" aria-label={`${label}, now ${value(data[data.length - 1].v)}`}>
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: reference ? 18 : 6, right: 0, bottom: 0, left: 0 }}>
          <CartesianGrid vertical={false} stroke={C.grid} />
          <XAxis
            dataKey="ts"
            type="number"
            domain={[first, last]}
            ticks={[first, last]}
            tick={({ x, y, payload }) => (
              <text x={Number(x)} y={Number(y) + 12} fill={C.muted} fontSize={11} textAnchor={Number(payload.value) === first ? 'start' : 'end'}>
                {CLOCK.format(new Date(Number(payload.value)))}
              </text>
            )}
            tickLine={false}
            axisLine={false}
            interval={0}
          />
          <YAxis
            orientation="right"
            domain={[0, top]}
            ticks={[0, top / 2, top]}
            tickFormatter={(v: number) => format(v)}
            tick={{ fill: C.muted, fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={44}
          />
          {reference && (
            <ReferenceLine y={reference.value} stroke={C.loss} strokeDasharray="4 4" label={{ value: reference.text, position: 'insideTopLeft', fill: C.loss, fontSize: 11 }} />
          )}
          <Tooltip
            cursor={{ stroke: C.line, strokeWidth: 1 }}
            content={({ active, payload }) => {
              const p = active && payload?.[0] ? (payload[0].payload as MetricPoint) : null;
              if (!p) return null;
              return (
                <div className="rounded-8 border border-line bg-raised px-3 py-2 shadow-lg">
                  <div className="type-label text-fg tnum">{value(p.v)}</div>
                  <div className="mt-0.5 type-caption text-fg-muted tnum">{CLOCK_S.format(new Date(p.ts))}</div>
                </div>
              );
            }}
          />
          <Area
            type="linear"
            dataKey="v"
            stroke={C.indicator}
            strokeWidth={1.5}
            fill={C.indicator}
            fillOpacity={0.1}
            isAnimationActive={false}
            activeDot={{ r: 4, fill: C.indicator, stroke: C.raised, strokeWidth: 2 }}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}
