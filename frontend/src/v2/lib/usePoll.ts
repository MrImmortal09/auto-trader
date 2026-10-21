import { useCallback, useEffect, useRef, useState } from 'react';

export interface Polled<T> {
  data: T | null;
  /** The last failure, cleared by the next success. */
  error: string | null;
  /** Fetch now, outside the schedule — after an action, say. Resolves once
   * the answer is in. */
  refresh: () => Promise<void>;
}

interface Answer<T> {
  key: string;
  data: T | null;
  error: string | null;
}

/**
 * `load` now and every `everyMs`, while `enabled` and the tab is visible —
 * a hidden tab stops polling and catches up the moment it's shown again.
 * A slow answer never overwrites a newer one. A new `key` (what's being
 * loaded changed) starts over, and the old key's data is never shown for it.
 */
export function usePoll<T>(load: () => Promise<T>, everyMs: number, key: string, enabled = true): Polled<T> {
  const [answer, setAnswer] = useState<Answer<T>>({ key, data: null, error: null });
  const loadRef = useRef(load);
  const seq = useRef(0);

  useEffect(() => {
    loadRef.current = load;
  });

  const run = useCallback(async (forKey: string) => {
    const mine = ++seq.current;
    try {
      const data = await loadRef.current();
      if (mine === seq.current) setAnswer({ key: forKey, data, error: null });
    } catch (e) {
      if (mine !== seq.current) return;
      const error = e instanceof Error ? e.message : 'Could not reach the server';
      setAnswer((prev) => ({ key: forKey, data: prev.key === forKey ? prev.data : null, error }));
    }
  }, []);

  useEffect(() => {
    if (!enabled) return;
    const counter = seq;
    const tick = () => {
      if (document.visibilityState === 'visible') void run(key);
    };
    void run(key);
    const timer = window.setInterval(tick, everyMs);
    document.addEventListener('visibilitychange', tick);
    return () => {
      // Answers still in flight are for the old key: drop them.
      counter.current++;
      window.clearInterval(timer);
      document.removeEventListener('visibilitychange', tick);
    };
  }, [key, everyMs, enabled, run]);

  const current = answer.key === key;
  return {
    data: current ? answer.data : null,
    error: current ? answer.error : null,
    refresh: () => run(key),
  };
}
