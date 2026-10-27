import { useEffect, useState } from 'react';
import type { ExecuteFrame } from '../types';
import { apiUrl } from '../lib/api';
import { getToken, onAuthChange } from '../lib/auth';

/** No frame for this long (several missed 250 ms beats) and the stream is shown as stale. */
export const STREAM_STALE_AFTER_MS = 2_000;

/**
 * Live Execute-screen frames for `index` (SSE, one every 250 ms): the
 * ladder of `expiry` (the nearest when null) around `centre` (ATM when
 * null). Switching any of them reopens the stream; the last frame is kept
 * meanwhile so the page never blanks. `now` ticks so the caller can tell how
 * old the frame is. The token goes in the URL (an `EventSource` can't set
 * headers): the stored sign-in's unless `token` says otherwise.
 *
 * The server answers with the caller's own view (the engine's mode and
 * positions for the owner, none for a visitor), so the stream is also
 * reopened whenever the sign-in changes: signed in, signed out, or signed
 * in again after it expired, or a different `token` passed. Until the
 * new stream's first frame lands the old one counts as stale, so nothing is
 * bought off a visitor's frame.
 */
export function useExecuteStream(serverBase: string, index: string, expiry: string | null = null, centre: number | null = null, token?: string | null) {
  const [frame, setFrame] = useState<ExecuteFrame | null>(null);
  const [connected, setConnected] = useState(false);
  // Bumped on every sign-in change, so the effect below reopens the stream.
  const [authEpoch, setAuthEpoch] = useState(0);
  const sendToken = token === undefined ? getToken() : token;
  // Local receipt time, so browser/VM clock skew can't make a live stream look
  // stale — kept per index, expiry and sign-in, so after a switch the stream
  // counts as stale (and nothing is bought off the previous one's frame)
  // until the new stream's first frame arrives.
  const key = `${serverBase}|${index}|${expiry ?? ''}|${authEpoch}|${sendToken ?? ''}`;
  const [received, setReceived] = useState<{ key: string; at: number } | null>(null);
  const receivedAt = received?.key === key ? received.at : null;
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => onAuthChange(() => setAuthEpoch((n) => n + 1)), []);

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
