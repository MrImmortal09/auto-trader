import { useCallback, useLayoutEffect, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react';

// Phone Trade: the chart and the ladder share the height between the index
// tabs and the positions bar. A handle between them drags the split; the
// ladder always keeps room for its lots row and a few strikes, so the chart
// can never push the options off the screen.

/** The least the chart is dragged down to. */
export const MIN_PHONE_CHART = 140;
/** What the ladder always keeps: its lots and expiry row and three strikes. */
export const MIN_PHONE_LADDER = 220;
/** The handle between them. */
export const SPLIT_HANDLE = 20;
/** The chart's share of the room before it is ever dragged. */
const DEFAULT_SHARE = 0.42;
/** Never shorter than this, even when the window can't fit both. */
const FLOOR = 96;
/** A key press on the handle moves it this far. */
const KEY_STEP = 24;

/** The chart's height in `area` px: `wanted` (the default share when never
 * dragged) within the chart's least and what leaves the ladder its room. */
export function phoneChartHeight(wanted: number | null, area: number): number {
  const most = area - SPLIT_HANDLE - MIN_PHONE_LADDER;
  const want = wanted ?? Math.round((area - SPLIT_HANDLE) * DEFAULT_SHARE);
  return Math.round(Math.max(FLOOR, Math.min(Math.max(want, MIN_PHONE_CHART), most)));
}

/**
 * The split's state. `attach` is the ref for the box the chart, handle and ladder
 * share; `saved` is the last height dragged to (null: never), `onSave` keeps
 * a new one once the finger lifts.
 */
export function usePhoneSplit(saved: number | null, onSave: (h: number) => void) {
  const [box, setBox] = useState<HTMLDivElement | null>(null);
  const [area, setArea] = useState<number | null>(null);
  // The height under a drag, kept here until the pointer lifts, and where the drag began.
  const [dragged, setDragged] = useState<number | null>(null);
  const [start, setStart] = useState<{ y: number; h: number } | null>(null);

  useLayoutEffect(() => {
    if (!box) return;
    const measure = () => setArea(box.clientHeight);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(box);
    return () => ro.disconnect();
  }, [box]);

  const height = area == null ? (saved ?? 220) : phoneChartHeight(dragged ?? saved, area);
  const min = area == null ? height : phoneChartHeight(0, area);
  const max = area == null ? height : phoneChartHeight(Infinity, area);

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    setStart({ y: e.clientY, h: height });
    // Captured, so the lift lands on the handle — never on a Buy under the finger.
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (start && area != null) setDragged(phoneChartHeight(start.h + (e.clientY - start.y), area));
  };
  const onPointerEnd = () => {
    if (start && dragged != null) onSave(dragged);
    setStart(null);
    setDragged(null);
  };
  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const by = e.key === 'ArrowUp' ? -KEY_STEP : e.key === 'ArrowDown' ? KEY_STEP : 0;
    if (!by || area == null) return;
    e.preventDefault();
    onSave(phoneChartHeight(height + by, area));
  };

  const attach = useCallback((el: HTMLDivElement | null) => setBox(el), []);
  return { attach, height, min, max, dragging: dragged != null, handle: { onPointerDown, onPointerMove, onPointerUp: onPointerEnd, onPointerCancel: onPointerEnd, onKeyDown } };
}

export type PhoneSplit = ReturnType<typeof usePhoneSplit>;
