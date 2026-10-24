// The trading panel's History and Activity lists, shared by the desktop
// panel and the phone sheet.

import { contractLabel } from '../../../lib/execute';
import { hhmm, signedMoney, toneOf } from '../../lib/format';
import type { Call } from '../../lib/types';
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

/** Today's manual fills and the engine's recent errors, newest first. */
export function ActivityList({ events, large = false }: { events: ActivityEvent[]; large?: boolean }) {
  if (events.length === 0) return <p className="py-3 type-body text-fg-muted">Nothing yet today.</p>;
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
