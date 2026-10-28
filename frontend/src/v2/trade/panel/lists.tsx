// The trading panel's History and Activity lists, shared by the desktop
// panel and the phone sheet.

import { contractLabel } from '../../../lib/execute';
import { hhmm, hhmmss, shortDate, signedMoney, todayIst, toneOf } from '../../lib/format';
import type { Call, PaperActivity } from '../../lib/types';
import { istClock } from '../model';
import type { ActivityEvent } from './activity';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** Today's manual calls closed, newest first. */
export function HistoryList({ calls, large = false }: { calls: Call[]; large?: boolean }) {
  if (calls.length === 0) return <p className="py-3 type-body text-fg-muted">No trade closed today.</p>;
  return (
    <ul className="flex flex-col">
      {calls.map((c) => (
        <li key={c.key} className={`flex items-center gap-3 border-t border-line ${large ? 'py-3' : 'py-2'}`}>
          <span className="w-12 shrink-0 type-caption text-fg-muted tnum">{hhmm(c.closed_at)}</span>
          <span className="min-w-0 flex-1 truncate type-body-strong text-fg">{contractLabel(c.index, c.strike, c.side)}</span>
          <span className="shrink-0 type-caption text-fg-muted tnum">{c.sold_qty} qty</span>
          {c.outcome && <span className={`shrink-0 type-caption ${TONE_TEXT[c.outcome.tone]}`}>{c.outcome.label}</span>}
          <span className={`w-24 shrink-0 text-right num-small ${TONE_TEXT[toneOf(c.pnl)]}`}>{c.pnl == null ? '—' : signedMoney(c.pnl)}</span>
        </li>
      ))}
    </ul>
  );
}

/** Today's manual fills and the last minute's engine errors, newest first. */
export function ActivityList({ events, large = false }: { events: ActivityEvent[]; large?: boolean }) {
  if (events.length === 0) return <p className="py-3 type-body text-fg-muted">Nothing yet today. Engine errors show here for a minute; the System log keeps them.</p>;
  return (
    <ul className="flex flex-col">
      {events.map((e) => (
        <li key={e.key} className={`flex items-start gap-3 border-t border-line ${large ? 'py-3' : 'py-2'}`}>
          <span className="w-12 shrink-0 type-caption text-fg-muted tnum">{Number.isFinite(e.t) ? istClock(e.t) : ''}</span>
          <span className={`min-w-0 flex-1 type-caption ${e.tone === 'loss' ? 'text-loss' : 'text-fg'}`}>{e.text}</span>
        </li>
      ))}
    </ul>
  );
}

const PAPER_TEXT: Record<PaperActivity['kind'], string> = { buy: 'text-fg', sell: 'text-fg', levels: 'text-fg-muted', reset: 'text-fg-muted' };

/** "10:42:05", or "27 Oct 10:42:05" on another day. */
function when(ts: string): string {
  return ts.startsWith(todayIst()) ? hhmmss(ts) : `${shortDate(ts.slice(0, 10))} ${hhmmss(ts)}`;
}

/** A user's last fills, levels and resets, newest first (the server keeps
 * them in memory, so a restart starts the list over). */
export function PaperActivityList({ items, large = false }: { items: PaperActivity[] | null; large?: boolean }) {
  if (items == null) return <p className="py-3 type-body text-fg-muted">Loading…</p>;
  if (items.length === 0) return <p className="py-3 type-body text-fg-muted">Nothing yet. Buys, sells, stops and targets show here until the server next restarts.</p>;
  return (
    <ul className="flex flex-col">
      {items.map((a, i) => (
        <li key={`${a.t}-${i}`} className={`flex items-baseline gap-3 border-t border-line ${large ? 'py-3' : 'py-2'}`}>
          <span className="min-w-[64px] shrink-0 whitespace-nowrap type-caption text-fg-muted tnum">{when(a.t)}</span>
          <span className={`min-w-0 flex-1 type-caption ${PAPER_TEXT[a.kind]}`}>{a.message}</span>
        </li>
      ))}
    </ul>
  );
}
