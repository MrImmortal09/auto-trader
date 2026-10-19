import { useEffect, useEffectEvent } from 'react';
import { CircleAlert, CircleCheck, LoaderCircle, X } from 'lucide-react';
import type { Toast } from '../../lib/orderTracker';

const LIFETIME: Record<Toast['tone'], number | null> = { pending: null, ok: 4_500, err: 9_000 };

function ToastCard({ toast, onDismiss }: { toast: Toast; onDismiss: (key: string) => void }) {
  const life = LIFETIME[toast.tone];
  // The screen re-renders every 250 ms; only a new outcome restarts the clock.
  const dismiss = useEffectEvent(() => onDismiss(toast.key));
  useEffect(() => {
    if (life == null) return;
    const t = window.setTimeout(dismiss, life);
    return () => window.clearTimeout(t);
  }, [life, toast.key]);

  const tone = {
    pending: 'border-outline-variant',
    ok: 'border-secondary/40',
    err: 'border-error/40',
  }[toast.tone];
  return (
    <div
      role={toast.tone === 'err' ? 'alert' : 'status'}
      className={`pointer-events-auto w-full sm:w-80 rounded-lg border bg-surface-container-lowest shadow-xl px-3 py-2.5 flex items-start gap-2.5 ${tone}`}
    >
      <span className="mt-0.5 shrink-0">
        {toast.tone === 'pending' && <LoaderCircle size={16} className="animate-spin text-primary" />}
        {toast.tone === 'ok' && <CircleCheck size={16} className="text-secondary" />}
        {toast.tone === 'err' && <CircleAlert size={16} className="text-error" />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="text-[13px] font-bold text-on-surface leading-snug">{toast.title}</div>
        {toast.detail && <div className="mt-0.5 text-[11px] text-on-surface-variant leading-snug wrap-break-word">{toast.detail}</div>}
      </div>
      <button type="button" onClick={() => onDismiss(toast.key)} aria-label="Dismiss" className="shrink-0 text-outline hover:text-on-surface">
        <X size={14} />
      </button>
    </div>
  );
}

/** Order outcomes — bottom-right on desktop, under the top bar on phones. */
export function OrderToasts({ toasts, onDismiss }: { toasts: Toast[]; onDismiss: (key: string) => void }) {
  if (toasts.length === 0) return null;
  return (
    <div className="pointer-events-none fixed z-70 inset-x-3 top-16 sm:inset-x-auto sm:top-auto sm:right-5 sm:bottom-6 flex flex-col gap-2 items-stretch sm:items-end">
      {toasts.slice(-4).map((t) => (
        <ToastCard key={t.key} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}
