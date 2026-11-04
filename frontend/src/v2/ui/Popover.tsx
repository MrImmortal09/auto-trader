import { useEffect, useLayoutEffect, useRef, type ReactNode } from 'react';

/** A popover anchored under its button: white, a hairline and the floating
 * shadow. A click outside or Escape closes it. It is never taller than the
 * room under it (a long menu scrolls inside). */
export function Popover({ open, onClose, align = 'left', children }: { open: boolean; onClose: () => void; align?: 'left' | 'right'; children: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (!open || !el) return;
    // The room: down to the nearest ancestor that clips or scrolls, else the window.
    let bottom = window.innerHeight;
    for (let a = el.parentElement; a; a = a.parentElement) {
      const o = getComputedStyle(a).overflowY;
      if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') {
        bottom = a.getBoundingClientRect().bottom;
        break;
      }
    }
    el.style.maxHeight = `${Math.max(160, Math.floor(bottom - el.getBoundingClientRect().top - 12))}px`;
  }, [open]);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (box.current && !box.current.parentElement?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    document.addEventListener('pointerdown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div ref={box} className={`absolute top-full z-30 mt-1 min-w-[260px] animate-fade-in overflow-y-auto rounded-12 border border-line bg-panel p-2 shadow-md ${align === 'right' ? 'right-0' : 'left-0'}`}>
      {children}
    </div>
  );
}
