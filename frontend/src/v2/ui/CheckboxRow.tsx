import { Check } from 'lucide-react';

/** Figma "Checkbox row". */
export function CheckboxRow({ checked, disabled = false, title, note, onChange }: { checked: boolean; disabled?: boolean; title: string; note: string; onChange: (v: boolean) => void }) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className="flex w-full items-start gap-3 rounded-8 px-2 py-2 text-left hover:bg-raised disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent"
    >
      <span className={`mt-0.5 grid size-4 shrink-0 place-items-center rounded-4 border ${checked ? 'border-fg bg-fg text-on-accent' : 'border-line-strong'}`}>
        {checked && <Check size={12} strokeWidth={3} />}
      </span>
      <span className="min-w-0">
        <span className="block type-body text-fg">{title}</span>
        <span className="block type-caption text-fg-muted">{note}</span>
      </span>
    </button>
  );
}
