import type { ReactNode } from 'react';
import { getLatency } from '../lib/api';
import { dayLabel, todayIst } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { LatencyAnswer, LatencyFigures, LatencySummary } from '../lib/types';
import { Row, Section } from '../settings/parts';

/** "182 ms", "1.4 s". */
function ms(v: number): string {
  const a = Math.abs(v);
  return a < 1000 ? `${Math.round(a)} ms` : `${(a / 1000).toFixed(1)} s`;
}

/** "Kotak 137 ms", "Upstox 40 ms" — who had the price first, and by how much. */
function lead(v: number): string {
  return `${v >= 0 ? 'Kotak' : 'Upstox'} ${ms(v)}`;
}

/** A feed that has said nothing for this long is called quiet. */
const QUIET_MS = 10_000;

type Measure = keyof LatencyFigures;
const MEASURES: { key: Measure; title: string; show: (v: number) => string }[] = [
  { key: 'upstox_delay', title: 'Upstox delay', show: ms },
  { key: 'kotak_delay', title: 'Kotak delay', show: ms },
  { key: 'kotak_lead', title: 'First by', show: lead },
];

/** The live median, and the day's average under it. */
function Cell({ live, day, show, label, desktop }: { live: LatencySummary | null; day: LatencySummary | null; show: (v: number) => string; label: string; desktop: boolean }) {
  const today = day ? `avg ${show(day.mean)} today` : 'none today';
  if (!desktop) {
    return (
      <div className="tnum">
        <span className="text-fg-muted">{label} </span>
        <span className="text-fg">{live ? show(live.p50) : '—'}</span>
        <span className="text-fg-muted"> · {today}</span>
      </div>
    );
  }
  return (
    <div className="min-w-0" title={live ? `Last minute: median ${show(live.p50)}, 95% within ${show(live.p95)}, ${live.n} samples` : 'No samples in the last minute'}>
      <div className="num-medium text-fg">{live ? show(live.p50) : '—'}</div>
      <div className="type-caption text-fg-muted tnum">{today}</div>
    </div>
  );
}

function Line({ title, caption, minute, day, desktop }: { title: ReactNode; caption?: ReactNode; minute: LatencyFigures; day: LatencyFigures; desktop: boolean }) {
  if (!desktop) {
    return (
      <Row
        title={title}
        caption={
          <>
            {caption && <div>{caption}</div>}
            {MEASURES.map((m) => (
              <Cell key={m.key} label={m.title} live={minute[m.key]} day={day[m.key]} show={m.show} desktop={false} />
            ))}
          </>
        }
      />
    );
  }
  return (
    <div className="grid grid-cols-[minmax(0,1.5fr)_repeat(3,minmax(0,1fr))] items-center gap-4 border-t border-line py-3 pr-4 pl-4 first:border-t-0">
      <div className="min-w-0">
        <div className="truncate type-body-strong text-fg">{title}</div>
        {caption && <div className="mt-0.5 type-caption text-fg-muted">{caption}</div>}
      </div>
      {MEASURES.map((m) => (
        <Cell key={m.key} label={m.title} live={minute[m.key]} day={day[m.key]} show={m.show} desktop />
      ))}
    </div>
  );
}

function Header() {
  return (
    <div className="grid grid-cols-[minmax(0,1.5fr)_repeat(3,minmax(0,1fr))] gap-4 border-b border-line py-2 pr-4 pl-4 type-caption text-fg-muted">
      <span />
      {MEASURES.map((m) => (
        <span key={m.key}>{m.title}</span>
      ))}
    </div>
  );
}

/** "Upstox quiet 14 s", when a feed has gone quiet for an instrument. */
function quiet(i: LatencyAnswer['instruments'][number]): string | null {
  const parts = [
    i.upstox_age_ms == null ? 'no Upstox tick yet' : i.upstox_age_ms > QUIET_MS ? `Upstox quiet ${ms(i.upstox_age_ms)}` : null,
    i.kotak_age_ms == null ? 'no Kotak tick yet' : i.kotak_age_ms > QUIET_MS ? `Kotak quiet ${ms(i.kotak_age_ms)}` : null,
  ].filter(Boolean);
  return parts.length ? parts.join(' · ') : null;
}

function status(d: LatencyAnswer): { title: string; caption: string } {
  if (d.status === 'not_configured') return { title: 'Not measuring', caption: "UPSTOX_ANALYTICS_TOKEN isn't set on the server; Upstox's trade times are what both feeds are timed against." };
  if (d.status === 'market_closed') {
    return {
      title: 'Market closed',
      caption: d.day ? `Figures from ${d.day === todayIst() ? 'today' : dayLabel(d.day)}; measuring starts again at the next open.` : 'Measuring starts at the next open.',
    };
  }
  const upstox = d.upstox_error ? `Upstox down (${d.upstox_error}), reconnecting: no new samples until it's back` : 'Upstox streaming';
  const kotak = d.kotak_streaming ? 'Kotak streaming' : 'Kotak not streaming (no session, or its feed is down): no Kotak figures until it is';
  const lagged = d.kotak_lagged ? `${d.kotak_lagged} Kotak ticks skipped while busy` : null;
  return { title: 'Measuring', caption: [upstox, kotak, lagged].filter(Boolean).join(' · ') };
}

/**
 * How late prices reach the server, live (the last minute) and on average
 * over the day: the indices, their near futures and the contracts held.
 * Admin only.
 */
export function FeedLatency({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const answer = usePoll(() => getLatency(serverBase), 2_000, serverBase);
  const d = answer.data;
  const s = d && status(d);
  const instruments = d?.instruments ?? [];

  return (
    <Section
      title="Feed latency"
      lead="How late prices reach the server. Upstox is timed against the exchange's time of each trade; Kotak, whose own clock is whole seconds, against the same trade on Upstox. First by is which feed had the price sooner. Big figures are the last minute's median, small ones the day's average. Market hours only."
    >
      {d == null || s == null ? (
        <Row title={answer.error ? `Couldn't load the latency: ${answer.error}` : 'Loading…'} />
      ) : (
        <>
          <Row title={s.title} caption={s.caption} />
          {d.classes.some((c) => c.day.upstox_delay || c.minute.upstox_delay) && (
            <div className="border-t border-line">
              {desktop && <Header />}
              {d.classes.map((c) => (
                <Line key={c.class} title={c.label} minute={c.minute} day={c.day} desktop={desktop} />
              ))}
            </div>
          )}
          {instruments.length > 0 && (
            <div className="border-t border-line">
              {desktop && <Header />}
              {instruments.map((i) => (
                <Line key={i.upstox_key} title={i.label} caption={quiet(i)} minute={i.minute} day={i.day} desktop={desktop} />
              ))}
            </div>
          )}
        </>
      )}
    </Section>
  );
}
