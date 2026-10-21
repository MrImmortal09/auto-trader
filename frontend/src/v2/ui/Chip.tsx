import type { ReactNode } from 'react';
import { Send, Zap } from 'lucide-react';
import type { Source, Tone } from '../lib/types';

// Figma "Chip".
export type ChipKind = 'outline' | 'live' | 'neutral' | 'profit' | 'loss';

const KIND: Record<ChipKind, string> = {
  outline: 'border border-line text-fg-muted',
  live: 'bg-live text-on-accent',
  neutral: 'bg-raised text-fg-muted',
  profit: 'bg-profit-subtle text-profit',
  loss: 'bg-loss-subtle text-loss',
};

export function Chip({ kind = 'outline', icon, className = '', children }: {
  kind?: ChipKind;
  icon?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <span className={`inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-4 px-1.5 type-caption ${KIND[kind]} ${className}`}>
      {icon}
      {children}
    </span>
  );
}

/** A chip in a tone's colours: profit, loss, or neutral grey. */
export function ToneChip({ tone, children }: { tone: Tone; children: ReactNode }) {
  return <Chip kind={tone === 'neutral' ? 'neutral' : tone}>{children}</Chip>;
}

/** Where a call came from: its Telegram channel, or "Scalp". */
export function SourceChip({ source }: { source: Source }) {
  return (
    <Chip icon={source.kind === 'scalp' ? <Zap size={11} /> : <Send size={11} />}>
      <span className="max-w-[140px] truncate">{source.title}</span>
    </Chip>
  );
}
