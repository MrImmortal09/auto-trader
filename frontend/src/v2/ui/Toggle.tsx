// Figma "Toggle": on is an accent track with the white knob right, off a
// raised track with the white knob left.

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
      className={`relative inline-flex h-6 w-10 shrink-0 items-center rounded-full border transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-40 ${
        checked ? 'border-accent bg-accent' : 'border-line bg-raised'
      }`}
    >
      <span
        aria-hidden
        className={`absolute top-1/2 size-4 -translate-y-1/2 rounded-full bg-panel shadow-sm transition-[left,background-color] duration-150 ${checked ? 'left-[19px]' : 'left-[3px]'}`}
      />
    </button>
  );
}
