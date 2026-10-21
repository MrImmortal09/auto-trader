import { useEffect, useState, useSyncExternalStore } from 'react';
import { getStoredServerBase } from '../../lib/api';

/** The backend v1 talks to — the same stored setting, so one change covers both. */
export function useServerBase(): string {
  const [base, setBase] = useState(() => getStoredServerBase());
  useEffect(() => {
    const onChange = (e: Event) => {
      const next = (e as CustomEvent<{ serverBase?: string }>).detail?.serverBase;
      if (typeof next === 'string') setBase(next);
    };
    window.addEventListener('at_server_base_change', onChange);
    return () => window.removeEventListener('at_server_base_change', onChange);
  }, []);
  return base;
}

const DESKTOP = '(min-width: 1024px)';

/** The desktop layout (1024 px and up), or the phone one. */
export function useIsDesktop(): boolean {
  return useSyncExternalStore(
    (onChange) => {
      const mq = window.matchMedia(DESKTOP);
      mq.addEventListener('change', onChange);
      return () => mq.removeEventListener('change', onChange);
    },
    () => window.matchMedia(DESKTOP).matches,
    () => true,
  );
}

const WIDE = '(min-width: 1280px)';

/** Wide enough for Desk's Telegram rail beside the positions. */
export function useIsWide(): boolean {
  return useSyncExternalStore(
    (onChange) => {
      const mq = window.matchMedia(WIDE);
      mq.addEventListener('change', onChange);
      return () => mq.removeEventListener('change', onChange);
    },
    () => window.matchMedia(WIDE).matches,
    () => true,
  );
}

/** The IST wall clock, `HH:MM:SS`, ticking each second. */
export function useIstClock(): string {
  const fmt = () =>
    new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }).format(new Date());
  const [now, setNow] = useState(fmt);
  useEffect(() => {
    const t = window.setInterval(() => setNow(fmt()), 1000);
    return () => window.clearInterval(t);
  }, []);
  return now;
}
