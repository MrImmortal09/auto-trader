import { Ellipsis } from 'lucide-react';
import { SCREENS, TAB_SCREENS, type ScreenId } from './screens';

// Figma "Nav item" (desktop rail) and "Tab item" (phone tab bar). `screens`
// is what this person may open — see `visibleScreens`.

export function NavRail({ screens, screen, onSelect }: { screens: ScreenId[]; screen: ScreenId; onSelect: (s: ScreenId) => void }) {
  return (
    <nav aria-label="Screens" className="flex w-[72px] shrink-0 flex-col items-center gap-2 border-r border-line bg-inset py-4">
      <div aria-hidden className="mb-4 grid size-10 place-items-center rounded-8 bg-raised type-label font-semibold text-fg">
        AT
      </div>
      {SCREENS.filter((s) => screens.includes(s.id)).map(({ id, label, icon: Icon }) => {
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

/** Phone: Desk, Trade, Market, and More (whatever else this person may open). */
export function TabBar({ screens, screen, onSelect }: { screens: ScreenId[]; screen: ScreenId; onSelect: (s: ScreenId) => void }) {
  const tabs = SCREENS.filter((s) => TAB_SCREENS.includes(s.id) && screens.includes(s.id));
  const rest = screens.filter((s) => !TAB_SCREENS.includes(s));
  const more = rest.includes(screen);
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
        {rest.length > 0 && (
          <button type="button" aria-current={more ? 'page' : undefined} onClick={() => onSelect(more ? screen : rest[0])} className={tab(more)}>
            <Ellipsis size={22} strokeWidth={1.75} />
            More
          </button>
        )}
      </div>
    </nav>
  );
}
