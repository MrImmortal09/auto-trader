import type { ReactNode } from 'react';
import { HealthDot } from '../ui/Health';
import type { Level } from '../lib/types';

/** A settings section: its title over a white hairline card of rows. */
export function Section({ id, title, lead, children }: { id?: string; title?: string; lead?: ReactNode; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-6">
      {title && <h2 className="mb-3 type-title text-fg">{title}</h2>}
      {lead && <p className="mb-3 type-body text-fg-muted">{lead}</p>}
      <div className="rounded-12 border border-line bg-panel">{children}</div>
    </section>
  );
}

/** One setting: its name and what it does, with its control on the right —
 * or under them, `stacked`, for a wide one. `indent` sets it under the
 * toggle it depends on, with the same hairline. */
export function Row({ title, caption, indent = false, stacked = false, children }: {
  title: ReactNode;
  caption?: ReactNode;
  indent?: boolean;
  stacked?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className={`flex border-t border-line py-3.5 pr-4 first:border-t-0 ${stacked ? 'flex-col gap-2.5' : 'items-center gap-4'} ${indent ? 'ml-4 pl-4' : 'pl-4'}`}>
      <div className="min-w-0 flex-1">
        <div className="type-body-strong text-fg">{title}</div>
        {caption && <div className="mt-0.5 type-caption text-fg-subtle">{caption}</div>}
      </div>
      {children}
    </div>
  );
}

/** "● Connected", "● Not connected". */
export function StateDot({ level, children }: { level: Level; children: ReactNode }) {
  return (
    <span className="flex shrink-0 items-center gap-1.5 type-caption text-fg-muted">
      <HealthDot level={level} />
      {children}
    </span>
  );
}

/** A link styled as the `neutral` sm button: the few settings that still
 * open in the old design (/v1/settings). */
export const LINK_BUTTON = 'inline-flex h-7 items-center rounded-6 bg-raised px-2.5 type-button text-fg transition-colors duration-120 hover:bg-raised-strong';

/** Figma "Section nav item": blue marks the section being read. */
export function NavItem({ selected, onClick, children }: { selected: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      aria-current={selected ? 'true' : undefined}
      onClick={onClick}
      className={`flex h-9 w-full items-center rounded-8 px-3 text-left type-body transition-colors duration-120 ${selected ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:text-fg'}`}
    >
      {children}
    </button>
  );
}
