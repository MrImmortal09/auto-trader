// Drawing tools on the KLineChart charts: KLineChart's own overlays, kept per
// instrument in the browser as time and price points, so a drawing shows on
// every timeframe. Stop and target lines are never among them: those always
// come from the engine.

import { useCallback, useRef, useState } from 'react';
import type { Chart, DeepPartial, OverlayCreate, OverlayEvent, OverlayStyle } from 'klinecharts';
import { readStored, writeStored } from '../../lib/execute';
import { todayIst } from '../lib/format';
import { C, FONT } from './chartSetup';
import { DrawHistory } from './undoHistory';

export type DrawTool = 'segment' | 'horizontalStraightLine' | 'rayLine' | 'verticalStraightLine' | 'fibonacciLine';

/** The tools in menu order, and what the banner asks for while one is drawn. */
export const DRAW_TOOLS: { tool: DrawTool; label: string; ask: string }[] = [
  { tool: 'segment', label: 'Trend line', ask: 'Tap where the trend line starts, then where it ends' },
  { tool: 'horizontalStraightLine', label: 'Horizontal line', ask: 'Tap where the line goes' },
  { tool: 'rayLine', label: 'Ray', ask: 'Tap where the ray starts, then where it points' },
  { tool: 'verticalStraightLine', label: 'Vertical line', ask: 'Tap where the line goes' },
  { tool: 'fibonacciLine', label: 'Fibonacci', ask: 'Tap where the move starts, then where it ends' },
];

/** What a chart's Draw menu does to it. */
export interface DrawApi {
  /** Draw `tool` with the next taps; `done` once it is down or given up. */
  start: (tool: DrawTool, done: () => void) => void;
  /** Give up a drawing not finished yet. */
  cancel: () => void;
  /** Every drawing on the instrument shown, gone. */
  clear: () => void;
  /** Undo the last drawing change (or cancel a drawing in progress). */
  undo: () => boolean;
  /** Redo a previously undone change. */
  redo: () => boolean;
  canUndo: () => boolean;
  canRedo: () => boolean;
}

/** The tool being drawn on a chart, if any: `pick` starts one (giving up
 * another), `cancel` gives it up. A drawing given up by the chart itself (a
 * "+ Stop", another instrument) ends it too. `api` must be stable. */
export function useDrawing(api: () => DrawApi | null) {
  const [drawing, setDrawing] = useState<DrawTool | null>(null);
  // The latest pick: an earlier one ending never clears it.
  const turn = useRef(0);
  const pick = useCallback(
    (tool: DrawTool) => {
      const to = api();
      if (!to) return;
      const mine = ++turn.current;
      setDrawing(tool);
      to.start(tool, () => {
        if (turn.current === mine) setDrawing(null);
      });
    },
    [api],
  );
  const cancel = useCallback(() => {
    turn.current += 1;
    setDrawing(null);
    api()?.cancel();
  }, [api]);
  return { drawing, pick, cancel };
}

/** Every drawing's overlay id starts so: the order-line sync leaves them alone. */
export const DRAW_PREFIX = 'draw:';
const GROUP = 'drawings';
/** Saved drawings, by instrument: KLineChart's and Vela's formats differ, so apart. */
export const KLINE_DRAWINGS = 'v2.draw.k.';
export const VELA_DRAWINGS = 'v2.draw.v.';

interface Saved {
  name: DrawTool;
  points: { t: number; v: number }[];
}

const TOOLS = new Set<string>(DRAW_TOOLS.map((t) => t.tool));

// The indicators' light blue: never a stop's red or a target's green.
const STYLES: DeepPartial<OverlayStyle> = {
  line: { color: C.indicator, size: 1 },
  point: { color: C.indicator, borderColor: 'rgba(121, 167, 242, 0.35)', activeColor: C.indicator, activeBorderColor: 'rgba(121, 167, 242, 0.35)' },
  text: { color: C.onAccent, backgroundColor: C.indicator, borderColor: C.indicator, family: FONT, size: 11 },
};

function isSaved(d: unknown): d is Saved {
  const s = d as Partial<Saved> | null;
  return typeof s?.name === 'string' && TOOLS.has(s.name) && Array.isArray(s.points) && s.points.every((p) => Number.isFinite(p?.t) && Number.isFinite(p?.v));
}

function load(key: string): Saved[] {
  const raw = readStored<unknown>(key, []);
  return Array.isArray(raw) ? raw.filter(isSaved) : [];
}

/** Keeps `list` under `key`, or nothing when it is empty. */
export function keepList(key: string, list: unknown[]) {
  if (list.length > 0) return writeStored(key, list);
  try {
    window.localStorage.removeItem(key);
  } catch {
    // Storage blocked: nothing was kept.
  }
}

/** Drops the saved drawings of options that have expired. An option is
 * `INDEX:YYYY-MM-DD:STRIKE:SIDE`; an index or a future is kept. */
export function pruneExpired(prefix: string) {
  const today = todayIst();
  try {
    const ls = window.localStorage;
    for (let i = ls.length - 1; i >= 0; i--) {
      const key = ls.key(i);
      if (!key?.startsWith(prefix)) continue;
      const expiry = /^[^:]+:(\d{4}-\d{2}-\d{2}):/.exec(key.slice(prefix.length))?.[1];
      if (expiry && expiry < today) ls.removeItem(key);
    }
  } catch {
    // Storage blocked: nothing was kept to prune.
  }
}

let made = 0;
const newId = () => `${DRAW_PREFIX}${Date.now().toString(36)}-${(made++).toString(36)}`;

/** One KLineChart's drawings: drawn from storage for the instrument shown, and
 * saved as the user draws, drags or right-clicks one away. */
export class KlineDrawings implements DrawApi {
  private readonly chart: Chart;
  private symbol: string | null = null;
  private kept = new Map<string, Saved>();
  private pending: { id: string; done: () => void } | null = null;
  private history = new DrawHistory<Saved[]>();

  constructor(chart: Chart) {
    this.chart = chart;
  }

  /** The kept list as a snapshot: a deep copy so history is immutable. */
  private snapshot(): Saved[] {
    return [...this.kept.values()].map((d) => ({
      name: d.name,
      points: d.points.map((p) => ({ t: p.t, v: p.v })),
    }));
  }

  /** Push the current state onto the undo stack (before a change). */
  private pushUndo() {
    if (!this.symbol) return;
    this.history.push(this.symbol, this.snapshot());
  }

  /** `symbol`'s saved drawings, drawn afresh. */
  show(symbol: string) {
    this.hide();
    this.symbol = symbol;
    for (const d of load(KLINE_DRAWINGS + symbol)) {
      const id = newId();
      this.kept.set(id, d);
      this.chart.createOverlay(this.overlay(id, d.name, d.points));
    }
  }

  /** Off the chart, as kept: another instrument is coming. */
  hide() {
    this.cancel();
    this.chart.removeOverlay({ groupId: GROUP });
    this.symbol = null;
    this.kept = new Map();
  }

  start(tool: DrawTool, done: () => void) {
    this.cancel();
    if (!this.symbol) return done();
    const id = newId();
    this.pending = { id, done };
    this.chart.createOverlay(this.overlay(id, tool, []));
  }

  cancel() {
    const p = this.pending;
    if (!p) return;
    this.pending = null;
    this.chart.removeOverlay({ id: p.id });
    p.done();
  }

  clear() {
    this.cancel();
    if (this.kept.size > 0) this.pushUndo();
    this.chart.removeOverlay({ groupId: GROUP });
    this.kept.clear();
    this.save();
  }

  undo(): boolean {
    // A drawing in progress: cancel it instead.
    if (this.pending) {
      this.cancel();
      return true;
    }
    if (!this.symbol) return false;
    const prev = this.history.undo(this.symbol, this.snapshot());
    if (!prev) return false;
    this.applySnapshot(prev);
    return true;
  }

  redo(): boolean {
    if (!this.symbol) return false;
    const next = this.history.redo(this.symbol, this.snapshot());
    if (!next) return false;
    this.applySnapshot(next);
    return true;
  }

  canUndo(): boolean {
    return this.pending != null || (this.symbol != null && this.history.canUndo(this.symbol));
  }

  canRedo(): boolean {
    return this.symbol != null && this.history.canRedo(this.symbol);
  }

  /** Replace the drawings on the chart with `snapshot` and save. */
  private applySnapshot(snapshot: Saved[]) {
    this.chart.removeOverlay({ groupId: GROUP });
    this.kept = new Map();
    for (const d of snapshot) {
      const id = newId();
      const points = d.points.map((p) => ({ t: p.t, v: p.v }));
      this.kept.set(id, { name: d.name, points });
      this.chart.createOverlay(this.overlay(id, d.name, points));
    }
    this.save();
  }

  private overlay(id: string, name: DrawTool, points: Saved['points']): OverlayCreate {
    const keep = (e: OverlayEvent<unknown>) => {
      const at = e.overlay.points.flatMap((p) => (Number.isFinite(p.timestamp) && Number.isFinite(p.value) ? [{ t: p.timestamp!, v: p.value! }] : []));
      if (at.length === 0) return;
      const prev = this.kept.get(id);
      if (prev && prev.points.length === at.length && prev.points.every((p, i) => p.t === at[i]?.t && p.v === at[i]?.v)) {
        return;
      }
      this.pushUndo();
      this.kept.set(id, { name, points: at });
      this.save();
    };
    return {
      id,
      name,
      groupId: GROUP,
      // On the candles only: a point is a price.
      paneId: 'candle_pane',
      styles: STYLES,
      points: points.map((p) => ({ timestamp: p.t, value: p.v })),
      onDrawEnd: (e) => {
        if (this.pending?.id !== id) return;
        const { done } = this.pending;
        this.pending = null;
        keep(e);
        done();
      },
      onPressedMoveEnd: keep,
      // KLineChart removes it once this returns.
      onRightClick: () => {
        if (this.pending?.id === id) {
          const { done } = this.pending;
          this.pending = null;
          return done();
        }
        this.pushUndo();
        this.kept.delete(id);
        this.save();
      },
    };
  }

  private save() {
    if (!this.symbol) return;
    keepList(KLINE_DRAWINGS + this.symbol, [...this.kept.values()]);
    pruneExpired(KLINE_DRAWINGS);
  }
}
