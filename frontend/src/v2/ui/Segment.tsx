// Figma "Segment": a row of choices, the selected one raised.

export function Segment<T extends string>({
  options,
  value,
  onChange,
  size = 'sm',
  stretch = false,
  label,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  size?: 'sm' | 'lg';
  /** Share the width equally (phone). */
  stretch?: boolean;
  label: string;
}) {
  const item = size === 'lg' ? 'h-10 px-4 rounded-8 type-body-strong' : 'h-7 px-3 rounded-6 type-label';
  return (
    <div role="tablist" aria-label={label} className={`inline-flex gap-1 bg-inset p-1 ${size === 'lg' ? 'rounded-12' : 'rounded-8'} ${stretch ? 'flex w-full' : ''}`}>
      {options.map((o) => {
        const selected = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="tab"
            aria-selected={selected}
            onClick={() => onChange(o.value)}
            className={`${item} ${stretch ? 'flex-1' : ''} whitespace-nowrap transition-colors ${selected ? 'bg-raised text-fg' : 'text-fg-muted hover:text-fg'}`}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
