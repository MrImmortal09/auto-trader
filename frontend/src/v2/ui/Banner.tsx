import type { ReactNode } from 'react';
import { CircleAlert, Clock, Info } from 'lucide-react';
import { Button } from './Button';

// Figma "Banner", under the status bar. Stopped and Disconnected are
// brick-tinted; Stale and Expiry are neutral.
export type BannerKind = 'stopped' | 'disconnected' | 'stale' | 'expiry';

export function Banner({ kind, title, body, action }: {
  kind: BannerKind;
  title: ReactNode;
  body: ReactNode;
  action?: { label: string; onClick: () => void; disabled?: boolean };
}) {
  const alarming = kind === 'stopped' || kind === 'disconnected';
  const Icon = kind === 'stale' ? Clock : kind === 'expiry' ? Info : CircleAlert;
  return (
    <div role={alarming ? 'alert' : 'status'} className={`flex items-center gap-3 border-b border-line px-4 py-3 ${alarming ? 'bg-loss-subtle' : 'bg-panel'}`}>
      <Icon size={18} className={`shrink-0 ${alarming ? 'text-loss' : 'text-fg-muted'}`} />
      <div className="min-w-0 flex-1">
        <div className="type-body-strong text-fg">{title}</div>
        <div className="type-caption text-fg-muted">{body}</div>
      </div>
      {action && (
        <Button size="md" onClick={action.onClick} disabled={action.disabled}>
          {action.label}
        </Button>
      )}
    </div>
  );
}
