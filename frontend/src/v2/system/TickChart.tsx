import type { TelemetryPoint } from '../../types';
import { MetricChart } from './MetricChart';
import { ms, tick } from './units';

/** The engine's tick period: a slower tick body is an overrun. */
export const TICK_BUDGET_MS = 50;

/** "Engine tick p95": the last 15 minutes against the 50 ms budget. */
export function TickChart({ history, height }: { history: TelemetryPoint[]; height: number }) {
  const points = history.filter((p) => p.tick_p95_ms != null).map((p) => ({ ts: p.ts * 1000, v: p.tick_p95_ms as number }));
  return (
    <MetricChart
      points={points}
      height={height}
      label="Engine tick p95 over the last 15 minutes"
      format={tick}
      value={ms}
      reference={{ value: TICK_BUDGET_MS, text: `Budget ${TICK_BUDGET_MS} ms` }}
    />
  );
}
