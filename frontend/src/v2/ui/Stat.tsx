import type { ReactNode } from 'react';
import type { Tone } from '../lib/types';

// Figma "Stat": an eyebrow label over a number. One hero per screen.

const SIZE = { hero: 'num-hero', large: 'num-large', medium: 'num-medium' } as const;
const TONE: Record<Tone, string> = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' };

export function Stat({ label, value, size = 'large', tone = 'neutral', sub, flash }: {
  label: ReactNode;
  value: ReactNode;
  size?: keyof typeof SIZE;
  tone?: Tone;
  /** A line under the number. */
  sub?: ReactNode;
  /** From `useTickFlash`: the figure's wrapper flashes its tint when the value ticks. */
  flash?: { key: number; className: string };
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <span className="type-eyebrow whitespace-nowrap text-fg-muted">{label}</span>
      <span key={flash?.key} className={`${SIZE[size]} -mx-1 self-start whitespace-nowrap rounded-4 px-1 ${TONE[tone]} ${flash?.className ?? ''}`}>
        {value}
      </span>
      {sub && <span className="type-caption text-fg-subtle">{sub}</span>}
    </div>
  );
}
