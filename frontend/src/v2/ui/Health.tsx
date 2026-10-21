import type { Level } from '../lib/types';

// Figma "Health": a dot — jade up, brick down, hollow when it doesn't
// apply right now (not configured, or the market is closed).

export function HealthDot({ level, className = '' }: { level: Level; className?: string }) {
  const look =
    level === 'ok' ? 'bg-profit' : level === 'down' ? 'bg-loss' : 'border-[1.5px] border-fg-muted bg-transparent';
  return <span aria-hidden className={`inline-block size-2 shrink-0 rounded-full ${look} ${className}`} />;
}
