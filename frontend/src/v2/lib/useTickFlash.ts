import { useEffect, useState } from 'react';

/** How long a figure's wrapper shows the tint after a tick. */
const FLASH_MS = 400;

function reducedMotion(): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

type Flash = { key: number; dir: '' | 'up' | 'down' };

/**
 * A figure that just rose or fell: `className` is `animate-tick-up` or
 * `animate-tick-down` for 400 ms after a change, and `key` changes with each
 * flash, so the consumer renders `<span key={key} className={className}>`
 * and the CSS animation restarts by remount. Changes during a flash are
 * coalesced: no re-trigger mid-flash, though the comparison baseline moves
 * to the latest value. Nothing flashes while `enabled` is false or the
 * person prefers reduced motion.
 */
export function useTickFlash(value: number | null | undefined, enabled = true): { key: number; className: string } {
  const now = typeof value === 'number' && Number.isFinite(value) ? value : null;
  // The baseline: the last finite value seen. Starts at the first one, so
  // mounting never flashes.
  const [seen, setSeen] = useState<number | null>(now);
  const [flash, setFlash] = useState<Flash>({ key: 0, dir: '' });

  if (now != null && now !== seen) {
    // Adjusting state while rendering: React re-runs this render at once,
    // before the stale one is painted.
    setSeen(now);
    // Mid-flash (`dir` still set) a further change moves the baseline only.
    if (seen != null && flash.dir === '' && enabled && !reducedMotion()) {
      setFlash({ key: flash.key + 1, dir: now > seen ? 'up' : 'down' });
    }
  }

  useEffect(() => {
    if (flash.dir === '') return;
    const t = window.setTimeout(() => setFlash((f) => ({ ...f, dir: '' })), FLASH_MS);
    return () => window.clearTimeout(t);
  }, [flash]);

  const dir = enabled ? flash.dir : '';
  return { key: flash.key, className: dir === 'up' ? 'animate-tick-up' : dir === 'down' ? 'animate-tick-down' : '' };
}
