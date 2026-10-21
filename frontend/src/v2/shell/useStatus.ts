import { useEffect, useRef } from 'react';
import { apiUrl } from '../../lib/api';
import { getToken } from '../../lib/auth';
import { getStatus } from '../lib/api';
import { fillToast } from '../lib/labels';
import { usePoll } from '../lib/usePoll';
import type { StatusAnswer } from '../lib/types';
import { useToast } from '../ui/toast';

/**
 * The status bar's data, every 3 s, and a toast for each new fill. The first
 * answer only sets where "new" starts, so opening the page never replays
 * the day's fills.
 */
export function useStatus(serverBase: string) {
  const toast = useToast();
  // The newest trade row already seen; `null` until the first answer.
  const seen = useRef<number | null>(null);
  const polled = usePoll<StatusAnswer>(() => getStatus(serverBase, seen.current), 3000, serverBase);

  const data = polled.data;
  useEffect(() => {
    if (!data) return;
    // A database cleared under us restarts the ids: start over from it.
    if (seen.current == null || data.last_fill_id < seen.current) {
      seen.current = data.last_fill_id;
      return;
    }
    for (const fill of data.fills) {
      if (fill.id > seen.current) toast(fillToast(fill));
    }
    seen.current = Math.max(seen.current, data.last_fill_id);
  }, [data, toast]);

  return polled;
}

/** A toast for each engine error on the live log. */
export function useEngineErrors(serverBase: string) {
  const toast = useToast();
  useEffect(() => {
    let es: EventSource;
    try {
      const token = getToken();
      es = new EventSource(apiUrl(serverBase, '/api/logs/stream') + (token ? `?token=${encodeURIComponent(token)}` : ''));
    } catch {
      return;
    }
    es.onmessage = (e: MessageEvent<string>) => {
      const text = String(e.data);
      // The log's own rule for an error line, as v1's terminal reads it.
      if (!text.includes('"level":"ERROR"') && !text.includes('"event":"ERROR"') && !text.includes('"event":"LIVE_ERROR"')) return;
      let message = 'The engine reported an error — see System.';
      let instrument = '';
      try {
        const p = JSON.parse(text) as { message?: string; error?: string; instrument?: string };
        message = p.message ?? p.error ?? message;
        instrument = p.instrument && p.instrument !== 'UPDATE' ? p.instrument : '';
      } catch {
        /* not JSON: keep the generic line */
      }
      toast({ tone: 'loss', title: instrument ? `Error · ${instrument}` : 'Engine error', body: message, key: `error-${message}` });
    };
    return () => es.close();
  }, [serverBase, toast]);
}
