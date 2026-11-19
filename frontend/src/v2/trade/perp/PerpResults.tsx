import type { HlPerpRules } from '../../lib/types';
import { searchPerps, usePerpCatalog } from './catalog';

/**
 * The Hyperliquid perps on the app's list that match `query`, for a search's
 * results: each a row that charts it (`onPick`). Only added perps — a
 * close-only one is held, not found. Nothing while the query matches none.
 */
export function PerpResults({ query, onPick, current, compact, limit = 6, heading = true }: {
  query: string;
  onPick: (r: HlPerpRules) => void;
  /** The perp shown now, marked. */
  current?: string | null;
  /** Phone: 44 pt rows. */
  compact: boolean;
  limit?: number;
  heading?: boolean;
}) {
  const { list } = usePerpCatalog();
  const rows = searchPerps(list, query, limit);
  if (rows.length === 0) return null;
  const row = compact ? 'h-11' : 'h-8';
  return (
    <div role="group" aria-label="Hyperliquid perps">
      {heading && <div className="px-2 pb-1 pt-2 type-eyebrow text-fg-muted">Hyperliquid perps</div>}
      {rows.map((r) => {
        const on = current != null && current.toLowerCase() === r.key.toLowerCase();
        return (
          <button
            key={r.key}
            type="button"
            role="option"
            aria-selected={on}
            title={`${r.name} · ${r.coin} on ${r.venue}, up to ${r.leverage_cap}x`}
            onClick={() => onPick(r)}
            className={`flex w-full items-center gap-2 rounded-6 px-2 text-left ${on ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'} ${row}`}
          >
            <span className={`shrink-0 whitespace-nowrap type-label ${on ? 'text-accent' : 'text-fg'}`}>{r.label}</span>
            <span className="shrink-0 rounded-4 border border-line px-1 type-caption text-fg-muted">PERP</span>
            <span className="min-w-0 truncate type-caption">
              {r.venue} · {r.leverage_cap}x
            </span>
          </button>
        );
      })}
    </div>
  );
}
