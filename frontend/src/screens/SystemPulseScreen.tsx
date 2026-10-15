import {
  Activity,
  Cable,
  Database,
  Gauge,
  Globe,
  HeartPulse,
  Layers,
  MemoryStick,
  Radio,
  Send,
  Table2,
  Timer,
} from 'lucide-react';
import type { ReactNode } from 'react';
import type { TelemetrySample } from '../types';
import { useTelemetryStream } from '../hooks/useTelemetryStream';
import { fmt, fmtUptime } from '../lib/format';
import {
  SERIES,
  TICK_BUDGET_MS,
  fmtAge,
  fmtClockIST,
  fmtCount,
  fmtMb,
  fmtMs,
  fmtRate,
  isMarketHoursIST,
  levelFor,
  outcomeLabel,
  sumValues,
  worst,
  type Level,
} from '../lib/pulse';
import { BarList, CARD, Figure, Meter, PulseCard, Section, StatusBadge, StatusRow } from '../components/system/PulseParts';
import { PulseChart } from '../components/system/PulseChart';
import { HostPanel } from '../components/system/HostPanel';

const LEVEL_LABEL: Record<Level, string> = {
  good: 'Healthy',
  warning: 'Degraded',
  critical: 'Down',
  neutral: 'Idle',
};

const SIGNAL_APPLIED = ['accepted', 'edit_applied', 'duplicate_revised', 'sl_updated', 'exit_applied'];

function pnlClass(v: number) {
  return v >= 0 ? 'text-secondary' : 'text-error';
}

function signedInr(v: number) {
  return `${v >= 0 ? '+' : ''}₹${fmt(v)}`;
}

function ConnectionBadge({ connected, stale, ageMs }: { connected: boolean; stale: boolean; ageMs: number | null }) {
  if (connected && !stale) return <StatusBadge level="good" label="Live · 0.5 Hz" />;
  if (ageMs == null) return <StatusBadge level="neutral" label="Connecting…" />;
  return <StatusBadge level="warning" label={`Stale · last sample ${fmtAge(ageMs / 1000)} ago`} />;
}

function KpiRow({ s }: { s: TelemetrySample }) {
  const marketHours = isMarketHoursIST();

  // Engine heartbeat: the tick loop stamps it every 50ms, so any real age
  // means the monitor is stuck inside a tick (or gone).
  const heartbeat = s.engine_heartbeat_age_secs;
  const heartbeatLevel = heartbeat == null ? 'critical' : levelFor(heartbeat, 0.5, 2);
  const tickLevel = levelFor(s.tick_p95_ms, TICK_BUDGET_MS / 2, TICK_BUDGET_MS);

  const memUsedPct = s.host_memory_total_mb > 0
    ? ((s.host_memory_total_mb - s.host_available_mb) / s.host_memory_total_mb) * 100
    : 0;
  const memLevel = levelFor(memUsedPct, 80, 90);

  // A silent feed only matters while the market is open.
  const feedLevel: Level = !marketHours
    ? 'neutral'
    : !s.feed_connected
      ? 'critical'
      : s.feed_subscriptions === 0
        ? 'neutral'
        : levelFor(s.feed_last_tick_age_secs ?? Infinity, 10, 30);
  const feedState = !marketHours
    ? 'Market closed'
    : !s.feed_connected
      ? 'Disconnected'
      : LEVEL_LABEL[feedLevel];

  const httpLevel = levelFor(s.http_5xx_pct, 1, 5);

  return (
    <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <PulseCard
        category="Saturation"
        title="Engine heartbeat"
        icon={<HeartPulse size={16} />}
        level={worst(heartbeatLevel, tickLevel)}
        statusLabel={heartbeat == null ? 'No ticks' : LEVEL_LABEL[worst(heartbeatLevel, tickLevel)]}
        value={<>{fmtMs(s.tick_p95_ms)} <span className="text-sm font-medium text-on-surface-variant">p95 tick</span></>}
        detail={<>Last tick {fmtAge(heartbeat)} ago · {fmtRate(s.ticks_per_sec)} ticks/s</>}
        meter={{
          pct: ((s.tick_p95_ms ?? 0) / TICK_BUDGET_MS) * 100,
          level: tickLevel,
          label: 'Tick p95 against the 50 ms budget',
        }}
        footer={<>Budget {TICK_BUDGET_MS} ms · {s.tick_overruns_window} overruns in 30s · {fmtCount(s.tick_overruns_total)} total</>}
      />
      <PulseCard
        category="Utilization"
        title="Memory"
        icon={<MemoryStick size={16} />}
        level={memLevel}
        statusLabel={LEVEL_LABEL[memLevel]}
        value={<>{fmtMb(s.process_rss_mb)} <span className="text-sm font-medium text-on-surface-variant">server RSS</span></>}
        detail={<>Host {Math.round(memUsedPct)}% used · {fmtMb(s.host_available_mb)} free of {fmtMb(s.host_memory_total_mb)}</>}
        meter={{ pct: memUsedPct, level: memLevel, label: 'Host memory used' }}
        footer={<>Swap {fmtMb(s.host_swap_used_mb)} / {fmtMb(s.host_swap_total_mb)} · CPU {s.process_cpu_pct.toFixed(1)}% server, {s.host_cpu_pct.toFixed(0)}% host</>}
      />
      <PulseCard
        category="Availability"
        title="Price feed"
        icon={<Radio size={16} />}
        level={feedLevel}
        statusLabel={feedState}
        value={<>{fmtRate(s.feed_ticks_per_sec)} <span className="text-sm font-medium text-on-surface-variant">ticks/s</span></>}
        detail={<>Last tick {fmtAge(s.feed_last_tick_age_secs)} ago · {s.feed_subscriptions} subscriptions</>}
        footer={<>{fmtCount(s.feed_ticks_total)} ticks · {s.feed_bridge_restarts_total} bridge restarts since start</>}
      />
      <PulseCard
        category="Rate / error"
        title="HTTP API"
        icon={<Globe size={16} />}
        level={httpLevel}
        statusLabel={LEVEL_LABEL[httpLevel]}
        value={<>{fmtRate(s.http_req_per_sec)} <span className="text-sm font-medium text-on-surface-variant">req/s</span></>}
        detail={<>p95 {fmtMs(s.http_p95_ms)} · {s.http_in_flight} in flight</>}
        meter={{ pct: (s.http_5xx_pct ?? 0) * 10, level: httpLevel, label: 'Share of 5xx responses' }}
        footer={<>{fmtCount(s.http_requests_total)} requests · {s.http_5xx_pct ?? 0}% 5xx · {s.http_4xx_pct ?? 0}% 4xx</>}
      />
    </div>
  );
}

function SignalsSection({ s }: { s: TelemetrySample }) {
  const outcomes = Object.entries(s.signal_outcomes);
  const applied = outcomes.filter(([k]) => SIGNAL_APPLIED.includes(k));
  const rejected = outcomes.filter(([k]) => !SIGNAL_APPLIED.includes(k));
  const toRows = (list: [string, number][]) => list.map(([k, v]) => ({ label: outcomeLabel(k), value: v }));
  return (
    <Section icon={<Send size={15} className="text-primary" />} title="Signal flow">
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-4 mb-4">
        <Figure label="Received" value={fmtCount(s.signals_received_total)} hint={`${fmtRate(s.signals_per_min)} / min`} />
        <Figure label="Telegram messages" value={fmtCount(s.telegram_messages_total)} />
        <Figure label="Unparsed" value={fmtCount(s.telegram_parse.unparsed ?? 0)} hint="watched-chat messages" />
        <Figure label="Dropped (lag)" value={fmtCount(s.signals_lagged_total)} />
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <div>
          <div className="text-[11px] font-bold uppercase tracking-wider text-on-surface-variant mb-2">Applied</div>
          <BarList rows={toRows(applied)} empty="Nothing applied yet." />
        </div>
        <div>
          <div className="text-[11px] font-bold uppercase tracking-wider text-on-surface-variant mb-2">Rejected or unmatched</div>
          <BarList rows={toRows(rejected)} empty="No rejections." />
        </div>
      </div>
    </Section>
  );
}

function OrdersSection({ s, realizedPnl, liveMtmPnl, tradesToday }: {
  s: TelemetrySample;
  realizedPnl: number;
  liveMtmPnl: number;
  tradesToday: number;
}) {
  const r = s.orders_by_result;
  const mutexLevel = levelFor(s.kotak_mutex_wait_p95_ms, 500, 5_000);
  return (
    <Section
      icon={<Activity size={15} className="text-primary" />}
      title="Orders & P&L"
      aside={<span className="text-[11px] font-semibold text-on-surface-variant uppercase">{s.mode}</span>}
    >
      <div className="grid grid-cols-2 sm:grid-cols-3 gap-4">
        <Figure label="Realized P&L" value={<span className={pnlClass(realizedPnl)}>{signedInr(realizedPnl)}</span>} />
        <Figure label="Live MTM" value={<span className={pnlClass(liveMtmPnl)}>{signedInr(liveMtmPnl)}</span>} />
        <Figure label="Trades today" value={tradesToday} hint={`engine booked ${fmtCount(sumValues(s.trades))} since start`} />
        <Figure label="Orders OK" value={fmtCount(r.ok ?? 0)} hint={`${s.orders_by_side.buy ?? 0} buy · ${s.orders_by_side.sell ?? 0} sell`} />
        <Figure label="Rejected" value={fmtCount(r.rejected ?? 0)} />
        <Figure label="Ambiguous" value={fmtCount(r.ambiguous ?? 0)} hint="no broker verdict at send" />
        <Figure label="Session expired" value={fmtCount(r.session_expired ?? 0)} />
        <Figure label="Cancels" value={fmtCount(sumValues(s.order_cancels))} hint={`${s.order_cancels.error ?? 0} failed`} />
        <Figure label="Order-book poll fails" value={fmtCount(s.order_book_poll_failures_total)} />
        <Figure label="LIVE errors" value={fmtCount(s.live_errors_total)} hint={`${s.live_halts_total} positions halted`} />
        <Figure
          label="Kotak lock wait p95"
          value={fmtMs(s.kotak_mutex_wait_p95_ms)}
          hint={mutexLevel === 'good' || mutexLevel === 'neutral' ? 'last 30s' : 'engine queued behind a broker call'}
        />
        <Figure label="Kotak REST" value={fmtCount(s.kotak_requests_total)} hint={`${s.kotak_errors_total} non-2xx`} />
      </div>
    </Section>
  );
}

function SaturationSection({ s }: { s: TelemetrySample }) {
  const dbPct = s.db_queue_capacity > 0 ? (s.db_queue_depth / s.db_queue_capacity) * 100 : 0;
  const dbLevel = levelFor(dbPct, 10, 50);
  const poolInUse = Math.max(0, s.db_pool_size - s.db_pool_idle);
  const rows: { label: string; value: string; pct?: number; level?: Level }[] = [
    { label: 'SQLite writer queue', value: `${s.db_queue_depth} / ${s.db_queue_capacity}`, pct: dbPct, level: dbLevel },
    {
      label: 'SQLite pool in use',
      value: `${poolInUse} / ${s.db_pool_size}`,
      pct: s.db_pool_size ? (poolInUse / s.db_pool_size) * 100 : 0,
      level: 'good',
    },
  ];
  return (
    <Section icon={<Layers size={15} className="text-primary" />} title="Saturation">
      <div className="space-y-3">
        {rows.map((row) => (
          <div key={row.label} className="space-y-1">
            <div className="flex justify-between text-xs">
              <span className="text-on-surface">{row.label}</span>
              <span className="tabular-nums font-semibold text-on-surface">{row.value}</span>
            </div>
            <Meter pct={row.pct ?? 0} level={row.level ?? 'good'} label={row.label} />
          </div>
        ))}
      </div>
      <div className="grid grid-cols-2 sm:grid-cols-3 gap-4 mt-4">
        <Figure label="DB write p95" value={fmtMs(s.db_write_p95_ms)} hint={`${s.db_write_errors_total} write errors`} />
        <Figure label="Tokio tasks" value={s.tokio_alive_tasks} hint={`${s.tokio_workers} workers · ${s.tokio_global_queue} queued`} />
        <Figure label="Stream clients" value={s.sse_clients} hint="logs + this page" />
        <Figure label="Load average" value={s.load_average[0].toFixed(2)} hint={`${s.load_average[1].toFixed(2)} · ${s.load_average[2].toFixed(2)} on ${s.cpu_cores} cores`} />
        <Figure label="Server uptime" value={fmtUptime(s.process_uptime_secs)} />
        <Figure label="Virtual memory" value={fmtMb(s.process_virtual_mb)} />
      </div>
    </Section>
  );
}

function ConnectionsSection({ s }: { s: TelemetrySample }) {
  const marketHours = isMarketHoursIST();
  const live = s.mode === 'live';
  const kotakLevel: Level = s.kotak_session_healthy ? 'good' : s.kotak_connected ? 'warning' : marketHours ? 'critical' : 'neutral';
  const kotakState = s.kotak_session_healthy ? 'Healthy' : s.kotak_connected ? 'Session failing' : 'Not logged in';
  const relogins = s.kotak_relogins;
  const upstoxLevel: Level = !s.upstox_configured
    ? 'neutral'
    : !marketHours
      ? 'neutral'
      : levelFor(s.upstox_last_success_age_secs ?? Infinity, 180, 600);
  return (
    <Section icon={<Cable size={15} className="text-primary" />} title="Connections">
      <StatusRow
        name="Kotak session"
        level={kotakLevel}
        state={kotakState}
        detail={`Watchdog re-logins: ${relogins.ok ?? 0} ok, ${relogins.error ?? 0} failed`}
      />
      <StatusRow
        name="Price feed"
        level={s.feed_connected ? 'good' : marketHours ? 'critical' : 'neutral'}
        state={s.feed_connected ? 'Connected' : marketHours ? 'Disconnected' : 'Closed'}
        detail={`Last tick ${fmtAge(s.feed_last_tick_age_secs)} ago`}
      />
      <StatusRow
        name="Telegram"
        level={s.telegram_connected ? 'good' : 'critical'}
        state={s.telegram_connected ? 'Listening' : 'Not running'}
        detail="The update loop itself, not the login state"
      />
      <StatusRow
        name="Upstox market data"
        level={upstoxLevel}
        state={!s.upstox_configured ? 'Not configured' : s.upstox_last_success_age_secs == null ? 'No data yet' : `${fmtAge(s.upstox_last_success_age_secs)} ago`}
        detail={`${s.upstox_errors_total} failed calls since start`}
      />
      <StatusRow
        name="LIVE startup reconcile"
        level={!live ? 'neutral' : s.live_startup_reconciled ? 'good' : 'critical'}
        state={!live ? 'Paper mode' : s.live_startup_reconciled ? 'Reconciled' : 'Not trading yet'}
        detail="LIVE never trades on unverified broker state"
      />
      <StatusRow
        name="Kill switch"
        level={s.kill_switch ? 'warning' : 'good'}
        state={s.kill_switch ? 'Active — entries blocked' : 'Off'}
      />
    </Section>
  );
}

function PositionsTable({ s }: { s: TelemetrySample }) {
  const rows = s.positions_table;
  const counts = s.positions;
  return (
    <Section
      icon={<Table2 size={15} className="text-primary" />}
      title="Tracked positions"
      aside={counts && (
        <span className="text-[11px] text-on-surface-variant font-semibold">
          {counts.waiting} waiting · {counts.active} active · {counts.target1} past T1 · {counts.closed} closed
        </span>
      )}
    >
      {rows == null ? (
        <div className="text-xs text-on-surface-variant">Engine was busy at the last sample — table refreshes on the next one.</div>
      ) : rows.length === 0 ? (
        <div className="text-xs text-on-surface-variant">No tracked positions.</div>
      ) : (
        <div className="overflow-x-auto -mx-4">
          <table className="w-full text-xs">
            <thead>
              <tr className="text-left text-on-surface-variant border-b border-outline-variant">
                <th className="px-4 py-2 font-semibold">Contract</th>
                <th className="px-2 py-2 font-semibold">State</th>
                <th className="px-2 py-2 font-semibold text-right">Qty</th>
                <th className="px-2 py-2 font-semibold text-right">Avg</th>
                <th className="px-2 py-2 font-semibold text-right">SL</th>
                <th className="px-2 py-2 font-semibold text-right">LTP</th>
                <th className="px-2 py-2 font-semibold text-right">MTM</th>
                <th className="px-4 py-2 font-semibold text-right">LTP age</th>
              </tr>
            </thead>
            <tbody className="tabular-nums whitespace-nowrap">
              {rows.map((p) => (
                <tr key={p.id} className="border-b border-outline-variant last:border-0">
                  <td className="px-4 py-2 text-on-surface font-medium whitespace-nowrap">
                    {[p.instrument, p.strike, p.option_type].filter((x) => x != null && x !== '').join(' ')}
                    {p.halted && <span className="ml-2 text-error font-semibold">HALTED</span>}
                  </td>
                  <td className="px-2 py-2 text-on-surface-variant whitespace-nowrap">{p.state}</td>
                  <td className="px-2 py-2 text-right">{p.qty}</td>
                  <td className="px-2 py-2 text-right">{p.avg_price > 0 ? fmt(p.avg_price) : fmt(p.entry_price)}</td>
                  <td className="px-2 py-2 text-right">{fmt(p.current_sl)}</td>
                  <td className="px-2 py-2 text-right">{p.ltp != null ? fmt(p.ltp) : '—'}</td>
                  <td className={`px-2 py-2 text-right ${p.mtm != null ? pnlClass(p.mtm) : ''}`}>
                    {p.mtm != null ? signedInr(p.mtm) : '—'}
                  </td>
                  <td className="px-4 py-2 text-right">{fmtAge(p.ltp_age_secs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Section>
  );
}

function LatencyTable({ title, icon, rows, keyLabel, errorLabel }: {
  title: string;
  icon: ReactNode;
  rows: TelemetrySample['kotak_endpoints'];
  keyLabel: string;
  errorLabel: string;
}) {
  return (
    <Section icon={icon} title={title} aside={<span className="text-[11px] text-on-surface-variant">p95 over 30s</span>}>
      {rows.length === 0 ? (
        <div className="text-xs text-on-surface-variant">No calls yet.</div>
      ) : (
        <div className="overflow-x-auto -mx-4">
          <table className="w-full text-xs">
            <thead>
              <tr className="text-left text-on-surface-variant border-b border-outline-variant">
                <th className="px-4 py-2 font-semibold">{keyLabel}</th>
                <th className="px-2 py-2 font-semibold text-right">Calls</th>
                <th className="px-2 py-2 font-semibold text-right">{errorLabel}</th>
                <th className="px-4 py-2 font-semibold text-right">p95</th>
              </tr>
            </thead>
            <tbody className="tabular-nums whitespace-nowrap">
              {rows.map((r) => (
                <tr key={r.endpoint} className="border-b border-outline-variant last:border-0">
                  <td className="px-4 py-2 text-on-surface font-mono whitespace-normal break-all">{r.endpoint}</td>
                  <td className="px-2 py-2 text-right">{fmtCount(r.total)}</td>
                  <td className={`px-2 py-2 text-right ${r.errors > 0 ? 'text-error font-semibold' : ''}`}>{fmtCount(r.errors)}</td>
                  <td className="px-4 py-2 text-right">{fmtMs(r.p95_ms)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Section>
  );
}

export function SystemPulseScreen({ serverBase, realizedPnl, liveMtmPnl, tradesToday }: {
  serverBase: string;
  realizedPnl: number;
  liveMtmPnl: number;
  tradesToday: number;
}) {
  const { history, latest: s, connected, stale, ageMs, error } = useTelemetryStream(serverBase);

  return (
    <div className="space-y-4">
      <div className={`${CARD} flex flex-wrap items-center justify-between gap-3 px-4 py-3`}>
        <div className="min-w-0">
          <div className="text-sm font-semibold text-on-surface">System pulse</div>
          <div className="text-xs text-on-surface-variant">
            Engine, broker, feed and server stats, sampled every 2s. Charts keep the last 15 minutes.
            {s && <> Last sample {fmtClockIST(s.ts)} IST.</>}
          </div>
        </div>
        <div className="flex items-center gap-3">
          {s && (
            <span className="px-2 py-0.5 rounded text-[11px] font-bold uppercase tracking-wide bg-surface-container text-on-surface">
              {s.mode}
            </span>
          )}
          <ConnectionBadge connected={connected} stale={stale} ageMs={ageMs} />
        </div>
      </div>

      {error && (
        <div className="bg-error-container border border-error text-on-error-container rounded-lg px-4 py-3 text-sm">{error}</div>
      )}

      {!s ? (
        <div className={`${CARD} px-4 py-10 text-sm text-on-surface-variant text-center`}>
          Waiting for the first sample from the backend…
        </div>
      ) : (
        <div className={`space-y-4 transition-opacity ${stale ? 'opacity-60' : ''}`}>
          <KpiRow s={s} />

          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <PulseChart
              title="CPU"
              subtitle="Percent; server is of one core, host is all cores"
              data={history}
              series={[
                { key: 'process_cpu_pct', label: 'Server', color: SERIES.primary },
                { key: 'host_cpu_pct', label: 'Host', color: SERIES.secondary },
              ]}
              format={(v) => `${v.toFixed(0)}%`}
              current={`${s.process_cpu_pct.toFixed(1)}%`}
            />
            <PulseChart
              title="Server memory"
              subtitle="Resident set size, MB"
              data={history}
              series={[{ key: 'process_rss_mb', label: 'RSS', color: SERIES.primary }]}
              format={(v) => `${v.toFixed(0)}`}
              current={fmtMb(s.process_rss_mb)}
            />
            <PulseChart
              title="Tick duration"
              subtitle="Engine tick p95 over 30s, ms (budget 50)"
              data={history}
              series={[{ key: 'tick_p95_ms', label: 'p95', color: SERIES.primary }]}
              format={(v) => (v >= 10 ? v.toFixed(0) : v.toFixed(1))}
              current={fmtMs(s.tick_p95_ms)}
            />
            <PulseChart
              title="Price-feed ticks"
              subtitle="LTP ticks per second"
              data={history}
              series={[{ key: 'feed_ticks_per_sec', label: 'ticks/s', color: SERIES.primary }]}
              format={(v) => fmtRate(v)}
              current={`${fmtRate(s.feed_ticks_per_sec)}/s`}
            />
            <PulseChart
              title="Kotak REST latency"
              subtitle="p95 over 30s, ms"
              data={history}
              series={[{ key: 'kotak_p95_ms', label: 'p95', color: SERIES.primary }]}
              format={(v) => v.toFixed(0)}
              current={fmtMs(s.kotak_p95_ms)}
            />
            <PulseChart
              title="HTTP API traffic"
              subtitle="Dashboard requests per second"
              data={history}
              series={[{ key: 'http_req_per_sec', label: 'req/s', color: SERIES.primary }]}
              format={(v) => fmtRate(v)}
              current={`${fmtRate(s.http_req_per_sec)}/s`}
            />
          </div>

          <div className="grid gap-4 lg:grid-cols-2">
            <SignalsSection s={s} />
            <OrdersSection s={s} realizedPnl={realizedPnl} liveMtmPnl={liveMtmPnl} tradesToday={tradesToday} />
          </div>

          <div className="grid gap-4 lg:grid-cols-2">
            <SaturationSection s={s} />
            <ConnectionsSection s={s} />
          </div>

          <PositionsTable s={s} />

          <div className="grid gap-4 lg:grid-cols-2">
            <LatencyTable
              title="Kotak REST by endpoint"
              icon={<Timer size={15} className="text-primary" />}
              rows={s.kotak_endpoints}
              keyLabel="Endpoint"
              errorLabel="Non-2xx"
            />
            <LatencyTable
              title="Dashboard API by route"
              icon={<Gauge size={15} className="text-primary" />}
              rows={s.http_routes}
              keyLabel="Route"
              errorLabel="5xx"
            />
          </div>
        </div>
      )}

      <HostPanel serverBase={serverBase} />

      <div className="flex items-center gap-2 text-[11px] text-on-surface-variant">
        <Database size={12} />
        <span>
          Prometheus text format at <code className="font-mono">/metrics</code> for any external scraper.
        </span>
      </div>
    </div>
  );
}
