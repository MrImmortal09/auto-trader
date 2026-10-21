import { CartesianGrid, Line, LineChart, ReferenceLine, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import type { TelemetryPoint } from '../../types';

// The design's tokens as plain values: SVG attributes can't read CSS variables.
const C = { indicator: '#79a7f2', loss: '#ee705c', grid: '#2c3442', muted: '#8e97a7', line: '#353e4d', raised: '#2c3442' };

/** The engine's tick period: a slower tick body is an overrun. */
export const TICK_BUDGET_MS = 50;

const CLOCK = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const CLOCK_S = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });

/** "Engine tick p95": the last 15 minutes against the 50 ms budget. */
export function TickChart({ history, height }: { history: TelemetryPoint[]; height: number }) {
  const data = history.filter((p) => p.tick_p95_ms != null).map((p) => ({ ts: p.ts * 1000, v: p.tick_p95_ms as number }));
  if (data.length < 2) {
    return <div className="grid place-items-center type-body text-fg-muted" style={{ height }}>Waiting for samples…</div>;
  }
  const top = Math.max(TICK_BUDGET_MS, ...data.map((d) => d.v));
  const first = data[0].ts;
  const last = data[data.length - 1].ts;
  return (
    <div style={{ height }} role="img" aria-label={`Engine tick p95 over the last 15 minutes, now ${data[data.length - 1].v.toFixed(1)} ms`}>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={data} margin={{ top: 18, right: 0, bottom: 0, left: 0 }}>
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
            domain={[0, Math.ceil(top / 25) * 25]}
            ticks={Array.from({ length: Math.ceil(top / 25) + 1 }, (_, k) => k * 25)}
            tick={{ fill: C.muted, fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={32}
          />
          <ReferenceLine
            y={TICK_BUDGET_MS}
            stroke={C.loss}
            strokeDasharray="4 4"
            label={{ value: `Budget ${TICK_BUDGET_MS} ms`, position: 'insideTopLeft', fill: C.loss, fontSize: 11 }}
          />
          <Tooltip
            cursor={{ stroke: C.line, strokeWidth: 1 }}
            content={({ active, payload }) => {
              const p = active && payload?.[0] ? (payload[0].payload as { ts: number; v: number }) : null;
              if (!p) return null;
              return (
                <div className="rounded-8 border border-line bg-raised px-3 py-2 shadow-lg">
                  <div className="type-label text-fg tnum">{p.v.toFixed(1)} ms</div>
                  <div className="mt-0.5 type-caption text-fg-muted tnum">{CLOCK_S.format(new Date(p.ts))}</div>
                </div>
              );
            }}
          />
          <Line type="linear" dataKey="v" stroke={C.indicator} strokeWidth={1.5} dot={false} isAnimationActive={false} activeDot={{ r: 4, fill: C.indicator, stroke: C.raised, strokeWidth: 2 }} />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}
