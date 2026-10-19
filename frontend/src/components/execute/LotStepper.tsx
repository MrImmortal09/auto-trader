import { Minus, Plus } from 'lucide-react';
import { MAX_LOTS } from '../../lib/execute';

/** Lots per tap, 1–MAX_LOTS, with the resulting quantity. Keys + / − work too. */
export function LotStepper({ lots, lotSize, onChange }: {
  lots: number;
  lotSize: number | null;
  onChange: (lots: number) => void;
}) {
  const btn =
    'w-9 h-9 md:w-8 md:h-8 grid place-items-center text-on-surface hover:bg-surface-container active:bg-surface-container-high transition-colors disabled:text-outline disabled:hover:bg-transparent';
  return (
    <div className="flex items-center gap-2">
      <span className="text-[11px] font-bold uppercase tracking-wider text-on-surface-variant">Lots</span>
      <div className="flex items-center rounded-lg border border-outline-variant bg-surface-container-lowest overflow-hidden shadow-sm">
        <button type="button" className={btn} onClick={() => onChange(lots - 1)} disabled={lots <= 1} aria-label="One lot fewer" title="One lot fewer (−)">
          <Minus size={15} />
        </button>
        <span className="w-9 text-center font-mono-code font-bold text-base tabular-nums text-on-surface" aria-live="polite">
          {lots}
        </span>
        <button type="button" className={btn} onClick={() => onChange(lots + 1)} disabled={lots >= MAX_LOTS} aria-label="One lot more" title="One lot more (+)">
          <Plus size={15} />
        </button>
      </div>
      {lotSize != null && (
        <span className="text-xs text-on-surface-variant font-mono-code tabular-nums whitespace-nowrap">
          = <span className="font-bold text-on-surface">{lots * lotSize}</span> qty
        </span>
      )}
    </div>
  );
}
