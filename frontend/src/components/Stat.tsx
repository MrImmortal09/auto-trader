import type { ReactNode } from 'react';

// Small reusable stat card
export function Stat({ icon, label, children }: {
  icon: ReactNode;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center gap-3 bg-surface-container-lowest border border-outline-variant rounded-lg px-4 py-3.5 hover:shadow-sm transition-shadow">
      <div className="p-2 bg-surface-container rounded-lg text-primary">{icon}</div>
      <div>
        <div className="text-xs text-on-surface-variant font-medium">{label}</div>
        <div className="text-lg font-bold text-on-surface tabular-nums">{children}</div>
      </div>
    </div>
  );
}
