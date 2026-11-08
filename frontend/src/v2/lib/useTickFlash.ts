import { useEffect, useState } from 'react';

/** How long a figure's wrapper shows the tint after a tick. */
const FLASH_MS = 400;

function reducedMotion(): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

type Flash = { key: number; dir: '' | 'up' | 'down' };

/** The baseline a change is measured from. */
type Seen = { value: number | null; identity: unknown; on: boolean };

/**
 * A figure that just rose or fell: `className` is `animate-tick-up` or
 * `animate-tick-down` for 400 ms after a change, and `key` changes with each
 * flash, so the consumer renders `<span key={key} className={className}>`
 * and the CSS animation restarts by remount. Changes during a flash are
 * coalesced: no re-trigger mid-flash, though the comparison baseline moves
 * to the latest value. Nothing flashes while `enabled` is false or the
 * person prefers reduced motion.
 *
 * `identity` says what the figure is (the contract, the index, the set of
 * open positions). When it changes, the next value is a different figure, not
 * a tick: the baseline starts over from it and nothing flashes. Switching a
 * chart from NIFTY to BANKNIFTY must not read as a price move.
 *
 * While `enabled` is false nothing is tracked, so a row that can't flash (a
 * strike far from ATM) doesn't re-render on every tick; turning it on starts
 * over from the value then.
 */
export function useTickFlash(value: number | null | undefined, enabled = true, identity: unknown = null): { key: number; className: string } {
  const now = typeof value === 'number' && Number.isFinite(value) ? value : null;
  // Starts at the first value, so mounting never flashes.
  const [seen, setSeen] = useState<Seen>({ value: now, identity, on: enabled });
  const [flash, setFlash] = useState<Flash>({ key: 0, dir: '' });

  // Adjusting state while rendering: React re-runs this render at once,
  // before the stale one is painted.
  if (identity !== seen.identity || enabled !== seen.on) {
    setSeen({ value: now, identity, on: enabled });
  } else if (enabled && now != null && now !== seen.value) {
    setSeen({ value: now, identity, on: true });
    // Mid-flash (`dir` still set) a further change moves the baseline only.
    if (seen.value != null && flash.dir === '' && !reducedMotion()) {
      setFlash({ key: flash.key + 1, dir: now > seen.value ? 'up' : 'down' });
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
