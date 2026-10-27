import { useEffect, useState } from 'react';
import type { TelemetryPoint, TelemetrySample, TelemetrySnapshot } from '../types';
import { apiFetch, apiFetchWith, apiUrl } from '../lib/api';
import { getToken } from '../lib/auth';

/** Matches the backend ring: 15 minutes at one sample per 2s. */
const MAX_POINTS = 450;
/** No sample for this long (3 missed beats) and the feed is shown as stale. */
export const STALE_AFTER_MS = 6_000;

function pointOf(s: TelemetrySample): TelemetryPoint {
  return {
    ts: s.ts,
    process_cpu_pct: s.process_cpu_pct,
    host_cpu_pct: s.host_cpu_pct,
    process_rss_mb: s.process_rss_mb,
    host_available_mb: s.host_available_mb,
    tick_p95_ms: s.tick_p95_ms,
    ticks_per_sec: s.ticks_per_sec,
    feed_ticks_per_sec: s.feed_ticks_per_sec,
    http_req_per_sec: s.http_req_per_sec,
    http_p95_ms: s.http_p95_ms,
    kotak_p95_ms: s.kotak_p95_ms,
    db_queue_depth: s.db_queue_depth,
    signals_per_min: s.signals_per_min,
  };
}

/** Append points newer than what `prev` already holds, capped to the ring size. */
function mergePoints(prev: TelemetryPoint[], incoming: TelemetryPoint[]): TelemetryPoint[] {
  const lastTs = prev.length ? prev[prev.length - 1].ts : 0;
  const fresh = incoming.filter((p) => p.ts > lastTs);
  if (!fresh.length) return prev;
  const next = prev.concat(fresh);
  return next.length > MAX_POINTS ? next.slice(next.length - MAX_POINTS) : next;
}

/**
 * Live system pulse: backfills the backend's 15-minute history once, then
 * appends each 2s sample from the SSE stream. `now` ticks every second so the
 * caller can show how old the latest sample is.
 */
export function useTelemetryStream(serverBase: string, token?: string | null) {
  // The stored sign-in's token unless `token` says otherwise.
  const sendToken = token === undefined ? getToken() : token;
  const [history, setHistory] = useState<TelemetryPoint[]>([]);
  const [latest, setLatest] = useState<TelemetrySample | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState('');
  // Local receipt time, not the sample's server timestamp, so clock skew
  // between browser and VM can't make a live stream look stale.
  const [receivedAt, setReceivedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    let cancelled = false;

    // A sample can arrive on the stream before the snapshot does: keep the
    // newer `latest`, and merge the backfill *under* anything already streamed.
    const acceptLatest = (s: TelemetrySample) => {
      setLatest((prev) => (prev && prev.ts >= s.ts ? prev : s));
      setReceivedAt(Date.now());
    };

    (token === undefined ? apiFetch(serverBase, '/api/telemetry/snapshot') : apiFetchWith(sendToken, serverBase, '/api/telemetry/snapshot'))
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then((snap: TelemetrySnapshot) => {
        if (cancelled) return;
        setError('');
        setHistory((prev) => mergePoints(snap.history, prev));
        if (snap.latest) acceptLatest(snap.latest);
      })
      .catch((e) => {
        if (!cancelled) setError(`Could not load stats history: ${String(e)}`);
      });

    const es = new EventSource(
      apiUrl(serverBase, '/api/telemetry/stream') + (sendToken ? `?token=${encodeURIComponent(sendToken)}` : ''),
    );
    es.onopen = () => setConnected(true);
    es.onmessage = (e: MessageEvent<string>) => {
      let sample: TelemetrySample;
      try {
        sample = JSON.parse(e.data) as TelemetrySample;
      } catch {
        return;
      }
      setConnected(true);
      acceptLatest(sample);
      setHistory((prev) => mergePoints(prev, [pointOf(sample)]));
    };
    // EventSource reconnects on its own; this only flips the badge meanwhile.
    es.onerror = () => setConnected(false);

    return () => {
      cancelled = true;
      es.close();
    };
  }, [serverBase, token, sendToken]);

  const ageMs = receivedAt == null ? null : Math.max(0, now - receivedAt);
  const stale = ageMs == null || ageMs > STALE_AFTER_MS;
  return { history, latest, connected, stale, ageMs, error };
}
