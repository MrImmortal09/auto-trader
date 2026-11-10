import { useCallback, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { daysLeftLabel, expiryLabel } from '../../lib/execute';
import { Popover } from '../ui/Popover';
import { expiryWords, type ExpiryRow } from './expiries';

/** "10 Nov": an expiry's day and month. */
function dayMonth(iso: string): string {
  const [y, m, d] = iso.split('-').map(Number);
  return new Date(Date.UTC(y, m - 1, d)).toLocaleDateString('en-IN', { day: 'numeric', month: 'short', timeZone: 'UTC' });
}

/**
 * Which expiry the ladder (or a chart's option picker) is on: a button
 * showing it, and a list of every expiry the index's options are listed for.
 * `value` null means the nearest, which the server picks.
 */
export function ExpiryPicker({ expiries, value, onChange, size = 'sm', align = 'left', short = false }: {
  expiries: ExpiryRow[] | undefined;
  /** `YYYY-MM-DD`, or null for the nearest. */
  value: string | null;
  onChange: (date: string | null) => void;
  size?: 'sm' | 'lg';
  align?: 'left' | 'right';
  /** "10 Nov · 5d" rather than "Expiry Tue, 10 Nov · 5 days left". */
  short?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const close = useCallback(() => setOpen(false), []);
  const all = expiries ?? [];
  const current = all.find((e) => e.date === value) ?? all[0];
  const text = short
    ? current
      ? `${dayMonth(current.date)} · ${current.days_to_expiry <= 0 ? 'today' : `${current.days_to_expiry}d`}`
      : value
        ? dayMonth(value)
        : 'Expiry'
    : current
      ? `Expiry ${expiryLabel(current.date)} · ${daysLeftLabel(current.days_to_expiry)}`
      : value
        ? `Expiry ${expiryLabel(value)}`
        : 'Expiry';
  const lg = size === 'lg';
  return (
    <div className="relative">
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={short && current ? `Expiry ${expiryLabel(current.date)}, ${daysLeftLabel(current.days_to_expiry)}` : undefined}
        disabled={all.length === 0}
        onClick={() => setOpen((o) => !o)}
        className={`inline-flex items-center gap-1 whitespace-nowrap rounded-6 border border-line-strong/50 text-fg-muted transition-colors duration-120 hover:text-fg disabled:opacity-50 ${lg ? 'h-10 px-3 type-label' : 'h-6 px-2 type-caption'}`}
      >
        {text}
        <ChevronDown size={lg ? 16 : 12} />
      </button>
      <Popover open={open} onClose={close} align={align} backdrop={short}>
        <div className="px-2 pt-1 pb-1.5 type-caption text-fg-muted">Expiry</div>
        <div role="listbox" aria-label="Expiry" className="max-h-72 overflow-y-auto">
          {all.map((e, i) => {
            const on = current?.date === e.date;
            const { label, tag } = expiryWords(e, all);
            return (
              <button
                key={e.date}
                type="button"
                role="option"
                aria-selected={on}
                onClick={() => {
                  // The nearest is the server's default, so it is saved as "none".
                  onChange(i === 0 ? null : e.date);
                  close();
                }}
                className={`flex h-10 w-full items-center gap-2 rounded-6 px-3 text-left ${on ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}
              >
                <span className="min-w-0 flex-1 truncate type-label tnum">{label}</span>
                {tag && <span className="type-caption">{tag}</span>}
                <span className="type-caption tnum">lot {e.lot_size}</span>
              </button>
            );
          })}
        </div>
      </Popover>
    </div>
  );
}
