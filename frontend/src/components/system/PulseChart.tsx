import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import type { TelemetryPoint } from '../../types';
import { fmtClockIST } from '../../lib/pulse';
import { CARD } from './PulseParts';

type NumericKey = {
  [K in keyof TelemetryPoint]: TelemetryPoint[K] extends number | null ? K : never;
}[keyof TelemetryPoint];

export interface PulseSeries {
  key: NumericKey;
  label: string;
  color: string;
}

/** Live time-series card. One series draws a line over a 10% wash; two series
 * draw lines only, with a legend. One y-axis, always. */
export function PulseChart({
  title,
  subtitle,
  data,
  series,
  format,
  current,
}: {
  title: string;
  subtitle: string;
  data: TelemetryPoint[];
  series: PulseSeries[];
  format: (v: number) => string;
  /** The latest value, shown as the card's headline (the line's end label). */
  current: string;
}) {
  const single = series.length === 1;
  return (
    <div className={`${CARD} p-4 flex flex-col gap-2`}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-sm font-semibold text-on-surface">{title}</div>
          <div className="text-[11px] text-on-surface-variant">{subtitle}</div>
        </div>
        <div className="text-lg font-bold text-on-surface shrink-0">{current}</div>
      </div>
      {!single && (
        <div className="flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-on-surface-variant" aria-hidden="true">
          {series.map((s) => (
            <span key={s.key} className="inline-flex items-center gap-1.5">
              <span className="inline-block w-3 h-0.5 rounded-full" style={{ background: s.color }} />
              {s.label}
            </span>
          ))}
        </div>
      )}
      <div className="h-36" role="img" aria-label={`${title}: currently ${current}`}>
        {data.length < 2 ? (
          <div className="h-full flex items-center justify-center text-xs text-on-surface-variant">
            Collecting samples…
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
              <CartesianGrid vertical={false} stroke="var(--color-outline-variant)" strokeOpacity={0.6} />
              <XAxis
                dataKey="ts"
                type="number"
                scale="time"
                domain={['dataMin', 'dataMax']}
                tickFormatter={(v: number) => fmtClockIST(v).slice(0, 5)}
                tick={{ fontSize: 10, fill: 'var(--color-on-surface-variant)' }}
                tickLine={false}
                axisLine={{ stroke: 'var(--color-outline-variant)' }}
                minTickGap={40}
              />
              <YAxis
                tickFormatter={(v: number) => format(v)}
                tick={{ fontSize: 10, fill: 'var(--color-on-surface-variant)' }}
                tickLine={false}
                axisLine={false}
                width={52}
                allowDecimals
              />
              <Tooltip
                isAnimationActive={false}
                cursor={{ stroke: 'var(--color-outline)', strokeWidth: 1 }}
                content={(p) => {
                  const point = p.payload?.[0]?.payload as TelemetryPoint | undefined;
                  if (!p.active || !point) return null;
                  return (
                    <div className="bg-surface-container-lowest border border-outline-variant rounded-lg shadow-md px-3 py-2 text-xs">
                      <div className="text-on-surface-variant mb-1">{fmtClockIST(point.ts)} IST</div>
                      {series.map((s) => {
                        const v = point[s.key];
                        return (
                          <div key={s.key} className="flex items-center gap-2">
                            <span className="inline-block w-3 h-0.5 rounded-full" style={{ background: s.color }} />
                            <span className="font-semibold text-on-surface tabular-nums">
                              {v == null ? '—' : format(v)}
                            </span>
                            <span className="text-on-surface-variant">{s.label}</span>
                          </div>
                        );
                      })}
                    </div>
                  );
                }}
              />
              {series.map((s) => (
                <Area
                  key={s.key}
                  type="monotone"
                  dataKey={s.key}
                  name={s.label}
                  stroke={s.color}
                  strokeWidth={2}
                  strokeLinejoin="round"
                  strokeLinecap="round"
                  fill={s.color}
                  fillOpacity={single ? 0.1 : 0}
                  isAnimationActive={false}
                  connectNulls={false}
                  dot={false}
                  activeDot={{ r: 4, stroke: 'var(--color-surface-container-lowest)', strokeWidth: 2 }}
                />
              ))}
            </AreaChart>
          </ResponsiveContainer>
        )}
      </div>
    </div>
  );
}
