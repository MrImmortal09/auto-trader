import { useEffect, useRef, type ReactNode } from 'react';

/**
 * A popover anchored under its button; a click outside or Escape closes it.
 * With `backdrop` (phone, where a menu can lie over the ladder), the tap that
 * closes it lands on a cover and goes no further — it can never also press a
 * Buy underneath. With `scroll`, a long menu scrolls rather than run off the
 * screen.
 */
export function Popover({ open, onClose, align = 'left', backdrop = false, scroll = false, children }: {
  open: boolean;
  onClose: () => void;
  align?: 'left' | 'right';
  backdrop?: boolean;
  /** A long menu with nothing nested in it: scroll within the screen. */
  scroll?: boolean;
  children: ReactNode;
}) {
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
    <>
      {/* Inside the anchor, so the outside-press check above leaves it to this click. */}
      {backdrop && <div aria-hidden className="fixed inset-0 z-20" onClick={onClose} />}
      <div ref={box} className={`absolute top-full z-30 mt-1 min-w-[260px] animate-fade-in rounded-12 border border-line bg-panel p-2 shadow-lg ${scroll ? 'max-h-[70dvh] overflow-y-auto' : ''} ${align === 'right' ? 'right-0' : 'left-0'}`}>
        {children}
      </div>
    </>
  );
}
