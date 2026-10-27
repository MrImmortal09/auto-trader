import { useEffect, useState } from 'react';
import { apiFetchWith, apiUrl } from '../../lib/api';
import { nowIstTime, todayIst } from '../lib/format';
import { logLine, type LogLine } from './logLines';

/** Most lines held, newest first. */
const MAX_LINES = 2000;

interface State {
  /** The server and token the lines were read with. */
  key: string;
  lines: LogLine[];
  /** `null` until the stream first opens or fails. */
  connected: boolean | null;
}

/**
 * The engine log, newest first: the last 500 saved lines, then each new
 * one as the server streams it (`/api/logs/*`, unchanged). A new `token`
 * (v2's `streamToken()`) reads it all again with that one.
 */
export function useEngineLog(serverBase: string, token: string | null): { lines: LogLine[]; connected: boolean | null } {
  const key = `${serverBase}|${token ?? ''}`;
  const [state, setState] = useState<State>({ key, lines: [], connected: null });

  useEffect(() => {
    let cancelled = false;
    let seq = 0;
    const add = (fresh: LogLine[], older: boolean) =>
      setState((prev) => {
        const lines = prev.key === key ? prev.lines : [];
        // Streamed lines are newer than the saved ones, whichever came first.
        const next = older ? [...lines, ...fresh] : [...fresh, ...lines];
        return { key, lines: next.slice(0, MAX_LINES), connected: prev.key === key ? prev.connected : null };
      });

    apiFetchWith(token, serverBase, '/api/logs/history')
      .then((r) => (r.ok ? r.json() : []))
      .then((rows: unknown) => {
        if (cancelled || !Array.isArray(rows)) return;
        const saved = (rows as { message: unknown; timestamp: string }[])
          .map((r, i) => logLine(JSON.stringify(r.message), r.timestamp, `h${i}`))
          .filter((l): l is LogLine => l != null)
          .reverse();
        add(saved, true);
      })
      .catch(() => {});

    const es = new EventSource(apiUrl(serverBase, '/api/logs/stream') + (token ? `?token=${encodeURIComponent(token)}` : ''));
    es.onopen = () => setState((prev) => (prev.key === key ? { ...prev, connected: true } : { key, lines: [], connected: true }));
    es.onmessage = (e: MessageEvent<string>) => {
      const line = logLine(e.data, `${todayIst()} ${nowIstTime()}`, `s${++seq}`);
      if (line) add([line], false);
    };
    // EventSource reconnects by itself; meanwhile the log shows it.
    es.onerror = () => setState((prev) => ({ ...prev, connected: false }));
    return () => {
      cancelled = true;
      es.close();
    };
  }, [key, serverBase, token]);

  const current = state.key === key;
  return { lines: current ? state.lines : [], connected: current ? state.connected : null };
}
