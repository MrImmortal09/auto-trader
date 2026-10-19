import { useState } from 'react';
import { fmtLtp } from '../../lib/execute';

/**
 * A live price that tints green/red for a moment on each up/down tick and
 * keeps a ▲/▼ for the last move. The tint is a CSS animation on a re-keyed
 * element, so there are no timers to manage.
 */
export function PriceCell({ value, className = '', arrow = 'after' }: {
  value: number | null;
  className?: string;
  /** Which side of the number the ▲/▼ sits on (mirrored for the puts side). */
  arrow?: 'before' | 'after' | 'none';
}) {
  const [prev, setPrev] = useState(value);
  const [dir, setDir] = useState<'up' | 'down' | null>(null);
  const [ticks, setTicks] = useState(0);
  if (value !== prev) {
    setPrev(value);
    if (value != null && prev != null) {
      setDir(value > prev ? 'up' : 'down');
      setTicks((n) => n + 1);
    }
  }

  const glyph = arrow !== 'none' && dir && value != null && (
    <span className={`text-[9px] leading-none ${dir === 'up' ? 'text-secondary' : 'text-error'}`} aria-hidden>
      {dir === 'up' ? '▲' : '▼'}
    </span>
  );
  return (
    <span
      key={ticks}
      className={`inline-flex items-center gap-1 rounded px-1 font-mono-code tabular-nums ${
        ticks > 0 ? (dir === 'up' ? 'tick-up' : 'tick-down') : ''
      } ${value == null ? 'text-outline' : ''} ${className}`}
    >
      {arrow === 'before' && glyph}
      {fmtLtp(value)}
      {arrow === 'after' && glyph}
    </span>
  );
}
