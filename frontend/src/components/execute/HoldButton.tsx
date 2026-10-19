import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';

/**
 * A button that fires only after being held down for `holdMs` — for actions
 * a stray tap must not trigger. A fill sweeps across while it's held;
 * letting go early cancels. Space/Enter held down works the same way.
 */
export function HoldButton({ onFire, holdMs = 600, disabled = false, title, className = '', children }: {
  onFire: () => void;
  holdMs?: number;
  disabled?: boolean;
  title?: string;
  className?: string;
  children: ReactNode;
}) {
  const [holding, setHolding] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => () => {
    if (timer.current != null) window.clearTimeout(timer.current);
  }, []);

  function cancel() {
    if (timer.current != null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    setHolding(false);
  }

  function start() {
    if (disabled) return;
    cancel();
    setHolding(true);
    timer.current = window.setTimeout(() => {
      timer.current = null;
      setHolding(false);
      onFire();
    }, holdMs);
  }

  return (
    <button
      type="button"
      title={title}
      disabled={disabled}
      onPointerDown={(e) => {
        if (e.button === 0) start();
      }}
      onPointerUp={cancel}
      onPointerLeave={cancel}
      onPointerCancel={cancel}
      onKeyDown={(e) => {
        if ((e.key === 'Enter' || e.key === ' ') && !e.repeat) {
          e.preventDefault();
          start();
        }
      }}
      onKeyUp={cancel}
      onBlur={cancel}
      onContextMenu={(e) => e.preventDefault()}
      className={`relative overflow-hidden select-none touch-none ${className}`}
    >
      {holding && (
        <span
          aria-hidden
          className="hold-fill absolute inset-0 bg-white/30"
          style={{ '--hold-ms': `${holdMs}ms` } as CSSProperties}
        />
      )}
      <span className="relative inline-flex items-center gap-1.5">{children}</span>
    </button>
  );
}
