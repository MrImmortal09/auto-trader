// Standard pivot points: TradingView's "Pivot Points Standard", Traditional
// type, daily — each day's levels from the day before's high, low and close.

import { inSession, istDay, type Bar } from './model';

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

/**
 * Each day's pivots, by IST day number, from the session bars of the trading
 * day before it: its high, low and last close. The first day in `bars` has
 * nothing before it, so no levels.
 */
export function pivotsByDay(bars: Bar[]): Map<number, PivotLevels> {
  const days = new Map<number, { h: number; l: number; c: number; t: number }>();
  for (const b of bars) {
    if (!inSession(b.t)) continue;
    const d = istDay(b.t);
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
