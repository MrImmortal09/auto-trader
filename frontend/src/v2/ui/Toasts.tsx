import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { CircleAlert, CircleCheck, Info, X } from 'lucide-react';
import { ToastContext, type ToastSpec } from './toast';

// Figma "Toast": fills, stop and target hits, and errors. A white card with
// a hairline and the lg shadow; the tone is the icon at the left. Nothing
// clips: the Dismiss button sits inside the card with room for its focus ring.

interface Shown extends ToastSpec {
  id: number;
}

const MAX_SHOWN = 4;
const REPEAT_WINDOW_MS = 60_000;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [shown, setShown] = useState<Shown[]>([]);
  const nextId = useRef(1);
  const recent = useRef(new Map<string, number>());

  const dismiss = useCallback((id: number) => setShown((s) => s.filter((t) => t.id !== id)), []);

  const push = useCallback((t: ToastSpec) => {
    if (t.key) {
      const now = Date.now();
      const last = recent.current.get(t.key);
      if (last != null && now - last < REPEAT_WINDOW_MS) return;
      recent.current.set(t.key, now);
    }
    const id = nextId.current++;
    setShown((s) => [...s, { ...t, id }].slice(-MAX_SHOWN));
  }, []);

  return (
    <ToastContext.Provider value={push}>
      {children}
      {createPortal(
        <div
          aria-live="polite"
          className="pointer-events-none fixed inset-x-4 top-[calc(env(safe-area-inset-top)+52px)] z-[60] flex flex-col items-center gap-2 lg:inset-x-auto lg:top-auto lg:right-4 lg:bottom-4 lg:items-end"
        >
          {shown.map((t) => (
            <Toast key={t.id} toast={t} onDismiss={dismiss} />
          ))}
        </div>,
        document.body,
      )}
    </ToastContext.Provider>
  );
}

const ICON = { profit: CircleCheck, loss: CircleAlert, neutral: Info } as const;
const ICON_TONE = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;

function Toast({ toast, onDismiss }: { toast: Shown; onDismiss: (id: number) => void }) {
  // Errors stay up longer: someone may need to act on them.
  useEffect(() => {
    const t = window.setTimeout(() => onDismiss(toast.id), toast.tone === 'loss' ? 10_000 : 6_000);
    return () => window.clearTimeout(t);
  }, [toast.id, toast.tone, onDismiss]);

  const Icon = ICON[toast.tone];
  return (
    <div role="status" className="pointer-events-auto relative flex w-full max-w-[360px] animate-rise-in items-stretch rounded-8 border border-line bg-panel shadow-lg lg:w-[320px]">
      <span aria-hidden className={`flex shrink-0 items-center px-3 ${ICON_TONE[toast.tone]}`}>
        <Icon size={16} />
      </span>
      <div className="min-w-0 flex-1 py-2.5 pr-2">
        <div className="type-body-strong text-fg">{toast.title}</div>
        {toast.body && <div className="mt-0.5 type-caption text-fg-subtle tnum">{toast.body}</div>}
      </div>
      <button type="button" aria-label="Dismiss" onClick={() => onDismiss(toast.id)} className="mr-1 grid size-8 shrink-0 place-items-center self-center rounded-6 text-fg-muted transition-colors duration-120 hover:text-fg">
        <X size={14} />
      </button>
    </div>
  );
}
