// Standard pivot points: TradingView's "Pivot Points Standard", Traditional
// type, daily — each day's levels from the day before's high, low and close.
// An Indian instrument's day is its IST session; a perp's, trading around the
// clock, is the UTC day, as Hyperliquid's own chart (TradingView) counts it.

import { inSession, istDay, utcDay, type Bar } from './model';

export interface PivotLevels {
  p: number;
  /** R1 … R5. */
  r: number[];
  /** S1 … S5. */
  s: number[];
}

/** Traditional floor pivots from one day's high, low and close. */
export function standardPivots(h: number, l: number, c: number): PivotLevels {
  const p = (h + l + c) / 3;
  return {
    p,
    r: [2 * p - l, p + (h - l), 2 * p + (h - 2 * l), 3 * p + (h - 3 * l), 4 * p + (h - 4 * l)],
    s: [2 * p - h, p - (h - l), 2 * p - (2 * h - l), 3 * p - (3 * h - l), 4 * p - (4 * h - l)],
  };
}

/** The day number a bar's pivots go by: its IST day, or a perp's (`allDay`) UTC day. */
export function pivotDay(allDay: boolean): (t: number) => number {
  return allDay ? utcDay : istDay;
}

/**
 * Each day's pivots, by day number (`pivotDay`), from the session bars of
 * the trading day before it: its high, low and last close. The first day in
 * `bars` has nothing before it, so no levels. `allDay` (a perp, trading
 * around the clock) counts every bar of the UTC day; the bars, read from an
 * IST midnight, start part-way into their first UTC day, which then gives
 * the next day no levels rather than wrong ones.
 */
export function pivotsByDay(bars: Bar[], allDay = false): Map<number, PivotLevels> {
  const dayOf = pivotDay(allDay);
  const days = new Map<number, { h: number; l: number; c: number; t: number }>();
  let first = Infinity;
  for (const b of bars) {
    if (!allDay && !inSession(b.t)) continue;
    first = Math.min(first, b.t);
    const d = dayOf(b.t);
    const day = days.get(d);
    if (!day) days.set(d, { h: b.h, l: b.l, c: b.c, t: b.t });
    else {
      day.h = Math.max(day.h, b.h);
      day.l = Math.min(day.l, b.l);
      if (b.t >= day.t) {
        day.c = b.c;
        day.t = b.t;
      }
    }
  }
  const order = [...days.keys()].sort((a, b) => a - b);
  const out = new Map<number, PivotLevels>();
  for (let i = 1; i < order.length; i++) {
    // A perp's day before must be there whole, from its 00:00 UTC.
    if (allDay && first > order[i - 1] * 86_400_000) continue;
    const prev = days.get(order[i - 1])!;
    out.set(order[i], standardPivots(prev.h, prev.l, prev.c));
  }
  return out;
}

/** The levels of a day, named the way the chart labels them, nearest first. */
export function pivotLines(levels: PivotLevels): { name: string; value: number }[] {
  return [
    { name: 'P', value: levels.p },
    ...levels.r.map((value, i) => ({ name: `R${i + 1}`, value })),
    ...levels.s.map((value, i) => ({ name: `S${i + 1}`, value })),
  ];
}
