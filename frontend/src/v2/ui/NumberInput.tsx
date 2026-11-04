import { useState, type KeyboardEvent } from 'react';

// Figma "Input": an inset well with the value and its unit (%, ×, ₹). Edits
// are kept here until Enter or leaving the field; only a number actually
// typed, valid and different, is committed, and the well flashes accent for
// a moment to say it saved. Escape puts the saved value back.

const GROUPED = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 2 });

export function NumberInput({ value, onCommit, unit, min, max, decimals = 0, grouped = false, size = 'md', label, disabled = false, width = 'w-[120px]' }: {
  value: number;
  onCommit: (n: number) => void;
  unit?: string;
  min: number;
  max: number;
  /** Digits shown after the point: 1 shows 1.0. */
  decimals?: number;
  /** Indian digit grouping (10,000). */
  grouped?: boolean;
  size?: 'md' | 'lg';
  label: string;
  disabled?: boolean;
  width?: string;
}) {
  // `null` while not being edited: the saved value shows.
  const [draft, setDraft] = useState<string | null>(null);
  // Typed into since focus. Focusing and leaving alone never saves.
  const [typed, setTyped] = useState(false);
  // Commits so far: keying the well on it restarts the "saved" flash.
  const [saves, setSaves] = useState(0);
  const shown = grouped ? GROUPED.format(value) : value.toFixed(decimals);
  const text = draft ?? shown;
  const parsed = draft == null ? value : Number(draft.replace(/,/g, '').trim());
  const valid = draft == null || (draft.trim() !== '' && Number.isFinite(parsed) && parsed >= min && parsed <= max);

  function commit() {
    if (draft != null && typed && valid && parsed !== value) {
      onCommit(parsed);
      setSaves((n) => n + 1);
    }
    setDraft(null);
    setTyped(false);
  }

  function onKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === 'Enter') e.currentTarget.blur();
    if (e.key === 'Escape') {
      setDraft(null);
      setTyped(false);
      e.currentTarget.blur();
    }
  }

  return (
    <div className="flex flex-col items-end gap-1">
      <label
        key={saves}
        className={`flex items-center gap-2 border bg-inset px-3 transition-colors duration-120 focus-within:border-accent ${
          size === 'lg' ? 'h-11 rounded-8' : 'h-9 rounded-6'
        } ${valid ? 'border-line' : 'border-loss'} ${disabled ? 'opacity-50' : ''} ${saves > 0 ? 'animate-saved' : ''} ${width}`}
      >
        <input
          type="text"
          inputMode="decimal"
          aria-label={label}
          aria-invalid={!valid}
          disabled={disabled}
          value={text}
          // The exact saved value, never the rounded one shown: 0.55 is 0.55.
          onFocus={() => setDraft(String(value))}
          onChange={(e) => {
            setDraft(e.target.value);
            setTyped(true);
          }}
          onBlur={commit}
          onKeyDown={onKey}
          className="min-w-0 flex-1 bg-transparent num-medium text-fg outline-none placeholder:text-fg-subtle"
        />
        {unit && <span className="shrink-0 type-caption text-fg-subtle">{unit}</span>}
      </label>
      {!valid && (
        <span className="type-caption text-loss">
          Between {min} and {max}
        </span>
      )}
    </div>
  );
}
