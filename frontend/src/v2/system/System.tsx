import { useState, type ReactNode } from 'react';
import { ChevronLeft, Download } from 'lucide-react';
import { useTelemetryStream } from '../../hooks/useTelemetryStream';
import { getSystem } from '../lib/api';
import { hhmm, plural } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { HealthSet, Level, SystemAnswer } from '../lib/types';
import { Button } from '../ui/Button';
import { EngineLog, LogPreview } from './EngineLog';
import { HealthTile } from './HealthTile';
import { downloadLog } from './logLines';
import { TICK_BUDGET_MS, TickChart } from './TickChart';
import { useEngineLog } from './useEngineLog';

const COUNT = new Intl.NumberFormat('en-IN');
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const CLOCK = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });

/** "8.4 ms", "142 ms", "1.2 s". */
function ms(v: number | null | undefined): string {
  if (v == null) return '—';
  if (v >= 1000) return `${(v / 1000).toFixed(1)} s`;
  return v >= 10 ? `${Math.round(v)} ms` : `${v.toFixed(1)} ms`;
}

/** "0.3 s", "12 s", "4 min". */
function ago(secs: number | null | undefined): string {
  if (secs == null) return '—';
  if (secs < 10) return `${secs.toFixed(1)} s`;
  if (secs < 120) return `${Math.round(secs)} s`;
  return `${Math.round(secs / 60)} min`;
}

/** "Mar 2027" from `2027-03-14`. */
function monthYear(day: string): string {
  return `${MONTHS[Number(day.slice(5, 7)) - 1]} ${day.slice(0, 4)}`;
}

function Card({ title, note, children }: { title: string; note?: ReactNode; children: ReactNode }) {
  return (
    <section className="flex min-w-0 flex-col rounded-12 bg-panel p-4">
      <div className="flex items-baseline gap-2">
        <h3 className="type-title text-fg">{title}</h3>
        {note && <span className="type-caption text-fg-muted">{note}</span>}
      </div>
      <div className="mt-2 flex-1">{children}</div>
    </section>
  );
}

function CountRow({ label, value, alarm = false }: { label: string; value: ReactNode; alarm?: boolean }) {
  return (
    <div className="flex items-center justify-between py-1.5">
      <span className="type-body text-fg-muted">{label}</span>
      <span className={`num-medium ${alarm ? 'text-loss' : 'text-fg'}`}>{value}</span>
    </div>
  );
}

const n = (v: number | undefined) => (v == null ? '—' : COUNT.format(v));

/** The tiles: engine, Kotak, price feed, Telegram and Upstox. */
function tiles(t: ReturnType<typeof useTelemetryStream>, d: SystemAnswer | null, level: (k: keyof HealthSet) => Level, health: HealthSet | null, compact: boolean) {
  const s = t.latest;
  const series = (pick: (p: (typeof t.history)[number]) => number | null) => t.history.map(pick);
  const kotakWord = health?.kotak.level === 'ok' ? 'Healthy' : health?.kotak.label ?? '—';
  const upstoxAge = s?.upstox_last_success_age_secs;
  const upstoxOn = d?.upstox.configured ?? s?.upstox_configured ?? false;
  // When it last polled: the sample's own time less the age it reports.
  const upstox = !upstoxOn ? 'Not set up' : s && upstoxAge != null ? `Polled ${CLOCK.format(new Date((s.ts - upstoxAge) * 1000))}` : 'No data yet';
  return [
    <HealthTile
      key="engine"
      name="Engine"
      level={level('engine')}
      value={ms(s?.tick_p95_ms)}
      detail={`Tick p95 · budget ${TICK_BUDGET_MS} ms${compact ? '' : ` · heartbeat ${ago(s?.engine_heartbeat_age_secs)} ago`}`}
      series={series((p) => p.tick_p95_ms)}
      compact={compact}
    />,
    <HealthTile
      key="kotak"
      name="Kotak"
      level={level('kotak')}
      value={kotakWord}
      detail={`REST p95 ${ms(s?.kotak_p95_ms)}${compact ? '' : ` · ${plural(d?.kotak.relogins ?? 0, 're-login')}${d?.kotak.logged_in_at ? ` · logged in ${d.kotak.logged_in_at}` : ''}`}`}
      series={series((p) => p.kotak_p95_ms)}
      compact={compact}
    />,
    <HealthTile
      key="feed"
      name="Price feed"
      level={level('feed')}
      value={s ? `${COUNT.format(Math.round(s.feed_ticks_per_sec))} ticks/s` : '—'}
      detail={`Last tick ${ago(s?.feed_last_tick_age_secs)} ago${compact ? '' : ` · ${n(s?.feed_subscriptions)} subscriptions`}`}
      series={series((p) => p.feed_ticks_per_sec)}
      compact={compact}
    />,
    <HealthTile
      key="telegram"
      name="Telegram"
      level={level('telegram')}
      value={d?.telegram.listening != null ? plural(d.telegram.listening, 'channel') : s?.telegram_connected ? 'Listening' : 'Not running'}
      detail={`${n(d?.telegram.messages)} messages today${d?.telegram.last_at && !compact ? ` · last at ${hhmm(d.telegram.last_at)}` : ''}`}
      series={series((p) => p.signals_per_min)}
      compact={compact}
    />,
    <HealthTile
      key="upstox"
      name="Upstox"
      level={level('upstox')}
      value={upstox}
      detail={
        compact
          ? d?.upstox.token_expires
            ? `Token valid till ${monthYear(d.upstox.token_expires)}`
            : 'Option chain and VWAP'
          : `Option chain and VWAP${d?.upstox.token_expires ? ` · token valid till ${monthYear(d.upstox.token_expires)}` : ''}`
      }
      compact={compact}
    />,
  ];
}

/**
 * System: the five health tiles, today's calls and orders, the engine's
 * tick time and its log. Live from `/api/telemetry/*` and `/api/logs/*`;
 * the day's counts from `/api/v2/system`.
 */
export function System({ serverBase, desktop, health }: { serverBase: string; desktop: boolean; health: HealthSet | null }) {
  const t = useTelemetryStream(serverBase);
  const sys = usePoll(() => getSystem(serverBase), 15_000, serverBase);
  const log = useEngineLog(serverBase);
  const [fullLog, setFullLog] = useState(false);
  const d = sys.data;
  const level = (k: keyof HealthSet): Level => health?.[k]?.level ?? 'off';
  const live = d?.mode === 'LIVE';
  const sig = d?.signals;
  const ord = d?.orders;
  const lateStart = live && d?.started_today && d.started_today > '09:15' ? d.started_today : null;
  const caption = t.stale ? 'Waiting for the server’s next sample…' : `Live · sampled every 2 s${desktop ? ' · charts show the last 15 minutes' : ''}`;

  if (!desktop && fullLog) {
    return (
      <div className="flex flex-col gap-3 p-4">
        <div className="flex items-center justify-between">
          <button type="button" onClick={() => setFullLog(false)} className="flex items-center gap-1 type-label text-fg-muted hover:text-fg">
            <ChevronLeft size={16} /> System
          </button>
          <Button size="sm" icon={<Download size={14} />} onClick={() => downloadLog(log.lines)} disabled={log.lines.length === 0}>
            Download
          </Button>
        </div>
        <h2 className="type-title text-fg">Engine log</h2>
        <EngineLog lines={log.lines} connected={log.connected} compact />
      </div>
    );
  }

  if (!desktop) {
    return (
      <div className="flex flex-col gap-3 p-4">
        <p className="type-caption text-fg-muted">{caption}</p>
        <div className="grid grid-cols-2 gap-3">{tiles(t, d, level, health, true)}</div>
        <Card title="Today">
          <CountRow label="Calls parsed" value={n(sig?.calls)} />
          <CountRow label="Traded" value={n(sig?.traded)} />
          <CountRow label="Orders filled" value={ord ? `${n(ord.filled)} of ${n(ord.sent)}` : '—'} />
          <CountRow label="Orders rejected" value={n(ord?.rejected)} alarm={(ord?.rejected ?? 0) > 0} />
        </Card>
        <section className="rounded-12 bg-panel p-4">
          <div className="flex items-center justify-between">
            <h3 className="type-title text-fg">Engine log</h3>
            <Button size="sm" kind="ghost" onClick={() => setFullLog(true)}>
              Open full log
            </Button>
          </div>
          <LogPreview lines={log.lines} count={5} />
        </section>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4 p-6">
      <div className="flex items-center justify-between">
        <p className="type-caption text-fg-muted">{caption}</p>
        <Button size="md" icon={<Download size={14} />} onClick={() => downloadLog(log.lines)} disabled={log.lines.length === 0}>
          Download log
        </Button>
      </div>
      <div className="grid grid-cols-5 gap-4">{tiles(t, d, level, health, false)}</div>
      <div className="grid grid-cols-3 gap-4">
        <Card title="Signals today">
          <CountRow label="Calls parsed" value={n(sig?.calls)} />
          <CountRow label="Traded" value={n(sig?.traded)} />
          <CountRow label="Waiting for entry" value={n(sig?.waiting)} />
          <CountRow label="Expired before entry" value={n(sig?.expired)} />
          <CountRow label="Indicator disagreed" value={n(sig?.indicator)} />
          <CountRow label="Not index options" value={n(sig?.not_index)} />
          {(sig?.other ?? 0) > 0 && <CountRow label="Not taken, other reasons" value={n(sig?.other)} />}
        </Card>
        <Card title="Orders today" note={!live && d ? 'Paper fills at the market price' : lateStart ? `since the server started at ${lateStart}` : undefined}>
          <CountRow label="Sent" value={n(ord?.sent)} />
          <CountRow label="Filled" value={n(ord?.filled)} />
          <CountRow label="Rejected" value={n(ord?.rejected)} alarm={(ord?.rejected ?? 0) > 0} />
          <CountRow label="Cancelled" value={n(ord?.cancelled)} />
          <CountRow label="Kotak REST p95" value={ms(t.latest?.kotak_p95_ms)} />
          <CountRow label="Order-book poll failures" value={n(ord?.poll_failures)} alarm={(ord?.poll_failures ?? 0) > 0} />
        </Card>
        <Card title="Engine tick p95" note="ms · last 15 minutes">
          <TickChart history={t.history} height={190} />
        </Card>
      </div>
      <section className="rounded-12 bg-panel p-4">
        <EngineLog lines={log.lines} connected={log.connected} compact={false} />
      </section>
    </div>
  );
}
