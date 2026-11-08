// Drawing tools on the KLineChart charts: KLineChart's own overlays, and the
// few it doesn't ship (`drawTools.ts`), kept per instrument in the browser as
// time and price points, so a drawing shows on every timeframe. Stop and
// target lines are never among them: those always come from the engine, and
// nothing here (erasing, locking, hiding, clearing) ever touches them.

import { useCallback, useRef, useState, useSyncExternalStore } from 'react';
import type { Chart, DeepPartial, OverlayCreate, OverlayEvent, OverlayStyle } from 'klinecharts';
import { readStored, writeStored } from '../../lib/execute';
import { todayIst } from '../lib/format';
import { alpha } from '../lib/palette';
import { C, FONT } from './chartSetup';
import './drawTools';
import { DrawHistory } from './undoHistory';

export type DrawTool =
  | 'segment'
  | 'rayLine'
  | 'straightLine'
  | 'horizontalStraightLine'
  | 'horizontalRayLine'
  | 'horizontalSegment'
  | 'verticalStraightLine'
  | 'priceLine'
  | 'fibonacciLine'
  | 'parallelStraightLine'
  | 'priceChannelLine'
  | 'v2Path'
  | 'v2Rect'
  | 'brush'
  | 'v2Text'
  | 'simpleAnnotation'
  | 'v2Ruler';

/** Every tool, and what the banner asks for while one is drawn. */
export const DRAW_TOOLS: { tool: DrawTool; label: string; ask: string }[] = [
  { tool: 'segment', label: 'Trend line', ask: 'Tap where the trend line starts, then where it ends' },
  { tool: 'rayLine', label: 'Ray', ask: 'Tap where the ray starts, then where it points' },
  { tool: 'straightLine', label: 'Extended line', ask: 'Tap two points the line goes through' },
  { tool: 'horizontalStraightLine', label: 'Horizontal line', ask: 'Tap where the line goes' },
  { tool: 'horizontalRayLine', label: 'Horizontal ray', ask: 'Tap where the ray starts, then the side it runs to' },
  { tool: 'horizontalSegment', label: 'Horizontal segment', ask: 'Tap where it starts, then where it ends' },
  { tool: 'verticalStraightLine', label: 'Vertical line', ask: 'Tap where the line goes' },
  { tool: 'priceLine', label: 'Price line', ask: 'Tap the price to mark' },
  { tool: 'fibonacciLine', label: 'Fibonacci', ask: 'Tap where the move starts, then where it ends' },
  { tool: 'parallelStraightLine', label: 'Parallel channel', ask: 'Tap two points for one side, then one for the other' },
  { tool: 'priceChannelLine', label: 'Price channel', ask: 'Tap two points for one side, then one for the other' },
  { tool: 'v2Path', label: 'Path', ask: 'Tap each point; double-click or press Enter to finish' },
  { tool: 'v2Rect', label: 'Rectangle', ask: 'Tap one corner, then the opposite one' },
  { tool: 'brush', label: 'Brush', ask: 'Press and drag to draw' },
  { tool: 'v2Text', label: 'Text', ask: 'Tap where the text goes' },
  { tool: 'simpleAnnotation', label: 'Note', ask: 'Tap the bar the note is for' },
  { tool: 'v2Ruler', label: 'Measure', ask: 'Tap where to measure from, then to' },
];

/** The phone's Draw menu: the first, simplest few. */
export const PHONE_TOOLS: DrawTool[] = ['segment', 'horizontalStraightLine', 'rayLine', 'verticalStraightLine', 'fibonacciLine'];

/** Tools that carry words, asked for once placed. */
const WORDED = new Set<DrawTool>(['v2Text', 'simpleAnnotation']);
/** Measured, never kept. */
const RULER: DrawTool = 'v2Ruler';

/** What a chart's drawing controls do to it. */
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
  /** End a path at the points tapped so far. */
  finish?: () => void;
  /** While on, a tap on a drawing removes it. */
  setErasing?: (on: boolean) => void;
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

// ── The toolbar's switches, the same on every KLine chart ─────────────── //

export interface DrawPrefs {
  /** New and moved points snap to the nearest open, high, low or close. */
  magnet: boolean;
  /** Drawings can't be moved or erased. */
  locked: boolean;
  /** Drawings are off the chart (still kept). */
  hidden: boolean;
}

const PREFS_KEY = 'v2.drawPrefs.k';
const prefListeners = new Set<() => void>();
let prefs: DrawPrefs = (() => {
  const s = readStored<Partial<DrawPrefs>>(PREFS_KEY, {});
  return { magnet: s.magnet === true, locked: s.locked === true, hidden: s.hidden === true };
})();

/** The toolbar's magnet, lock and hide, kept in this browser, and a setter. */
export function useDrawPrefs(): [DrawPrefs, (change: Partial<DrawPrefs>) => void] {
  const now = useSyncExternalStore(
    (l) => {
      prefListeners.add(l);
      return () => prefListeners.delete(l);
    },
    () => prefs,
  );
  return [
    now,
    (change: Partial<DrawPrefs>) => {
      prefs = { ...prefs, ...change };
      writeStored(PREFS_KEY, prefs);
      prefListeners.forEach((l) => l());
    },
  ];
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
  /** A text's or a note's words. */
  text?: string;
}

const TOOLS = new Set<string>(DRAW_TOOLS.map((t) => t.tool));

// The user's own marks, in the accent: never a stop's red, a target's green or
// the VWAP's indicator blue. The handles are accent dots in a faint accent
// halo, a label (Fibonacci's levels, a note) is white on accent, like the
// chart's other tags, and a rectangle is a faint accent fill.
const HALO = alpha(C.accent, 0.35);
const STYLES: DeepPartial<OverlayStyle> = {
  line: { color: C.accent, size: 1 },
  point: { color: C.accent, borderColor: HALO, activeColor: C.accent, activeBorderColor: HALO },
  text: { color: C.onAccent, backgroundColor: C.accent, borderColor: C.accent, family: FONT, size: 11 },
  polygon: { style: 'stroke_fill', color: alpha(C.accent, 0.1), borderColor: C.accent, borderSize: 1 },
};
/** The brush draws a little heavier, so a stroke reads as one. */
const BRUSH_STYLES: DeepPartial<OverlayStyle> = { ...STYLES, line: { color: C.accent, size: 2 } };

function isSaved(d: unknown): d is Saved {
  const s = d as Partial<Saved> | null;
  return (
    typeof s?.name === 'string' &&
    TOOLS.has(s.name) &&
    s.name !== RULER &&
    Array.isArray(s.points) &&
    s.points.every((p) => Number.isFinite(p?.t) && Number.isFinite(p?.v)) &&
    (s.text === undefined || typeof s.text === 'string')
  );
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

/** Asks for a text's words where it was placed (`x`, `y` on the chart);
 * `done(null)` when given up. */
export type AskText = (at: { x: number; y: number }, initial: string, done: (text: string | null) => void) => void;

const copy = (d: Saved): Saved => ({ name: d.name, points: d.points.map((p) => ({ t: p.t, v: p.v })), ...(d.text !== undefined ? { text: d.text } : {}) });

/** One KLineChart's drawings: drawn from storage for the instrument shown, and
 * saved as the user draws, drags, writes, erases or right-clicks one away. */
export class KlineDrawings implements DrawApi {
  private readonly chart: Chart;
  private symbol: string | null = null;
  private kept = new Map<string, Saved>();
  private pending: { id: string; tool: DrawTool; done: () => void } | null = null;
  /** The ruler on the chart, if any: gone with the next tool, never kept. */
  private ruler: string | null = null;
  private erasing = false;
  private prefs: DrawPrefs = prefs;
  private ask: AskText | null = null;
  private history = new DrawHistory<Saved[]>();

  constructor(chart: Chart) {
    this.chart = chart;
  }

  /** Where a text's words are asked for (the chart component's input). */
  setAsk(ask: AskText | null) {
    this.ask = ask;
  }

  /** The kept list as a snapshot: a deep copy so history is immutable. */
  private snapshot(): Saved[] {
    return [...this.kept.values()].map(copy);
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
      this.chart.createOverlay(this.overlay(id, d));
    }
  }

  /** Off the chart, as kept: another instrument is coming. */
  hide() {
    this.cancel();
    this.chart.removeOverlay({ groupId: GROUP });
    this.symbol = null;
    this.ruler = null;
    this.kept = new Map();
  }

  /** The toolbar's magnet, lock and hide, onto every drawing kept. */
  setPrefs(next: DrawPrefs) {
    this.prefs = next;
    for (const id of [...this.kept.keys(), ...(this.ruler ? [this.ruler] : [])]) {
      this.chart.overrideOverlay({ id, mode: next.magnet ? 'weak_magnet' : 'normal', lock: next.locked, visible: !next.hidden });
    }
  }

  setErasing(on: boolean) {
    this.erasing = on;
    if (on) this.cancel();
  }

  start(tool: DrawTool, done: () => void) {
    this.cancel();
    this.dropRuler();
    this.erasing = false;
    if (!this.symbol) return done();
    const id = newId();
    this.pending = { id, tool, done };
    this.chart.createOverlay(this.overlay(id, { name: tool, points: [] }));
  }

  cancel() {
    const p = this.pending;
    if (!p) return;
    this.pending = null;
    this.chart.removeOverlay({ id: p.id });
    p.done();
  }

  /** A path ends at the points tapped: the one following the pointer, and a
   * double-click's second tap on the same spot, aren't among them. */
  finish() {
    const p = this.pending;
    if (!p || p.tool !== 'v2Path') return;
    const o = this.chart.getOverlays({ id: p.id })[0];
    const tapped = (o?.points ?? []).slice(0, Math.max(0, (o?.currentStep ?? 1) - 1));
    const at: Saved['points'] = [];
    for (const q of tapped) {
      if (!Number.isFinite(q.timestamp) || !Number.isFinite(q.value)) continue;
      const last = at[at.length - 1];
      if (last && last.t === q.timestamp && last.v === q.value) continue;
      at.push({ t: q.timestamp!, v: q.value! });
    }
    if (at.length < 2) return this.cancel();
    this.pending = null;
    this.chart.overrideOverlay({ id: p.id, points: at.map((q) => ({ timestamp: q.t, value: q.v })), lock: this.prefs.locked });
    this.pushUndo();
    this.kept.set(p.id, { name: 'v2Path', points: at });
    this.save();
    p.done();
  }

  clear() {
    this.cancel();
    this.dropRuler();
    if (this.kept.size > 0) this.pushUndo();
    for (const id of this.kept.keys()) this.chart.removeOverlay({ id });
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

  private dropRuler() {
    if (this.ruler) this.chart.removeOverlay({ id: this.ruler });
    this.ruler = null;
  }

  /** Replace the drawings on the chart with `snapshot` and save. */
  private applySnapshot(snapshot: Saved[]) {
    for (const id of this.kept.keys()) this.chart.removeOverlay({ id });
    this.kept = new Map();
    for (const d of snapshot) {
      const id = newId();
      this.kept.set(id, copy(d));
      this.chart.createOverlay(this.overlay(id, d));
    }
    this.save();
  }

  /** Off the chart and out of the kept list, as an undoable change. */
  private remove(id: string) {
    this.pushUndo();
    this.kept.delete(id);
    this.chart.removeOverlay({ id });
    this.save();
  }

  /**
   * Ask for the words of the text or note `id`, drawn as `base`. A new one is
   * kept, as one undoable change, only once it has words; given none, it goes.
   * An old one emptied is left as it was.
   */
  private askWords(id: string, base: Saved, fresh: boolean) {
    const p = base.points[0];
    const ask = this.ask;
    if (!p || !ask) {
      if (fresh) this.chart.removeOverlay({ id });
      return;
    }
    const xy = this.chart.convertToPixel({ timestamp: p.t, value: p.v }, { paneId: 'candle_pane', absolute: true }) as { x?: number; y?: number };
    ask({ x: xy.x ?? 0, y: xy.y ?? 0 }, base.text ?? '', (text) => {
      const words = text?.trim() ?? '';
      // Gone meanwhile: another instrument, an undo, Clear.
      if (this.chart.getOverlays({ id }).length === 0) return;
      if (words === '' || words === base.text) {
        if (fresh) this.chart.removeOverlay({ id });
        return;
      }
      // Where it is now: it may have been dragged while being written.
      const now = fresh ? base : (this.kept.get(id) ?? base);
      this.pushUndo();
      this.kept.set(id, { ...now, text: words });
      this.chart.overrideOverlay({ id, extendData: words });
      this.save();
    });
  }

  private overlay(id: string, d: Saved): OverlayCreate {
    const name = d.name;
    const pointsOf = (e: OverlayEvent<unknown>) => e.overlay.points.flatMap((p) => (Number.isFinite(p.timestamp) && Number.isFinite(p.value) ? [{ t: p.timestamp!, v: p.value! }] : []));
    const keep = (e: OverlayEvent<unknown>) => {
      const at = pointsOf(e);
      const prev = this.kept.get(id);
      // The ruler, and a text or note still waiting for its words, aren't kept.
      if (at.length === 0 || name === RULER || (WORDED.has(name) && !prev)) return;
      if (prev && prev.points.length === at.length && prev.points.every((p, i) => p.t === at[i]?.t && p.v === at[i]?.v)) {
        return;
      }
      this.pushUndo();
      this.kept.set(id, { name, points: at, ...(prev?.text !== undefined ? { text: prev.text } : {}) });
      this.save();
    };
    const finished = d.points.length > 0;
    return {
      id,
      name,
      groupId: GROUP,
      // On the candles only: a point is a price.
      paneId: 'candle_pane',
      styles: name === 'brush' ? BRUSH_STYLES : STYLES,
      points: d.points.map((p) => ({ timestamp: p.t, value: p.v })),
      extendData: d.text ?? (name === 'simpleAnnotation' ? 'Note' : undefined),
      mode: this.prefs.magnet ? 'weak_magnet' : 'normal',
      // One being drawn takes taps whatever the switches say.
      lock: finished && this.prefs.locked,
      visible: !finished || !this.prefs.hidden,
      onDrawEnd: (e) => {
        if (this.pending?.id !== id) return;
        const { done } = this.pending;
        this.pending = null;
        if (name === RULER) {
          this.ruler = id;
        } else if (WORDED.has(name)) {
          this.askWords(id, { name, points: pointsOf(e) }, true);
        } else {
          keep(e);
        }
        if (name !== RULER && this.prefs.locked) this.chart.overrideOverlay({ id, lock: true });
        done();
      },
      onPressedMoveEnd: (e) => keep(e),
      onClick: () => {
        if (this.erasing && this.pending == null) {
          if (id === this.ruler) this.dropRuler();
          else if (this.kept.has(id)) this.remove(id);
        }
      },
      onDoubleClick: () => {
        const kept = this.kept.get(id);
        if (kept && WORDED.has(name) && this.pending == null && !this.erasing) this.askWords(id, kept, false);
      },
      // KLineChart removes it once this returns.
      onRightClick: () => {
        if (this.pending?.id === id) {
          const { done } = this.pending;
          this.pending = null;
          return done();
        }
        if (id === this.ruler) {
          this.ruler = null;
          return;
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
