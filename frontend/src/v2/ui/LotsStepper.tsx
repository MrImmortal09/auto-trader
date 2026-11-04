import { Minus, Plus } from 'lucide-react';

/** Most lots the stepper goes to — a fat-finger guard, like the Execute
 * screen's per-tap limit. */
export const MAX_STEPPER_LOTS = 10;

// Figma "Lots stepper": "Lots [− 2 +] 130 qty", the count in an inset well.
export function LotsStepper({ lots, lotSize, onChange, disabled = false, size = 'sm' }: {
  lots: number;
  lotSize: number;
  onChange: (lots: number) => void;
  disabled?: boolean;
  size?: 'sm' | 'lg';
}) {
  const lg = size === 'lg';
  const step = (by: number) => {
    const next = Math.min(MAX_STEPPER_LOTS, Math.max(1, lots + by));
    if (next !== lots) onChange(next);
  };
  const button = `grid place-items-center text-fg transition-colors duration-120 hover:bg-raised-strong disabled:opacity-40 disabled:hover:bg-transparent ${lg ? 'size-11 rounded-8' : 'size-7 rounded-6'}`;
  return (
    <div className="flex items-center gap-2">
      <span className="type-label text-fg-muted">Lots</span>
      <div className={`flex items-center border border-line bg-inset ${lg ? 'rounded-8' : 'rounded-6'}`}>
        <button type="button" aria-label="One lot fewer" className={button} disabled={disabled || lots <= 1} onClick={() => step(-1)}>
          <Minus size={lg ? 18 : 14} />
        </button>
        {/* Inter's digits are wider than the old face's: 32 / 44 px holds two. */}
        <span className={`text-center text-fg ${lg ? 'w-11 num-large' : 'w-8 num-medium'}`} aria-live="polite">
          {lots}
        </span>
        <button type="button" aria-label="One lot more" className={button} disabled={disabled || lots >= MAX_STEPPER_LOTS} onClick={() => step(1)}>
          <Plus size={lg ? 18 : 14} />
        </button>
      </div>
      <span className="type-caption whitespace-nowrap text-fg-subtle tnum">{lots * lotSize} qty</span>
    </div>
  );
}
