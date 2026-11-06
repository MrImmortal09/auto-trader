// Keyboard shortcuts for chart drawing undo/redo: Cmd+Z / Ctrl+Z, and redo.
// Routes to the chart last hovered or clicked, or the first on screen.

import { useEffect, useRef } from 'react';
import type { DrawApi } from './drawings';

/** A text field: a key there is typing, not a shortcut. */
export function editable(t: EventTarget | null): boolean {
  return (
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable)
  );
}

export function isMac(): boolean {
  return typeof navigator !== 'undefined' && /(Mac|iPhone|iPod|iPad)/i.test(navigator.platform || navigator.userAgent);
}

export function isUndoKey(e: KeyboardEvent): boolean {
  const mod = isMac() ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  return mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'z';
}

export function isRedoKey(e: KeyboardEvent): boolean {
  if (e.altKey) return false;
  if (isMac()) {
    return e.metaKey && !e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'z';
  }
  const ctrlShiftZ = e.ctrlKey && !e.metaKey && e.shiftKey && e.key.toLowerCase() === 'z';
  const ctrlY = e.ctrlKey && !e.metaKey && !e.shiftKey && e.key.toLowerCase() === 'y';
  return ctrlShiftZ || ctrlY;
}

interface ChartEntry {
  chartId: string;
  api: () => DrawApi | null;
  isPlacing: () => boolean;
}

const charts = new Map<string, ChartEntry>();
const order: string[] = [];
let activeId: string | null = null;
let listening = false;

function targetChart(): ChartEntry | undefined {
  if (activeId && charts.has(activeId)) return charts.get(activeId);
  const first = order.find((id) => charts.has(id));
  return first ? charts.get(first) : undefined;
}

function onWindowKey(e: KeyboardEvent) {
  const undo = isUndoKey(e);
  const redo = isRedoKey(e);
  if (!undo && !redo) return;
  if (editable(e.target)) return;

  const chart = targetChart();
  if (!chart || chart.isPlacing()) return;

  const api = chart.api();
  if (!api) return;

  if (undo) {
    if (api.undo()) e.preventDefault();
  } else if (redo) {
    if (api.redo()) e.preventDefault();
  }
}

function startListening() {
  if (listening || typeof window === 'undefined') return;
  window.addEventListener('keydown', onWindowKey, true);
  listening = true;
}

function stopListening() {
  if (!listening || typeof window === 'undefined') return;
  window.removeEventListener('keydown', onWindowKey, true);
  listening = false;
}

export function markChartActive(chartId: string) {
  if (charts.has(chartId)) activeId = chartId;
}

/** Hook to register a chart for undo/redo shortcuts and pointer tracking. */
export function useChartKeymap(chartId: string, api: () => DrawApi | null, isPlacing: boolean) {
  const placingRef = useRef(isPlacing);
  const apiRef = useRef(api);

  useEffect(() => {
    placingRef.current = isPlacing;
    apiRef.current = api;
  });

  useEffect(() => {
    charts.set(chartId, {
      chartId,
      api: () => apiRef.current(),
      isPlacing: () => placingRef.current,
    });
    if (!order.includes(chartId)) order.push(chartId);
    startListening();

    return () => {
      charts.delete(chartId);
      const idx = order.indexOf(chartId);
      if (idx >= 0) order.splice(idx, 1);
      if (activeId === chartId) activeId = null;
      if (charts.size === 0) stopListening();
    };
  }, [chartId]);
}
