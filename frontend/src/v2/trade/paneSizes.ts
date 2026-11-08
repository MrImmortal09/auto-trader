// The desktop Trade screen's resizable widths: the options ladder, how two or
// three charts share their row, and the trading panel's desk column. Heights
// (the trading panel's) live in `panel/height.ts`.

import { useLayoutEffect, useState, type RefObject } from 'react';

export const DEFAULT_LADDER_WIDTH = 380;
/** Narrower and the strike column and both prices stop fitting. */
export const MIN_LADDER_WIDTH = 300;
export const MAX_LADDER_WIDTH = 760;
/** Each chart keeps at least this much, header controls included. */
export const MIN_CHART_WIDTH = 240;

/** The trading panel's left column: the owner's desk, or a user's portfolio. */
export const DEFAULT_DESK_WIDTH = 260;
export const MIN_DESK_WIDTH = 200;
export const MAX_DESK_WIDTH = 480;
/** The tabs beside that column keep at least this much. */
export const MIN_PANEL_BODY = 360;

/** A saved width within `min`..`max`, or the default when it isn't a number. */
export function savedWidth(v: unknown, min: number, max: number, fallback: number): number {
  return typeof v === 'number' && Number.isFinite(v) ? Math.round(Math.max(min, Math.min(max, v))) : fallback;
}

export function equalShares(n: number): number[] {
  return Array.from({ length: n }, () => 1 / n);
}

/** A saved split of `n` charts: `n` positive fractions adding up to 1, else null. */
export function savedShares(v: unknown, n: number): number[] | null {
  if (!Array.isArray(v) || v.length !== n || !v.every((x) => typeof x === 'number' && Number.isFinite(x) && x > 0)) return null;
  const sum = v.reduce((a: number, b: number) => a + b, 0);
  return Math.abs(sum - 1) < 1e-6 ? v : v.map((x: number) => x / sum);
}

/** `shares` with chart `i` set to `left` of the row, taken from (or given
 * to) chart `i + 1`, so the others keep their widths. */
export function withLeftShare(shares: number[], i: number, left: number): number[] {
  const next = [...shares];
  const pair = shares[i] + shares[i + 1];
  next[i] = left;
  next[i + 1] = pair - left;
  return next;
}

/** An element's width, kept current as it resizes; 0 before it is laid out.
 * `mounted` changes when the element comes and goes (a panel opened, the
 * desktop layout replacing the phone's), so the new one is measured. */
export function useWidth(ref: RefObject<HTMLElement | null>, mounted: unknown): number {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setWidth(el.getBoundingClientRect().width);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref, mounted]);
  return width;
}
