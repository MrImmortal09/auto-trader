import type { ExecuteFrame } from '../../types';
import { fmtSpot, INDEX_ORDER } from '../../lib/execute';

/** One tab per index with its live spot and day change. */
export function IndexTabs({ frame, active, onSelect }: {
  frame: ExecuteFrame | null;
  active: string;
  onSelect: (index: string) => void;
}) {
  const byName = new Map((frame?.indices ?? []).map((i) => [i.index, i]));
  return (
    <div role="tablist" aria-label="Index" className="flex gap-2 overflow-x-auto no-scrollbar -mx-1 px-1 py-0.5">
      {INDEX_ORDER.map((name, i) => {
        const info = byName.get(name);
        const selected = name === active;
        const chg = info?.change_pct ?? null;
        return (
          <button
            key={name}
            type="button"
            role="tab"
            aria-selected={selected}
            onClick={() => onSelect(name)}
            title={`${name} (key ${i + 1})`}
            className={`shrink-0 min-w-[112px] rounded-lg border px-3 py-2 text-left transition-all ${
              selected
                ? 'bg-primary border-primary text-on-primary shadow-md'
                : 'bg-surface-container-lowest border-outline-variant text-on-surface hover:border-primary/50 hover:bg-surface-container-low'
            }`}
          >
            <div className="flex items-center gap-1.5 text-[11px] font-extrabold tracking-wider">
              <kbd
                className={`hidden md:inline-grid place-items-center w-4 h-4 rounded text-[9px] font-bold ${
                  selected ? 'bg-on-primary/20 text-on-primary' : 'bg-surface-container-high text-on-surface-variant'
                }`}
              >
                {i + 1}
              </kbd>
              {name}
            </div>
            <div className="mt-0.5 flex items-baseline gap-1.5">
              <span className="font-mono-code text-sm font-bold tabular-nums">{info?.spot != null ? fmtSpot(info.spot) : '—'}</span>
              {chg != null && (
                <span
                  className={`text-[10px] font-bold tabular-nums ${
                    selected ? 'text-on-primary/85' : chg >= 0 ? 'text-secondary' : 'text-error'
                  }`}
                >
                  {chg >= 0 ? '▲' : '▼'}
                  {Math.abs(chg).toFixed(2)}%
                </span>
              )}
            </div>
          </button>
        );
      })}
    </div>
  );
}
