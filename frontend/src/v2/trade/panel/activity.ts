// Today's manual fills and the last minute's engine errors (the stream keeps
// no more), as one list of events.

import { contractLabel } from '../../../lib/execute';
import { price } from '../../lib/format';
import type { FillMark } from '../../lib/types';

export interface EngineError {
  instrument: string;
  position_id: string | null;
  message: string;
  /** Unix ms. */
  ts: number;
}

export interface ActivityEvent {
  key: string;
  /** Unix ms. */
  t: number;
  text: string;
  tone: 'profit' | 'loss' | 'neutral';
}

/** Newest first. */
export function activityEvents(fills: FillMark[], errors: EngineError[]): ActivityEvent[] {
  const out: ActivityEvent[] = [];
  for (const f of fills) {
    if (!f.scalp) continue;
    const t = Date.parse(`${f.t.replace(' ', 'T')}+05:30`);
    const what = f.action === 'BUY' ? 'Bought' : 'Sold';
    const why = f.reason ? ` · ${f.reason.toLowerCase().replace(/_/g, ' ')}` : '';
    out.push({ key: `fill:${f.id}`, t, text: `${what} ${f.qty} ${contractLabel(f.index, f.strike, f.side)} @ ${price(f.price)}${why}`, tone: f.action === 'BUY' ? 'neutral' : 'profit' });
  }
  errors.forEach((e, i) => {
    out.push({ key: `err:${e.ts}:${e.instrument}:${e.position_id ?? ''}:${i}`, t: e.ts, text: `${e.instrument}: ${e.message}`, tone: 'loss' });
  });
  return out.sort((a, b) => b.t - a.t);
}
