import type { ReactNode } from 'react';
import { CircleAlert, Clock, Info } from 'lucide-react';
import { Button } from './Button';

// Figma "Banner", under the status bar. Stopped and Disconnected are
// brick-tinted; Stale is raised and Expiry sits on the panel.
export type BannerKind = 'stopped' | 'disconnected' | 'stale' | 'expiry';

const LOOK: Record<BannerKind, string> = {
  stopped: 'bg-loss-subtle',
  disconnected: 'bg-loss-subtle',
  stale: 'bg-raised',
  expiry: 'bg-panel',
};

export function Banner({ kind, title, body, action }: {
  kind: BannerKind;
  title: ReactNode;
  body: ReactNode;
  action?: { label: string; onClick: () => void; disabled?: boolean };
}) {
  const alarming = kind === 'stopped' || kind === 'disconnected';
  const Icon = kind === 'stale' ? Clock : kind === 'expiry' ? Info : CircleAlert;
  return (
    <div role={alarming ? 'alert' : 'status'} className={`flex items-center gap-3 border-b border-line px-4 py-3 ${LOOK[kind]}`}>
      <Icon size={18} className={`shrink-0 ${alarming ? 'text-loss' : 'text-fg'}`} />
      <div className="min-w-0 flex-1">
        <div className="type-body-strong text-fg">{title}</div>
        <div className="type-caption text-fg-muted">{body}</div>
      </div>
      {action && (
        <Button size="md" kind={alarming ? 'neutral' : kind === 'expiry' ? 'quiet' : 'ghost'} onClick={action.onClick} disabled={action.disabled}>
          {action.label}
        </Button>
      )}
    </div>
  );
}
