import { useEffect, useRef, type ReactNode } from 'react';

/** A popover anchored under its button; a click outside or Escape closes it. */
export function Popover({ open, onClose, align = 'left', children }: { open: boolean; onClose: () => void; align?: 'left' | 'right'; children: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
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
    <div ref={box} className={`absolute top-full z-30 mt-1 min-w-[260px] animate-fade-in rounded-12 border border-line bg-panel p-2 shadow-lg ${align === 'right' ? 'right-0' : 'left-0'}`}>
      {children}
    </div>
  );
}
