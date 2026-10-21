import { useEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

/**
 * A dialog over a scrim: centred on desktop, a bottom sheet on phone.
 * Escape or a tap on the scrim closes it, unless `locked` (an action is in
 * flight and must finish first).
 */
export function Modal({ open, onClose, label, locked = false, width = 'lg:max-w-[520px]', children }: {
  open: boolean;
  onClose: () => void;
  /** What a screen reader announces. */
  label: string;
  locked?: boolean;
  width?: string;
  children: ReactNode;
}) {
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !locked) onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open, locked, onClose]);

  useEffect(() => {
    if (!open) return;
    // Focus what the dialog marks (`data-autofocus`), else its first field
    // or button, so the keyboard lands inside.
    const el = panel.current;
    const first =
      el?.querySelector<HTMLElement>('[data-autofocus]') ??
      el?.querySelector<HTMLElement>('input, button:not([disabled]), [tabindex]:not([tabindex="-1"])');
    first?.focus();
  }, [open]);

  if (!open) return null;
  return createPortal(
    <div className="fixed inset-0 z-50 flex items-end justify-center animate-fade-in lg:items-center lg:p-6">
      <div aria-hidden className="absolute inset-0 bg-scrim" onClick={() => !locked && onClose()} />
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        className={`relative flex max-h-[92dvh] w-full flex-col overflow-hidden rounded-t-16 border-line bg-panel pb-safe animate-sheet-in lg:rounded-16 lg:border lg:pb-0 lg:animate-rise-in ${width}`}
      >
        <div aria-hidden className="mx-auto mt-2 h-1 w-9 shrink-0 rounded-full bg-line-strong lg:hidden" />
        {children}
      </div>
    </div>,
    document.body,
  );
}
