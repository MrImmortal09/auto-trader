import type { ReactNode } from 'react';
import type { TelemetryPoint, TelemetrySample } from '../../types';
import { MetricChart, type MetricPoint } from './MetricChart';
import { Card, CountRow } from './parts';
import { count, mb, ms, pct, rate, tick, uptime } from './units';

/** Host memory in use, as a share: the VM has little to spare. */
function usedShare(total: number, available: number): number | null {
  return total > 0 ? ((total - available) / total) * 100 : null;
}

const series = (history: TelemetryPoint[], pick: (p: TelemetryPoint) => number | null): MetricPoint[] =>
  history.map((p) => ({ ts: p.ts * 1000, v: pick(p) ?? Number.NaN }));

/** One of a card's small charts: what it is, its value now, and the chart. */
function Small({ label, value, alarm = false, children }: { label: string; value: ReactNode; alarm?: boolean; children: ReactNode }) {
  return (
    <div className="mt-2 first:mt-0">
      <div className="flex items-baseline justify-between gap-3">
        <span className="type-label text-fg-muted">{label}</span>
        <span className={`num-medium ${alarm ? 'text-loss' : 'text-fg'}`}>{value}</span>
      </div>
      <div className="mt-1">{children}</div>
    </div>
  );
}

const pctAxis = (v: number) => `${Math.round(v)}%`;

/** CPU: the server's own (of one core) and the whole host's (of all its
 * cores), each on its own chart — the two aren't on one scale. */
export function CpuCard({ s, history, height = 110 }: { s: TelemetrySample | null; history: TelemetryPoint[]; height?: number }) {
  return (
    <Card title="CPU" note={s ? `${s.cpu_cores} cores` : undefined}>
      <Small label="Server, of one core" value={pct(s?.process_cpu_pct)}>
        <MetricChart points={series(history, (p) => p.process_cpu_pct)} height={height} format={pctAxis} value={pct} label="Server CPU" atLeast={10} />
      </Small>
      <Small label="Host, of all cores" value={pct(s?.host_cpu_pct)} alarm={(s?.host_cpu_pct ?? 0) >= 90}>
        <MetricChart points={series(history, (p) => p.host_cpu_pct)} height={height} format={pctAxis} value={pct} label="Host CPU" atLeast={10} />
      </Small>
    </Card>
  );
}

/** Memory: the server's resident size, and how full the host is. */
export function MemoryCard({ s, history, height = 110 }: { s: TelemetrySample | null; history: TelemetryPoint[]; height?: number }) {
  const total = s?.host_memory_total_mb ?? 0;
  const used = s ? usedShare(total, s.host_available_mb) : null;
  return (
    <Card title="Memory" note={s ? `swap ${mb(s.host_swap_used_mb)} of ${mb(s.host_swap_total_mb)}` : undefined}>
      <Small label="Server, resident, MB" value={mb(s?.process_rss_mb)}>
        <MetricChart points={series(history, (p) => p.process_rss_mb)} height={height} format={tick} value={mb} label="Server memory" atLeast={100} />
      </Small>
      <Small label={s ? `Host, ${mb(s.host_available_mb)} free of ${mb(total)}` : 'Host'} value={used == null ? '—' : `${pct(used)} used`} alarm={(used ?? 0) >= 90}>
        <MetricChart points={series(history, (p) => usedShare(total, p.host_available_mb))} height={height} format={pctAxis} value={(v) => `${pct(v)} used`} label="Host memory used" atLeast={100} />
      </Small>
    </Card>
  );
}

/** The process, its runtime, SQLite and the streams. */
export function RuntimeCard({ s }: { s: TelemetrySample | null }) {
  if (!s) return <Card title="Host and runtime">{null}</Card>;
  const queue = s.db_queue_capacity > 0 ? s.db_queue_depth / s.db_queue_capacity : 0;
  return (
    <Card title="Host and runtime">
      <CountRow label="Load average" hint={`1, 5 and 15 minutes, on ${s.cpu_cores} cores`} value={`${s.load_average[0].toFixed(2)} · ${s.load_average[1].toFixed(2)} · ${s.load_average[2].toFixed(2)}`} />
      <CountRow label="Server uptime" value={uptime(s.process_uptime_secs)} />
      <CountRow label="Virtual memory" value={mb(s.process_virtual_mb)} />
      <CountRow label="Async tasks" hint={`${s.tokio_workers} workers · ${s.tokio_global_queue} queued`} value={count(s.tokio_alive_tasks)} />
      <CountRow label="SQLite writer queue" value={`${count(s.db_queue_depth)} of ${count(s.db_queue_capacity)}`} alarm={queue >= 0.5} />
      <CountRow label="SQLite pool in use" value={`${Math.max(0, s.db_pool_size - s.db_pool_idle)} of ${s.db_pool_size}`} />
      <CountRow label="Database write p95" hint={`${count(s.db_write_errors_total)} write errors`} value={ms(s.db_write_p95_ms)} alarm={s.db_write_errors_total > 0} />
      <CountRow label="Stream clients" hint="Logs, stats and price streams" value={count(s.sse_clients)} />
    </Card>
  );
}

/** Price feed, Kotak REST and the dashboard API over the last 15 minutes. */
export function TrafficCards({ s, history, height = 150 }: { s: TelemetrySample | null; history: TelemetryPoint[]; height?: number }) {
  return (
    <>
      <Card title="Price feed" note="ticks a second" aside={<span className="num-medium text-fg">{rate(s?.feed_ticks_per_sec)}</span>}>
        <MetricChart points={series(history, (p) => p.feed_ticks_per_sec)} height={height} format={tick} value={(v) => `${rate(v)} ticks/s`} label="Price-feed ticks a second" />
        <p className="mt-2 type-caption text-fg-muted">
          {s ? `${count(s.feed_ticks_total)} ticks · ${count(s.feed_bridge_restarts_total)} bridge restarts since the server started` : ' '}
        </p>
      </Card>
      <Card title="Kotak REST" note="p95 over 30 s" aside={<span className="num-medium text-fg">{ms(s?.kotak_p95_ms)}</span>}>
        <MetricChart points={series(history, (p) => p.kotak_p95_ms)} height={height} format={tick} value={ms} label="Kotak REST p95" atLeast={100} />
        <p className="mt-2 type-caption text-fg-muted">{s ? `${count(s.kotak_requests_total)} calls · ${count(s.kotak_errors_total)} not 2xx` : ' '}</p>
      </Card>
      <Card title="Dashboard API" note="requests a second" aside={<span className="num-medium text-fg">{rate(s?.http_req_per_sec)}</span>}>
        <MetricChart points={series(history, (p) => p.http_req_per_sec)} height={height} format={tick} value={(v) => `${rate(v)} req/s`} label="Dashboard API requests a second" />
        <p className="mt-2 type-caption text-fg-muted">
          {s ? `p95 ${ms(s.http_p95_ms)} · ${count(s.http_in_flight)} in flight · ${s.http_5xx_pct ?? 0}% 5xx · ${s.http_4xx_pct ?? 0}% 4xx` : ' '}
        </p>
      </Card>
    </>
  );
}
