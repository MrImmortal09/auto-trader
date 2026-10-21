import type { ReactNode } from 'react';

/** A System panel: a title, a note beside it, and its body. */
export function Card({ title, note, aside, children }: { title: string; note?: ReactNode; aside?: ReactNode; children: ReactNode }) {
  return (
    <section className="flex min-w-0 flex-col rounded-12 bg-panel p-4">
      <div className="flex items-baseline gap-2">
        <h3 className="type-title text-fg">{title}</h3>
        {note && <span className="type-caption text-fg-muted">{note}</span>}
        {aside && <span className="ml-auto self-center">{aside}</span>}
      </div>
      <div className="mt-2 flex-1">{children}</div>
    </section>
  );
}

/** A label and its number, with a line under the label when there's more
 * to say. `alarm` turns the number brick. */
export function CountRow({ label, value, hint, alarm = false }: { label: string; value: ReactNode; hint?: ReactNode; alarm?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-4 py-1.5">
      <span className="min-w-0">
        <span className="block type-body text-fg-muted">{label}</span>
        {hint && <span className="block type-caption text-fg-muted">{hint}</span>}
      </span>
      <span className={`shrink-0 text-right num-medium ${alarm ? 'text-loss' : 'text-fg'}`}>{value}</span>
    </div>
  );
}

/** A section heading over a group of panels. */
export function GroupTitle({ title, note }: { title: string; note?: ReactNode }) {
  return (
    <div className="mt-2 flex items-baseline gap-2">
      <h2 className="type-title text-fg">{title}</h2>
      {note && <span className="type-caption text-fg-muted">{note}</span>}
    </div>
  );
}
