import { SCREENS, type ScreenId } from './screens';

/** Where each screen lives in the current app until v2 builds it. */
const V1_PATH: Partial<Record<ScreenId, string>> = {
  scalp: '/execute',
  market: '/market',
  system: '/system',
  settings: '/settings',
};

/** A screen v2 doesn't have yet: a link to the same screen in the current app. */
export function Placeholder({ screen }: { screen: ScreenId }) {
  const s = SCREENS.find((x) => x.id === screen)!;
  const Icon = s.icon;
  return (
    <div className="grid min-h-full place-items-center p-8">
      <div className="flex max-w-sm flex-col items-center gap-3 text-center">
        <span className="grid size-12 place-items-center rounded-12 bg-panel text-fg-muted">
          <Icon size={22} strokeWidth={1.75} />
        </span>
        <h2 className="type-title text-fg">{s.label} is coming to the new design</h2>
        <p className="type-body text-fg-muted">Until then it&apos;s in the current app, which keeps working as it does today.</p>
        <a
          href={V1_PATH[screen] ?? '/'}
          className="inline-flex h-8 items-center rounded-6 bg-raised px-3 type-button text-fg transition-colors hover:bg-line"
        >
          Open {s.label} in the current app
        </a>
      </div>
    </div>
  );
}
