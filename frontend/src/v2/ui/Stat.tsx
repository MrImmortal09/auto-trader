import type { ReactNode } from 'react';
import type { Tone } from '../lib/types';

// Figma "Stat": a label over a number.

const SIZE = { hero: 'num-hero', large: 'num-large', medium: 'num-medium' } as const;
const TONE: Record<Tone, string> = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' };

export function Stat({ label, value, size = 'large', tone = 'neutral', sub }: {
  label: ReactNode;
  value: ReactNode;
  size?: keyof typeof SIZE;
  tone?: Tone;
  /** A line under the number. */
  sub?: ReactNode;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <span className="type-label whitespace-nowrap text-fg-muted">{label}</span>
      <span className={`${SIZE[size]} whitespace-nowrap ${TONE[tone]}`}>{value}</span>
      {sub && <span className="type-caption text-fg-muted">{sub}</span>}
    </div>
  );
}
