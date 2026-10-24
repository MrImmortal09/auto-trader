import { useEffect, useState } from 'react';
import type { ExecuteFrame } from '../types';
import { apiUrl } from '../lib/api';
import { getToken } from '../lib/auth';

/** No frame for this long (several missed 250 ms beats) and the stream is shown as stale. */
export const STREAM_STALE_AFTER_MS = 2_000;

/**
 * Live Execute-screen frames for `index` (SSE, one every 250 ms): the
 * ladder of `expiry` (the nearest when null) around `centre` (ATM when
 * null). Switching any of them reopens the stream; the last frame is kept
 * meanwhile so the page never blanks. `now` ticks so the caller can tell how
 * old the frame is.
 */
export function useExecuteStream(serverBase: string, index: string, expiry: string | null = null, centre: number | null = null) {
  const [frame, setFrame] = useState<ExecuteFrame | null>(null);
  const [connected, setConnected] = useState(false);
  // Local receipt time, so browser/VM clock skew can't make a live stream look stale.
  const [receivedAt, setReceivedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    const token = getToken();
    const params = new URLSearchParams({ index });
    if (expiry) params.set('expiry', expiry);
    if (centre != null) params.set('centre', String(centre));
    if (token) params.set('token', token);
    const es = new EventSource(`${apiUrl(serverBase, '/api/execute/stream')}?${params}`);
    es.onopen = () => setConnected(true);
    es.onmessage = (e: MessageEvent<string>) => {
      let next: ExecuteFrame;
      try {
        next = JSON.parse(e.data) as ExecuteFrame;
      } catch {
        return;
      }
      setConnected(true);
      setFrame(next);
      setReceivedAt(Date.now());
    };
    // EventSource reconnects on its own; this only flips the badge meanwhile.
    es.onerror = () => setConnected(false);
    return () => es.close();
  }, [serverBase, index, expiry, centre]);

  const ageMs = receivedAt == null ? null : Math.max(0, now - receivedAt);
  const stale = !connected || ageMs == null || ageMs > STREAM_STALE_AFTER_MS;
  return { frame, connected, stale, ageMs, now };
}
