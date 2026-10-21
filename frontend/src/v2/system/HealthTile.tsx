import type { ReactNode } from 'react';
import type { Level } from '../lib/types';
import { HealthDot } from '../ui/Health';

/** The last 15 minutes of one series, as a line: no axes, gaps skipped. */
function Sparkline({ values }: { values: (number | null)[] }) {
  const known = values.map((v, i) => [i, v] as const).filter((p): p is readonly [number, number] => p[1] != null && Number.isFinite(p[1]));
  if (known.length < 2) return <div className="h-7" />;
  const lo = Math.min(...known.map((p) => p[1]));
  const hi = Math.max(...known.map((p) => p[1]));
  const span = hi - lo || 1;
  const n = Math.max(1, values.length - 1);
  const points = known.map(([i, v]) => `${((i / n) * 100).toFixed(2)},${(26 - ((v - lo) / span) * 24).toFixed(2)}`).join(' ');
  return (
    <svg viewBox="0 0 100 28" preserveAspectRatio="none" className="h-7 w-full" aria-hidden>
      <polyline points={points} fill="none" stroke="var(--color-chart-indicator)" strokeWidth={1.5} vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
    </svg>
  );
}

/** Figma "Health tile": a dot and name, the reading, a line of detail and,
 * on desktop, the last 15 minutes. */
export function HealthTile({ name, level, value, detail, series, compact }: {
  name: string;
  level: Level;
  value: ReactNode;
  detail: ReactNode;
  series?: (number | null)[];
  compact: boolean;
}) {
  return (
    <section className="flex min-w-0 flex-col gap-1 rounded-12 bg-panel p-4">
      <div className="flex items-center gap-2 type-caption text-fg-muted">
        <HealthDot level={level} />
        {name}
      </div>
      <div className={`${compact ? 'type-title' : 'num-large'} truncate text-fg`}>{value}</div>
      <div className="type-caption text-fg-muted">{detail}</div>
      {!compact && series && (
        <div className="mt-2">
          <Sparkline values={series} />
        </div>
      )}
    </section>
  );
}
