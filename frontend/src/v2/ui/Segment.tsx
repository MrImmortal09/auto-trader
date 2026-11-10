// Figma "Segment": a row of choices on an inset track; the selected one is a
// white pill with accent text (or the marigold pill, for the LIVE option).

export function Segment<T extends string>({
  options,
  value,
  onChange,
  size = 'sm',
  stretch = false,
  disabled = false,
  label,
}: {
  /** `kind: 'live'` marks the option that means real money: selected, it is
   * the marigold pill with ink text (the Settings Mode segment). */
  options: { value: T; label: string; kind?: 'live' }[];
  value: T;
  onChange: (value: T) => void;
  /** `xs` fits a slim bar: the Trade screen's index ticker. */
  size?: 'xs' | 'sm' | 'lg';
  /** Share the width equally (phone). */
  stretch?: boolean;
  disabled?: boolean;
  label: string;
}) {
  const item = size === 'lg' ? 'h-10 px-4 rounded-8 type-body-strong' : size === 'xs' ? 'h-6 px-2.5 rounded-4 type-label' : 'h-7 px-3 rounded-6 type-label';
  const track = size === 'lg' ? 'gap-1 p-1 rounded-12' : size === 'xs' ? 'gap-0.5 p-0.5 rounded-6' : 'gap-1 p-1 rounded-8';
  return (
    <div role="tablist" aria-label={label} className={`inline-flex border border-line bg-inset ${track} ${stretch ? 'flex w-full' : ''}`}>
      {options.map((o) => {
        const selected = o.value === value;
        const look = !selected
          ? 'text-fg-muted hover:text-fg disabled:hover:text-fg-muted'
          : o.kind === 'live'
            ? 'border border-live-ink bg-live text-fg shadow-sm'
            : 'bg-panel text-accent shadow-sm';
        return (
          <button
            key={o.value}
            type="button"
            role="tab"
            aria-selected={selected}
            disabled={disabled}
            onClick={() => onChange(o.value)}
            className={`${item} ${stretch ? 'flex-1' : ''} whitespace-nowrap transition-colors duration-120 disabled:cursor-not-allowed disabled:opacity-50 ${look}`}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
