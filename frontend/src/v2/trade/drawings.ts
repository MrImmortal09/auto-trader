// Drawing tools on the KLineChart charts: KLineChart's own overlays, kept per
// instrument in the browser as time and price points, so a drawing shows on
// every timeframe. Stop and target lines are never among them: those always
// come from the engine.

import { useCallback, useRef, useState } from 'react';
import type { Chart, DeepPartial, OverlayCreate, OverlayEvent, OverlayStyle } from 'klinecharts';
import { readStored, writeStored } from '../../lib/execute';
import { todayIst } from '../lib/format';
import { alpha } from '../lib/palette';
import { C, FONT } from './chartSetup';

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

// The user's own marks, in the accent: never a stop's red, a target's green or
// the VWAP's indicator blue. The handles are accent dots in a faint accent
// halo, and a label (Fibonacci's levels) is white on accent, like the chart's
// other tags.
const HALO = alpha(C.accent, 0.35);
const STYLES: DeepPartial<OverlayStyle> = {
  line: { color: C.accent, size: 1 },
  point: { color: C.accent, borderColor: HALO, activeColor: C.accent, activeBorderColor: HALO },
  text: { color: C.onAccent, backgroundColor: C.accent, borderColor: C.accent, family: FONT, size: 11 },
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

  constructor(chart: Chart) {
    this.chart = chart;
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
    this.chart.removeOverlay({ groupId: GROUP });
    this.kept.clear();
    this.save();
  }

  private overlay(id: string, name: DrawTool, points: Saved['points']): OverlayCreate {
    const keep = (e: OverlayEvent<unknown>) => {
      const at = e.overlay.points.flatMap((p) => (Number.isFinite(p.timestamp) && Number.isFinite(p.value) ? [{ t: p.timestamp!, v: p.value! }] : []));
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
