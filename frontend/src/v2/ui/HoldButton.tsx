import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { Button, type ButtonKind, type ButtonSize } from './Button';

/**
 * A button that fires only after being held for `holdMs` — for actions a
 * stray tap must never trigger. A fill sweeps across while it's held, and
 * letting go early cancels. Holding Space or Enter works the same way.
 */
export function HoldButton({ onFire, holdMs = 1000, kind = 'loss', size = 'sm', disabled = false, className = '', children }: {
  onFire: () => void;
  holdMs?: number;
  kind?: ButtonKind;
  size?: ButtonSize;
  disabled?: boolean;
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
    <Button
      kind={kind}
      size={size}
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
      className={`relative touch-none overflow-hidden ${className}`}
    >
      {holding && (
        <span
          aria-hidden
          className="hold-sweep absolute inset-0 bg-fg/30"
          style={{ '--hold-ms': `${holdMs}ms` } as CSSProperties}
        />
      )}
      <span className="relative">{children}</span>
    </Button>
  );
}
