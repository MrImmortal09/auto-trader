import { Ellipsis } from 'lucide-react';
import { SCREENS, type ScreenId } from './screens';

// Figma "Nav item" (desktop rail) and "Tab item" (phone tab bar).

export function NavRail({ screen, onSelect }: { screen: ScreenId; onSelect: (s: ScreenId) => void }) {
  return (
    <nav aria-label="Screens" className="flex w-[72px] shrink-0 flex-col items-center gap-2 border-r border-line bg-inset py-4">
      <div aria-hidden className="mb-4 grid size-10 place-items-center rounded-8 bg-raised type-label font-semibold text-fg">
        AT
      </div>
      {SCREENS.map(({ id, label, icon: Icon }) => {
        const selected = id === screen;
        return (
          <button
            key={id}
            type="button"
            aria-current={selected ? 'page' : undefined}
            onClick={() => onSelect(id)}
            className={`flex w-14 flex-col items-center gap-1 rounded-8 py-2 type-caption transition-colors ${selected ? 'bg-raised text-fg' : 'text-fg-muted hover:text-fg'}`}
          >
            <Icon size={20} strokeWidth={1.75} />
            {label}
          </button>
        );
      })}
    </nav>
  );
}

/** Phone: Desk, Trade, Market, and More (System and Settings). */
export function TabBar({ screen, onSelect }: { screen: ScreenId; onSelect: (s: ScreenId) => void }) {
  const tabs = SCREENS.slice(0, 3);
  const more = screen === 'system' || screen === 'settings';
  const tab = (selected: boolean) =>
    `flex flex-1 flex-col items-center justify-center gap-1 type-caption transition-colors ${selected ? 'text-fg' : 'text-fg-muted'}`;
  return (
    <nav aria-label="Screens" className="z-30 shrink-0 border-t border-line bg-inset pb-safe">
      <div className="flex h-[50px]">
        {tabs.map(({ id, label, icon: Icon }) => (
          <button key={id} type="button" aria-current={id === screen ? 'page' : undefined} onClick={() => onSelect(id)} className={tab(id === screen)}>
            <Icon size={22} strokeWidth={1.75} />
            {label}
          </button>
        ))}
        <button type="button" aria-current={more ? 'page' : undefined} onClick={() => onSelect(more ? screen : 'system')} className={tab(more)}>
          <Ellipsis size={22} strokeWidth={1.75} />
          More
        </button>
      </div>
    </nav>
  );
}
