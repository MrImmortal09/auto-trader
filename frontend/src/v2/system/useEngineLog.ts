import { useEffect, useState } from 'react';
import { apiFetchWith, apiUrl } from '../../lib/api';
import { streamToken } from '../lib/api';
import { nowIstTime, todayIst } from '../lib/format';
import { logLine, type LogLine } from './logLines';

/** Most lines held, newest first. */
const MAX_LINES = 2000;

interface State {
  base: string;
  lines: LogLine[];
  /** `null` until the stream first opens or fails. */
  connected: boolean | null;
}

/**
 * The engine log, newest first: the last 500 saved lines, then each new
 * one as the server streams it (`/api/logs/*`, unchanged).
 */
export function useEngineLog(serverBase: string): { lines: LogLine[]; connected: boolean | null } {
  const [state, setState] = useState<State>({ base: serverBase, lines: [], connected: null });

  useEffect(() => {
    let cancelled = false;
    let seq = 0;
    const add = (fresh: LogLine[], older: boolean) =>
      setState((prev) => {
        const lines = prev.base === serverBase ? prev.lines : [];
        // Streamed lines are newer than the saved ones, whichever came first.
        const next = older ? [...lines, ...fresh] : [...fresh, ...lines];
        return { base: serverBase, lines: next.slice(0, MAX_LINES), connected: prev.base === serverBase ? prev.connected : null };
      });

    apiFetchWith(streamToken(), serverBase, '/api/logs/history')
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

    const token = streamToken();
    const es = new EventSource(apiUrl(serverBase, '/api/logs/stream') + (token ? `?token=${encodeURIComponent(token)}` : ''));
    es.onopen = () => setState((prev) => ({ ...prev, base: serverBase, connected: true }));
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
  }, [serverBase]);

  const current = state.base === serverBase;
  return { lines: current ? state.lines : [], connected: current ? state.connected : null };
}
