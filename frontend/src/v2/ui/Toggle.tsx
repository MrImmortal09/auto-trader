// Figma "Toggle": on is a light track with the knob right, off a raised
// track with a grey knob left.

export function Toggle({ checked, onChange, disabled = false, label }: {
  checked: boolean;
  onChange: (on: boolean) => void;
  disabled?: boolean;
  /** What a screen reader announces. */
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-6 w-10 shrink-0 items-center rounded-full border transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
        checked ? 'border-fg bg-fg' : 'border-line bg-raised'
      }`}
    >
      <span
        aria-hidden
        className={`absolute top-1/2 size-4 -translate-y-1/2 rounded-full transition-[left,background-color] ${checked ? 'left-[19px] bg-canvas' : 'left-[3px] bg-fg-muted'}`}
      />
    </button>
  );
}
