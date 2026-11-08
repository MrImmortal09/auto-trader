import { useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react';
import type { InputValue, SerializedDrawing, TimelineMark, Vela } from '@luxalgo/vela';
import { VelaWorkspace } from '@luxalgo/vela/workspace';
import { apiUrl } from '../../lib/api';
import { readStored } from '../../lib/execute';
import { streamToken } from '../lib/api';
import { fetchBars, fetchDays, upsert } from './bars';
import { editable } from './chartKeymap';
import { C, FONT, PIVOT_ORANGE, type MarkerData } from './chartSetup';
import { keepList, pruneExpired, VELA_DRAWINGS, type DrawApi } from './drawings';
import type { ChartLine, ChartProps } from './KChart';
import { barAt, futureName, symbolName, TF_MS, vwapDays, vwapOf, vwapOnIndex, type Bar, type CandleStyle, type Tf } from './model';
import { pivotLines, pivotsByDay } from './pivots';
import { DrawHistory } from './undoHistory';
import { CandleProvider, PROVIDER, velaSymbol, type Feed } from './velaProvider';

/** The design tokens on Vela's theme: white, the subtle ink for the axes,
 * the candle pair, as the KLineChart page draws; its menus and dialogs take them too. */
const THEME = { background: C.canvas, textColor: C.subtle, gridColor: C.grid, borderColor: C.line, upColor: C.up, downColor: C.down, fontFamily: FONT };
/** The pivot orange, as on the KLineChart page; the names are in the muted ink. */
const PIVOT = PIVOT_ORANGE;
/** A drag reports every move; the level is sent once the line has rested this long. */
const MOVE_SETTLE_MS = 300;
const MARK_COLOR: Record<MarkerData['kind'], string> = { B: C.profit, E: C.fg, T: C.profit, S: C.loss, call: C.indicator };
/** Vela's price pane: where every order line belongs. */
const PRICE_PANE = 'price';
/** RSI and CCI as Vela names them: the Layers menu owns them. */
const NATIVE = { rsi: 'rsi', cci: 'commodity-channel-index' } as const;
/** Studies added from Vela's Indicators menu, kept per chart slot. */
const STUDIES = 'v2.studies.v.';
/** The candle styles the Layers menu knows. */
const OUR_STYLES = new Set<string>(['candles', 'bars', 'area']);

type Text = NonNullable<SerializedDrawing['text']>;
/** A user's drawing as kept: all but its id. */
type KeptDrawing = Omit<SerializedDrawing, 'id' | 'createdAt'>;
interface KeptStudy {
  type: string;
  inputs: Record<string, InputValue>;
}

/** Our own changes to the chart's drawings: the edits they echo are not the user's. */
function quietly<T>(applying: { current: boolean }, change: () => T): T {
  applying.current = true;
  try {
    return change();
  } finally {
    applying.current = false;
  }
}

/** A drawing as it stands: anything that moves, restyles, unlocks, hides or
 * re-panes it after the sync put it there shows up as a different look. */
function lookOf(d: SerializedDrawing | undefined): string {
  return d ? JSON.stringify([d.anchors, d.style, d.text ?? null, d.locked, d.visible, d.paneId, d.showOn ?? null]) : '';
}

/** Vela names a timeframe `5` or `5m`. */
const sameTf = (t: string, tf: Tf) => t === tf || t === tf.slice(0, -1);
/** A Vela symbol, with or without its provider prefix, is `symbol`. */
const sameSymbol = (s: string, symbol: string) => s === velaSymbol(symbol) || `${PROVIDER}:${s}` === velaSymbol(symbol);

function cloneKept(drawings: KeptDrawing[]): KeptDrawing[] {
  return drawings.map((d) => ({
    ...d,
    anchors: d.anchors.map((a) => ({ ...a })),
    style: { ...d.style },
    text: d.text ? { ...d.text } : undefined,
    props: d.props ? { ...d.props } : undefined,
    showOn: d.showOn ? [...d.showOn] : undefined,
  }));
}

/** Our drawings (order lines, VWAP, pivots), as opposed to the user's. */
function ourIds(owned: Map<string, string>, extras: { vwap: Map<number, string>; pivots: Map<string, string> }): Set<string> {
  return new Set([...owned.values(), ...extras.vwap.values(), ...extras.pivots.values()]);
}

function isKeptDrawing(d: unknown): d is KeptDrawing {
  const k = d as Partial<KeptDrawing> | null;
  return typeof k?.type === 'string' && Array.isArray(k.anchors);
}

function isKeptStudy(s: unknown): s is KeptStudy {
  const k = s as Partial<KeptStudy> | null;
  return typeof k?.type === 'string' && typeof k.inputs === 'object' && k.inputs !== null;
}

function loadList<T>(key: string, keep: (x: unknown) => x is T): T[] {
  const raw = readStored<unknown>(key, []);
  return Array.isArray(raw) ? raw.filter(keep) : [];
}

/** A level's words. Vela centres a horizontal line's label on its anchor,
 * which is kept a fifth of the way into the view, clear of both axes. The
 * pills' buttons (×, + Stop) are not words: those live in the positions panel. */
function lineText(value: string, color: string): Text {
  return { value, color, size: 'small', hAlign: 'center', vAlign: 'top', side: 'above' };
}
const WORD_PARTS = new Set(['label', 'pnl']);

/** A pivot's name, on its line. */
function pivotText(value: string): Text {
  return { value, color: C.muted, size: 'tiny', hAlign: 'center', vAlign: 'top', side: 'above' };
}

/** Where a line's anchor (its label and handle) sits: a fifth of the way
 * across the visible bars, so the words never run under either axis. */
function anchorTime(c: Vela, fallback: number): number {
  const r = c.getVisibleRange();
  return r ? r.from + (r.to - r.from) * 0.2 : fallback;
}

interface Keyed<T> {
  key: string;
  value: T;
}

const VELA_STYLE: Record<CandleStyle, string> = { candles: 'candles', bars: 'bars', area: 'area' };

/**
 * One chart in Vela's own app (its workspace, one chart): `sym` at `tf`, the
 * same order lines, markers, VWAP and pivots as the KLineChart page, drawn
 * with Vela's tools — a level is a horizontal-line drawing dragged with
 * Vela's handles, markers sit on its timeline lane, and "+ Stop" places on
 * the next click — beside Vela's drawing toolbar, indicators, style menu,
 * object tree and screenshot. Bars come from the candle service through a
 * Vela data provider and follow the same stream.
 * Only a drag of a level's own line moves the level; whatever else Vela lets
 * the user do to a line is put back. Only our header picks the instrument and
 * the timeframe, and no key reaches Vela outside its text fields.
 */
export default function VChart({ chartId, serverBase, sym, tf, lines, markers, vwap, pivots, rsi, cci, style, placing, onPlacingDone, bus, onState, onStyle, onStudies, drawRef }: ChartProps & {
  /** A candle style picked in Vela's own menu. */
  onStyle?: (s: CandleStyle) => void;
  /** RSI or CCI added or closed with Vela's own menus. */
  onStudies?: (s: { rsi: boolean; cci: boolean }) => void;
  drawRef?: RefObject<DrawApi | null>;
}) {
  const host = useRef<HTMLDivElement>(null);
  const chart = useRef<Vela | null>(null);
  const history = useRef(new DrawHistory<KeptDrawing[]>());
  const lastSaved = useRef(new Map<string, KeptDrawing[]>());
  const apiRef = useRef<DrawApi | null>(null);
  const provider = useRef<CandleProvider | null>(null);
  const feed = useRef<Feed>({ onBar: null });
  const symbol = symbolName(sym);
  const future = sym.kind === 'index' ? futureName(sym.index) : null;
  const key = `${serverBase}|${symbol}|${tf}`;
  // Bars, the future's bars, the load state and an error, each for the market they belong to.
  const [barsAt, setBarsAt] = useState<Keyed<Bar[]> | null>(null);
  const [futureAt, setFutureAt] = useState<Keyed<Bar[]> | null>(null);
  const [painted, setPainted] = useState<Keyed<boolean> | null>(null);
  const [errorAt, setErrorAt] = useState<Keyed<string> | null>(null);
  const bars = barsAt?.key === key ? barsAt.value : null;
  const futureBars = useMemo(() => (futureAt?.key === key ? futureAt.value : []), [futureAt, key]);
  const error = errorAt?.key === key ? errorAt.value : null;
  const loaded = bars != null && bars.length > 0 && painted?.key === key && painted.value;
  // What the chart should be showing; a slow answer for anything else is dropped.
  const want = useRef({ base: serverBase, symbol, tf, key });
  // Our lines' drawings: line id → Vela's drawing id; handlers and what was applied, by Vela id.
  const owned = useRef(new Map<string, string>());
  const handlers = useRef(new Map<string, ChartLine>());
  const applied = useRef(new Map<string, { price: number; text: string; time: number; style: string; look: string }>());
  const appliedPivots = useRef(new Map<string, { price: number; text: string; time: number }>());
  // Each VWAP day's line as last drawn, by drawing id.
  const appliedVwap = useRef(new Map<string, string>());
  // A level moves only by a drag of its own line on the chart, and Vela commits
  // a drag inside the pointer's release on its canvas. Every other edit of a
  // line (its settings panel, unlocking it, dragging a selection of several,
  // keys, undo) is put back, and no key may ever move a stop.
  const pressed = useRef(new Set<number>());
  // The drawings edited while a pointer pressed on the canvas is released there.
  const release = useRef<Set<string> | null>(null);
  // The release each line's latest edit came in, if it came in one.
  const releaseOf = useRef(new Map<string, Set<string>>());
  const applying = useRef(false);
  // Our own adds and removals of indicators: the events they raise are not the user's.
  const studying = useRef(false);
  // The instrument the user's drawings on the chart belong to, once put there.
  const userFor = useRef<string | null>(null);
  // Writes a pending save of the user's drawings at once.
  const flushDrawings = useRef<() => void>(() => {});
  // Bumped (once a frame) as the view pans or zooms, so anchors follow it.
  const [view, setView] = useState(0);
  const [pivotLevels, setPivotLevels] = useState<Keyed<{ name: string; value: number }[]> | null>(null);
  // A drag: the line's own edits settle before the level is sent, and the screen's price waits.
  const settling = useRef(new Map<string, number>());
  // Our locked drawings: a VWAP line per IST day, and the pivot levels by name.
  const extras = useRef<{ vwap: Map<number, string>; pivots: Map<string, string> }>({ vwap: new Map(), pivots: new Map() });
  // Bumped when one of our lines is erased or edited off what the sync put
  // there, so the next sync puts it back.
  const [drift, setDrift] = useState(0);
  const placingNow = useRef(placing);
  const doneNow = useRef(onPlacingDone);
  const styleNow = useRef(style);
  const styleTold = useRef(onStyle);
  const studiesTold = useRef(onStudies);
  const first = useRef(true);

  useEffect(() => {
    want.current = { base: serverBase, symbol, tf, key };
  }, [serverBase, symbol, tf, key]);

  // The chart itself, for the component's lifetime.
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    const w = want.current;
    const p = new CandleProvider(
      () => want.current.base,
      feed.current,
      (loadedSymbol, loadedTf, loadedBars) => {
        const now = want.current;
        if (loadedSymbol === now.symbol && loadedTf === now.tf) setBarsAt({ key: now.key, value: loadedBars });
      },
      (failedSymbol, message) => {
        const now = want.current;
        if (failedSymbol === now.symbol) setErrorAt({ key: now.key, value: message });
      },
    );
    provider.current = p;
    const ws = new VelaWorkspace(el, {
      // One chart: no layout picker or links between charts.
      layout: false,
      symbol: velaSymbol(w.symbol),
      timeframe: w.tf,
      bars: 1500,
      live: true,
      theme: THEME,
      volume: false,
      currentPriceLine: true,
      animations: { liveBar: false },
      providers: { [PROVIDER]: () => p },
      // The axis and readouts in IST, whatever the browser's zone.
      timezone: 'Asia/Kolkata',
      timeframes: ['1m', '5m', '15m'],
      // What the user adds is kept by us, per instrument; Vela keeps nothing.
      persist: false,
      // Its range chips switch to hourly, daily or weekly bars, which the candle service doesn't serve.
      bottombar: false,
      watermark: false,
      // "auto" swaps in Vela's phone bar, whose instrument and timeframe buttons ignore the topbar's.
      layoutMode: 'desktop',
      // Our header owns the instrument and the timeframe. No undo (it would put
      // back a level the engine moved), no alerts, no layouts.
      topbar: { left: ['style', 'indicators'], right: ['panels', 'screenshot'] },
      // Bar depth and the trading session reload the chart.
      settings: { hidden: ['advanced', 'trading-session'] },
    });
    const c = ws.chart;
    const cell = ws.active;
    // The status line keeps its prices: our header names the instrument, which
    // Vela would show as its encoded ticker.
    const prefs = cell.statusPrefs();
    if (prefs.parts) cell.applyStatusPrefs({ ...prefs, parts: { ...prefs.parts, logo: false, name: false, market: false } });
    // None of Vela's shortcuts: undo, redo, the line tools' keys.
    ws.keymap.detach();
    // Pointer only: Vela's arrow keys would nudge a selected stop or target,
    // and a nudge is sent like a drag. This drops its key listener and tabindex.
    c.renderer.set('keyboard', false);
    // Keys reach Vela only in its text fields: typed anywhere else they open
    // its instrument search or timeframe box.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' && e.key !== 'Tab' && !editable(e.target)) e.stopPropagation();
    };
    el.addEventListener('keydown', onKey, true);
    // The chart's cell holds the canvases that take the pointer; Vela's menus,
    // panels and dialogs are outside it.
    const onCanvas = (t: EventTarget | null) => t instanceof HTMLCanvasElement && cell.host.contains(t);
    // The chart shows what our header picked (the market guard below puts anything else back).
    const onOurs = (s: string, t: string) => sameSymbol(s, want.current.symbol) && sameTf(t, want.current.tf);
    const onDown = (e: PointerEvent) => {
      if (onCanvas(e.target)) pressed.current.add(e.pointerId);
    };
    const onRelease = (e: PointerEvent) => {
      if (!pressed.current.delete(e.pointerId) || !onCanvas(e.target)) return;
      // A line dropped over another instrument's bars is no drag of ours.
      if (!onOurs(cell.symbol, cell.timeframe)) return;
      const edits = new Set<string>();
      release.current = edits;
      // Over once the release is handled (below), or at the latest on the next task.
      window.setTimeout(() => {
        if (release.current === edits) release.current = null;
      }, 0);
    };
    const onReleased = () => {
      release.current = null;
    };
    const onBlur = () => {
      pressed.current.clear();
      release.current = null;
    };
    window.addEventListener('pointerdown', onDown, true);
    window.addEventListener('pointerup', onRelease, true);
    window.addEventListener('pointercancel', onRelease, true);
    window.addEventListener('pointerup', onReleased);
    window.addEventListener('pointercancel', onReleased);
    window.addEventListener('blur', onBlur);
    c.marks.defineGroup({ id: 'fills', label: 'Trades' }).defineGroup({ id: 'calls', label: 'Telegram calls' });
    chart.current = c;
    const settle = settling.current;
    const ownedMap = owned.current;
    const handlerMap = handlers.current;
    const appliedMap = applied.current;
    const pivotMap = appliedPivots.current;
    const releases = releaseOf.current;
    const down = pressed.current;
    let frame = 0;
    // A line's edits have rested. One line dragged on its own, still
    // draggable, on our instrument, sends where it was dropped; anything else
    // is put back.
    const settleEdit = (id: string) => {
      // A drag under way decides on its release.
      if (pressed.current.size > 0) {
        settle.set(id, window.setTimeout(() => settleEdit(id), MOVE_SETTLE_MS));
        return;
      }
      settle.delete(id);
      const edits = releases.get(id);
      releases.delete(id);
      const line = handlers.current.get(id);
      const price = c.drawings.all().find((d) => d.id === id)?.anchors[0]?.price;
      const was = applied.current.get(id)?.price;
      const dragged = edits?.size === 1 && line?.data.draggable === true && onOurs(cell.symbol, cell.timeframe);
      if (dragged && line.onMove && price != null && was != null && Math.abs(price - was) >= 1e-9) {
        applied.current.delete(id);
        line.onMove(price);
        return;
      }
      setDrift((n) => n + 1);
    };

    // The user's own drawings, kept per instrument: never our lines, VWAP or
    // pivots, and only on the price pane (a study pane's id doesn't last).
    const currentKept = (): KeptDrawing[] => {
      const mine = ourIds(owned.current, extras.current);
      const raw = c.drawings
        .all()
        .filter((d) => !mine.has(d.id) && d.paneId === PRICE_PANE)
        .map((d) => ({
          type: d.type,
          paneId: d.paneId,
          anchors: d.anchors,
          style: d.style,
          text: d.text,
          locked: d.locked,
          visible: d.visible,
          zIndex: d.zIndex,
          props: d.props,
          showOn: d.showOn,
        }));
      return cloneKept(raw);
    };

    const applySnapshot = (snapshot: KeptDrawing[]) => {
      const symbol = want.current.symbol;
      const mine = ourIds(owned.current, extras.current);
      quietly(applying, () => {
        for (const d of c.drawings.all()) if (!mine.has(d.id)) c.drawings.remove(d.id);
        for (const k of snapshot) {
          const d = c.drawings.add(k.type, {
            paneId: PRICE_PANE,
            anchors: k.anchors,
            style: k.style,
            text: k.text,
            props: k.props,
            zIndex: k.zIndex,
          });
          if (!d) continue;
          if (k.locked) c.drawings.lock(d.id, true);
          if (k.visible === false) c.drawings.show(d.id, false);
          if (k.showOn?.length) c.drawings.update(d.id, { showOn: k.showOn });
        }
      });
      const cloned = cloneKept(snapshot);
      lastSaved.current.set(symbol, cloned);
      keepList(VELA_DRAWINGS + symbol, cloned);
      pruneExpired(VELA_DRAWINGS);
    };

    let saveTimer = 0;
    const saveDrawings = () => {
      saveTimer = 0;
      const symbol = want.current.symbol;
      if (userFor.current !== symbol) return;
      const kept = currentKept();
      const prev = lastSaved.current.get(symbol) ?? [];
      if (JSON.stringify(prev) !== JSON.stringify(kept)) {
        history.current.push(symbol, prev);
        lastSaved.current.set(symbol, kept);
      }
      keepList(VELA_DRAWINGS + symbol, kept);
      pruneExpired(VELA_DRAWINGS);
    };
    const drawingsChanged = () => {
      window.clearTimeout(saveTimer);
      saveTimer = window.setTimeout(saveDrawings, MOVE_SETTLE_MS);
    };
    flushDrawings.current = () => {
      if (!saveTimer) return;
      window.clearTimeout(saveTimer);
      saveDrawings();
    };
    // The instrument's kept drawings, in place of any other's.
    const restoreDrawings = () => {
      const symbol = want.current.symbol;
      if (userFor.current === symbol) return;
      const mine = ourIds(owned.current, extras.current);
      const loaded = loadList(VELA_DRAWINGS + symbol, isKeptDrawing);
      quietly(applying, () => {
        for (const d of c.drawings.all()) if (!mine.has(d.id)) c.drawings.remove(d.id);
        for (const k of loaded) {
          const d = c.drawings.add(k.type, { paneId: PRICE_PANE, anchors: k.anchors, style: k.style, text: k.text, props: k.props, zIndex: k.zIndex });
          if (!d) continue;
          if (k.locked) c.drawings.lock(d.id, true);
          if (k.visible === false) c.drawings.show(d.id, false);
          if (k.showOn?.length) c.drawings.update(d.id, { showOn: k.showOn });
        }
      });
      userFor.current = symbol;
      lastSaved.current.set(symbol, cloneKept(loaded));
    };

    const undo = (): boolean => {
      flushDrawings.current();
      const symbol = want.current.symbol;
      if (userFor.current !== symbol) return false;
      const cur = currentKept();
      const prev = history.current.undo(symbol, cur);
      if (!prev) return false;
      applySnapshot(prev);
      return true;
    };

    const redo = (): boolean => {
      flushDrawings.current();
      const symbol = want.current.symbol;
      if (userFor.current !== symbol) return false;
      const cur = currentKept();
      const next = history.current.redo(symbol, cur);
      if (!next) return false;
      applySnapshot(next);
      return true;
    };

    const canUndo = (): boolean => {
      const symbol = want.current.symbol;
      return saveTimer !== 0 || history.current.canUndo(symbol);
    };

    const canRedo = (): boolean => {
      const symbol = want.current.symbol;
      return saveTimer === 0 && history.current.canRedo(symbol);
    };

    apiRef.current = {
      start: () => {},
      cancel: () => {},
      clear: () => {
        flushDrawings.current();
        const symbol = want.current.symbol;
        const cur = currentKept();
        if (cur.length === 0) return;
        history.current.push(symbol, cur);
        applySnapshot([]);
      },
      undo,
      redo,
      canUndo,
      canRedo,
    };

    // Studies added from Vela's menu, kept for this chart; RSI and CCI are the Layers menu's.
    const saveStudies = () =>
      keepList(
        STUDIES + chartId,
        c.indicators().flatMap((h) => (h.nativeType && h.nativeType !== NATIVE.rsi && h.nativeType !== NATIVE.cci ? [{ type: h.nativeType, inputs: h.inputValues() }] : [])),
      );
    const studiesChanged = () => {
      if (studying.current) return;
      const present = c.presentNativeIndicators();
      studiesTold.current?.({ rsi: present.includes(NATIVE.rsi), cci: present.includes(NATIVE.cci) });
      saveStudies();
    };
    studying.current = true;
    try {
      for (const s of loadList(STUDIES + chartId, isKeptStudy)) c.addNativeIndicator(s.type, { inputs: s.inputs });
    } finally {
      studying.current = false;
    }

    const offs = [
      c.on('load:end', ({ symbol: s, timeframe: t, bars: n }) => {
        // Another market, on its way back (below): our lines wait for ours.
        if (!onOurs(s, t)) return;
        setPainted({ key: want.current.key, value: n > 0 });
        if (n > 0) restoreDrawings();
      }),
      // Only our header picks what the chart shows: any other switch goes straight back.
      c.on('market:changed', ({ symbol: s, timeframe: t }) => {
        if (!onOurs(s, t)) void c.setMarket({ symbol: velaSymbol(want.current.symbol), timeframe: want.current.tf });
      }),
      c.on('priceStyle:change', ({ from, to }) => {
        // Heikin Ashi draws made-up candles under real-price stop lines: never kept.
        if (to === 'heikinashi') {
          window.setTimeout(() => c.renderer.set('priceStyle', from === 'heikinashi' ? 'candles' : from), 0);
          return;
        }
        // Picked in Vela's menu: the Layers menu follows.
        if (OUR_STYLES.has(to) && to !== VELA_STYLE[styleNow.current]) styleTold.current?.(to as CandleStyle);
      }),
      c.on('indicator:added', studiesChanged),
      c.on('indicator:removed', studiesChanged),
      c.on('indicator:inputs', () => {
        if (!studying.current) saveStudies();
      }),
      c.on('viewport:changed', () => {
        if (frame) return;
        frame = requestAnimationFrame(() => {
          frame = 0;
          setView((n) => n + 1);
        });
      }),
      // A line edited: decided once its edits rest (see `settleEdit`).
      c.on('drawing:edited', ({ id }) => {
        if (applying.current) return;
        release.current?.add(id);
        if (!handlers.current.has(id)) {
          if (!ourIds(owned.current, extras.current).has(id)) drawingsChanged();
          return;
        }
        if (release.current) releases.set(id, release.current);
        else releases.delete(id);
        const prev = settle.get(id);
        if (prev != null) window.clearTimeout(prev);
        settle.set(id, window.setTimeout(() => settleEdit(id), MOVE_SETTLE_MS));
      }),
      // A copy of one of our lines (Duplicate, a Ctrl-drag) would pass for a
      // real stop or target: removed at once. Anything else is the user's.
      c.on('drawing:created', ({ id }) => {
        if (applying.current) return;
        const text = c.drawings.all().find((d) => d.id === id)?.text?.value;
        if (text && [...applied.current.values()].some((a) => a.text === text)) {
          window.setTimeout(() => quietly(applying, () => c.drawings.remove(id)), 0);
          return;
        }
        drawingsChanged();
      }),
      // Erased by the user: never a removed level — it comes back on the next sync.
      c.on('drawing:removed', ({ id }) => {
        let ours = false;
        for (const [lineId, vid] of owned.current) {
          if (vid !== id) continue;
          owned.current.delete(lineId);
          ours = true;
        }
        let extra = false;
        handlers.current.delete(id);
        applied.current.delete(id);
        releases.delete(id);
        const pending = settle.get(id);
        if (pending != null) window.clearTimeout(pending);
        settle.delete(id);
        for (const [day, vid] of extras.current.vwap) {
          if (vid !== id) continue;
          extras.current.vwap.delete(day);
          extra = true;
        }
        appliedVwap.current.delete(id);
        for (const [name, vid] of extras.current.pivots) {
          if (vid !== id) continue;
          extras.current.pivots.delete(name);
          extra = true;
        }
        if (ours) setDrift((n) => n + 1);
        else if (!extra && !applying.current) drawingsChanged();
      }),
      // "+ Stop": the next click on the price pane places it. An armed drawing
      // tool, the ruler or the eraser takes its own clicks.
      c.renderer.onClick((e) => {
        const now = placingNow.current;
        if (!now || e.price == null || e.paneKind === 'study') return;
        if (c.drawings.getTool() != null || c.drawings.getMode() != null) return;
        if (now.onPlace(e.price)) doneNow.current();
      }),
      c.renderer.onCrosshairMove((e) => bus.publish(chartId, e.time)),
      bus.subscribe(chartId, (t) => c.renderer.setExternalCrosshair(t)),
    ];
    return () => {
      offs.forEach((off) => off());
      flushDrawings.current();
      flushDrawings.current = () => {};
      apiRef.current = null;
      el.removeEventListener('keydown', onKey, true);
      window.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('pointerup', onRelease, true);
      window.removeEventListener('pointercancel', onRelease, true);
      window.removeEventListener('pointerup', onReleased);
      window.removeEventListener('pointercancel', onReleased);
      window.removeEventListener('blur', onBlur);
      if (frame) cancelAnimationFrame(frame);
      for (const t of settle.values()) window.clearTimeout(t);
      settle.clear();
      ws.destroy();
      chart.current = null;
      provider.current = null;
      ownedMap.clear();
      handlerMap.clear();
      appliedMap.clear();
      pivotMap.clear();
      appliedVwap.current = new Map();
      releases.clear();
      down.clear();
      release.current = null;
      userFor.current = null;
      extras.current = { vwap: new Map(), pivots: new Map() };
    };
  }, [bus, chartId]);

  useEffect(() => {
    if (!drawRef) return;
    drawRef.current = {
      start: () => {},
      cancel: () => {},
      clear: () => apiRef.current?.clear(),
      undo: () => apiRef.current?.undo() ?? false,
      redo: () => apiRef.current?.redo() ?? false,
      canUndo: () => apiRef.current?.canUndo() ?? false,
      canRedo: () => apiRef.current?.canRedo() ?? false,
    };
    return () => {
      drawRef.current = null;
    };
  }, [drawRef]);

  // Another symbol or timeframe: switched in place, from nothing. The user's
  // drawings are kept per instrument: another's come in once its bars are painted.
  useEffect(() => {
    const c = chart.current;
    if (!c) return;
    if (first.current) {
      first.current = false;
      return;
    }
    provider.current?.forget();
    if (userFor.current !== symbol) {
      flushDrawings.current();
      const mine = ourIds(owned.current, extras.current);
      quietly(applying, () => {
        for (const d of c.drawings.all()) if (!mine.has(d.id)) c.drawings.remove(d.id);
      });
      userFor.current = null;
    }
    void c.setMarket({ symbol: velaSymbol(symbol), timeframe: tf });
  }, [symbol, tf]);

  // The stream keeps the forming bar and the future's bars live.
  useEffect(() => {
    let live = true;
    if (future) {
      fetchBars(serverBase, future, tf)
        .then((fut) => live && setFutureAt({ key, value: fut }))
        .catch(() => live && setFutureAt({ key, value: [] }));
    }
    const token = streamToken();
    const params = new URLSearchParams({ symbols: [symbol, future].filter(Boolean).join(','), tf });
    if (token) params.set('token', token);
    const es = new EventSource(`${apiUrl(serverBase, '/api/v2/candles/stream')}?${params}`);
    es.addEventListener('bars', (e) => {
      let body: { updates: { symbol: string; bar: Bar }[] };
      try {
        body = JSON.parse((e as MessageEvent<string>).data);
      } catch {
        return;
      }
      for (const u of body.updates) {
        if (u.symbol === symbol) {
          setBarsAt((prev) => (prev?.key === key ? { key, value: upsert(prev.value, u.bar) } : prev));
          provider.current?.push(symbol, tf, u.bar);
        } else if (u.symbol === future) {
          setFutureAt((prev) => ({ key, value: upsert(prev?.key === key ? prev.value : [], u.bar) }));
        }
      }
    });
    return () => {
      live = false;
      es.close();
    };
  }, [serverBase, symbol, future, tf, key]);

  useEffect(() => {
    onState?.({ loading: !loaded && !error, error, last: bars?.[bars.length - 1] ?? null });
  }, [loaded, error, bars, onState]);

  useEffect(() => {
    placingNow.current = placing;
    doneNow.current = onPlacingDone;
    styleTold.current = onStyle;
    studiesTold.current = onStudies;
  });

  // "+ Stop" takes the next click: no drawing tool is left armed to take it too.
  const placingKey = placing?.key ?? null;
  useEffect(() => {
    if (placingKey) chart.current?.drawings.setTool(null).setMode(null);
  }, [placingKey]);

  useEffect(() => {
    styleNow.current = style;
    chart.current?.renderer.set('priceStyle', VELA_STYLE[style]);
  }, [style]);

  // RSI and CCI: Vela's own, in step with the Layers menu (Vela's menus report back, above).
  useEffect(() => {
    const c = chart.current;
    if (!c) return;
    studying.current = true;
    try {
      for (const [type, on] of [
        [NATIVE.rsi, rsi],
        [NATIVE.cci, cci],
      ] as const) {
        const shown = c.indicators().filter((h) => h.nativeType === type);
        if (on && shown.length === 0) c.addNativeIndicator(type);
        if (!on) for (const h of shown) h.remove();
      }
    } finally {
      studying.current = false;
    }
  }, [rsi, cci]);

  // Order lines: one horizontal-line drawing each, dragged with Vela's own
  // handle. Each is checked against the drawing as it stands, not against what
  // was last sent, so an edit that was put back, a line unlocked, hidden,
  // restyled or moved to another pane is set right; and a line is only touched
  // when something changed, as every change costs Vela an undo copy.
  useEffect(() => {
    const c = chart.current;
    if (!c || !loaded || !bars) return;
    const at = anchorTime(c, bars[0].t);
    const tfMs = TF_MS[tf];
    const wanted = new Set<string>();
    const now = new Map(c.drawings.all().map((d) => [d.id, d]));
    const touched = new Map<string, { price: number; text: string; time: number; style: string }>();
    quietly(applying, () => {
      for (const line of lines) {
        const price = typeof line.price === 'number' ? line.price : barAt(bars, line.price.at, tfMs)?.c;
        if (price == null || !Number.isFinite(price)) continue;
        wanted.add(line.id);
        const text = line.data
          .parts(price)
          .filter((part) => WORD_PARTS.has(part.key))
          .map((part) => part.text)
          .join(' · ');
        const style = { lineColor: line.data.color, lineWidth: 1, lineStyle: line.data.dashed ? ('dashed' as const) : ('solid' as const) };
        const styleKey = `${style.lineColor}|${style.lineStyle}`;
        const locked = !line.data.draggable;
        let vid = owned.current.get(line.id);
        const d = vid ? now.get(vid) : undefined;
        if (vid && !d) vid = undefined;
        // On another pane, or shown on some timeframes only: drawn afresh.
        if (vid && d && (d.paneId !== PRICE_PANE || (d.showOn?.length ?? 0) > 0)) {
          c.drawings.remove(vid);
          handlers.current.delete(vid);
          applied.current.delete(vid);
          vid = undefined;
        }
        if (!vid || !d) {
          const added = c.drawings.add('hline', { anchors: [{ time: at, price }], style, text: lineText(text, line.data.color) });
          if (!added) continue;
          vid = added.id;
          owned.current.set(line.id, vid);
          if (locked) c.drawings.lock(vid, true);
          // Adding makes its look the default for new horizontal lines: a line
          // the user draws must never pass for a stop.
          c.drawings.resetToolDefaults('hline');
          touched.set(vid, { price, text, time: at, style: styleKey });
        } else if (!settling.current.has(vid)) {
          const prev = applied.current.get(vid);
          const drifted = !prev || lookOf(d) !== prev.look;
          const patch: Partial<SerializedDrawing> = {};
          if (drifted || prev.price !== price || prev.time !== at) patch.anchors = [{ time: at, price }];
          if (drifted || prev.text !== text) patch.text = lineText(text, line.data.color);
          if (drifted || prev.style !== styleKey) patch.style = style;
          if (Object.keys(patch).length > 0) c.drawings.update(vid, patch);
          if (d.locked !== locked) c.drawings.lock(vid, locked);
          if (!d.visible) c.drawings.show(vid, true);
          if (Object.keys(patch).length > 0 || d.locked !== locked || !d.visible) touched.set(vid, { price, text, time: at, style: styleKey });
        }
        handlers.current.set(vid, line);
      }
      for (const [lineId, vid] of [...owned.current]) {
        if (wanted.has(lineId)) continue;
        c.drawings.remove(vid);
        owned.current.delete(lineId);
        handlers.current.delete(vid);
        applied.current.delete(vid);
      }
    });
    // What the sync left, to tell the next time whether anything else moved it.
    if (touched.size > 0) {
      const after = new Map(c.drawings.all().map((d) => [d.id, d]));
      for (const [vid, set] of touched) applied.current.set(vid, { ...set, look: lookOf(after.get(vid)) });
    }
  }, [lines, bars, loaded, tf, drift, view]);

  // Markers on Vela's timeline lane: B/E/T/S tokens, a pin for a Telegram call.
  useEffect(() => {
    const c = chart.current;
    if (!c || !loaded) return;
    const marks: TimelineMark[] = markers.map((m) => ({
      id: m.id,
      time: m.t,
      glyph: { shape: m.data.kind === 'call' ? 'pin' : 'circle', color: MARK_COLOR[m.data.kind], letter: m.data.kind === 'call' ? 'T' : m.data.kind },
      group: m.data.kind === 'call' ? 'calls' : 'fills',
      tooltip: m.data.kind === 'call' ? 'Telegram call' : { B: 'Bought', E: 'Exited', T: 'Target hit', S: 'Stop hit' }[m.data.kind],
    }));
    c.marks.set(marks);
  }, [markers, loaded]);

  // VWAP: a locked polyline per IST day through each bar's value — the
  // option's own, or the future's moved onto the index. One line per day, so
  // yesterday's close is never joined to today's open.
  useEffect(() => {
    const c = chart.current;
    if (!c || !loaded || !bars) return;
    const ex = extras.current;
    const days = vwap ? vwapDays(sym.kind === 'index' ? vwapOnIndex(futureBars, bars) : vwapOf(bars)).filter((d) => d.points.length >= 2) : [];
    const wanted = new Set(days.map((d) => d.day));
    quietly(applying, () => {
      for (const [day, vid] of ex.vwap) {
        if (wanted.has(day)) continue;
        c.drawings.remove(vid);
        ex.vwap.delete(day);
        appliedVwap.current.delete(vid);
      }
      const present = new Set(c.drawings.all().map((d) => d.id));
      for (const { day, points: anchors } of days) {
        // Every update costs Vela an undo copy of all drawings: only a changed line is redrawn.
        const signature = anchors.map((a) => `${a.time}:${a.price.toFixed(2)}`).join(',');
        const vid = ex.vwap.get(day);
        if (vid && present.has(vid)) {
          if (signature !== appliedVwap.current.get(vid)) c.drawings.update(vid, { anchors });
          appliedVwap.current.set(vid, signature);
          continue;
        }
        const d = c.drawings.add('polyline', { anchors, style: { lineColor: C.indicator, lineWidth: 1, lineStyle: 'solid' } });
        if (!d) continue;
        ex.vwap.set(day, d.id);
        appliedVwap.current.set(d.id, signature);
        c.drawings.lock(d.id, true);
      }
    });
  }, [vwap, bars, futureBars, loaded, sym.kind]);

  // Standard pivots for today, from a week of 15-minute bars; read again every
  // half hour in case the page stays open past midnight.
  useEffect(() => {
    if (!pivots) return;
    let live = true;
    const load = () =>
      fetchDays(serverBase, symbol, '15m', 8)
        .then((dayBars) => {
          if (!live) return;
          const byDay = pivotsByDay(dayBars);
          const today = Math.max(...byDay.keys());
          const levels = byDay.get(today);
          if (levels && Number.isFinite(today)) setPivotLevels({ key, value: pivotLines(levels) });
        })
        .catch(() => {});
    void load();
    const timer = window.setInterval(() => void load(), 30 * 60_000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [pivots, symbol, serverBase, key]);

  // The pivot lines: locked, orange, anchored where the labels stay in view.
  useEffect(() => {
    const c = chart.current;
    if (!c || !loaded || !bars) return;
    const ex = extras.current;
    const levels = pivots && pivotLevels?.key === key ? pivotLevels.value : [];
    const at = anchorTime(c, bars[0].t);
    const present = new Set(c.drawings.all().map((d) => d.id));
    const wanted = new Set<string>();
    quietly(applying, () => {
      for (const { name, value } of levels) {
        wanted.add(name);
        const text = `${name} (${value.toLocaleString('en-IN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })})`;
        const vid = ex.pivots.get(name);
        if (vid && present.has(vid)) {
          // Only what changed: every update costs Vela an undo copy of all drawings.
          const prev = appliedPivots.current.get(vid);
          const patch: Partial<SerializedDrawing> = {};
          if (prev?.price !== value || prev?.time !== at) patch.anchors = [{ time: at, price: value }];
          if (prev?.text !== text) patch.text = pivotText(text);
          if (Object.keys(patch).length > 0) c.drawings.update(vid, patch);
          appliedPivots.current.set(vid, { price: value, text, time: at });
        } else {
          const d = c.drawings.add('hline', { anchors: [{ time: at, price: value }], style: { lineColor: PIVOT, lineWidth: 1, lineStyle: 'solid' }, text: pivotText(text) });
          if (!d) continue;
          ex.pivots.set(name, d.id);
          appliedPivots.current.set(d.id, { price: value, text, time: at });
          c.drawings.lock(d.id, true);
          // A pivot is a horizontal line too: a line the user draws must not take its look.
          c.drawings.resetToolDefaults('hline');
        }
      }
      for (const [name, vid] of [...ex.pivots]) {
        if (wanted.has(name)) continue;
        c.drawings.remove(vid);
        ex.pivots.delete(name);
        appliedPivots.current.delete(vid);
      }
    });
  }, [pivots, pivotLevels, key, bars, loaded, view]);

  return <div ref={host} className="absolute inset-0" />;
}
