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
 * old the frame is. The token goes in the URL (an `EventSource` can't set
 * headers): the stored passkey token unless `token` says otherwise — v2
 * passes a phone sign-in's token, or null for a visitor.
 */
export function useExecuteStream(serverBase: string, index: string, expiry: string | null = null, centre: number | null = null, token?: string | null) {
  const [frame, setFrame] = useState<ExecuteFrame | null>(null);
  const [connected, setConnected] = useState(false);
  // Local receipt time, so browser/VM clock skew can't make a live stream look
  // stale — kept per index and expiry, so after a switch the stream counts as
  // stale (and nothing is bought off the previous one's frame) until the new
  // stream's first frame arrives.
  const key = `${serverBase}|${index}|${expiry ?? ''}`;
  const [received, setReceived] = useState<{ key: string; at: number } | null>(null);
  const receivedAt = received?.key === key ? received.at : null;
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(timer);
  }, []);

  const sendToken = token === undefined ? getToken() : token;
  useEffect(() => {
    const params = new URLSearchParams({ index });
    if (expiry) params.set('expiry', expiry);
    if (centre != null) params.set('centre', String(centre));
    if (sendToken) params.set('token', sendToken);
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
      setReceived({ key, at: Date.now() });
    };
    // EventSource reconnects on its own; this only flips the badge meanwhile.
    es.onerror = () => setConnected(false);
    return () => es.close();
  }, [serverBase, index, expiry, centre, key, sendToken]);

  const ageMs = receivedAt == null ? null : Math.max(0, now - receivedAt);
  const stale = !connected || ageMs == null || ageMs > STREAM_STALE_AFTER_MS;
  return { frame, connected, stale, ageMs, now };
}
