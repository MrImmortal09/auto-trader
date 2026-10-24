import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { Vela, type SerializedDrawing, type TimelineMark } from '@luxalgo/vela';
import { apiUrl } from '../../lib/api';
import { getToken } from '../../lib/auth';
import { fetchBars, fetchDays, upsert } from './bars';
import { C, FONT, type MarkerData } from './chartSetup';
import type { ChartLine, ChartProps } from './KChart';
import { barAt, futureName, symbolName, TF_MS, vwapOf, vwapOnIndex, type Bar } from './model';
import { pivotLines, pivotsByDay } from './pivots';
import { CandleProvider, PROVIDER, velaSymbol, type Feed } from './velaProvider';

/** The design tokens on Vela's theme, per the handoff's table. */
const THEME = { background: C.canvas, textColor: C.muted, gridColor: C.grid, borderColor: C.line, upColor: C.profit, downColor: C.loss, fontFamily: FONT };
/** Pivots in TradingView's orange, as on the KLineChart page. */
const PIVOT = '#fb8c00';
/** A drag reports every move; the level is sent once the line has rested this long. */
const MOVE_SETTLE_MS = 300;
const MARK_COLOR: Record<MarkerData['kind'], string> = { B: C.profit, E: C.muted, T: C.profit, S: C.loss, call: C.indicator };

type Text = NonNullable<SerializedDrawing['text']>;

/** A level's words. Vela centres a horizontal line's label on its anchor,
 * which is kept a fifth of the way into the view, clear of both axes. The
 * pills' buttons (×, + Stop) are not words: those live in the positions panel. */
function lineText(value: string, color: string): Text {
  return { value, color, size: 'small', hAlign: 'center', vAlign: 'top', side: 'above' };
}
const WORD_PARTS = new Set(['label', 'pnl']);

/** A pivot's name, on its line. */
function pivotText(value: string): Text {
  return { value, color: PIVOT, size: 'tiny', hAlign: 'center', vAlign: 'top', side: 'above' };
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

/**
 * One Vela chart: `sym` at `tf`, the same order lines, markers, VWAP and
 * pivots as the KLineChart page, drawn with Vela's own tools — a level is a
 * horizontal-line drawing dragged with Vela's handles, markers sit on its
 * timeline lane, and "+ Stop" places on the next click. Bars come from the
 * candle service through a Vela data provider and follow the same stream.
 */
export default function VChart({ chartId, serverBase, sym, tf, lines, markers, vwap, pivots, placing, onPlacingDone, bus, onState }: ChartProps) {
  const host = useRef<HTMLDivElement>(null);
  const chart = useRef<Vela | null>(null);
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
  const applied = useRef(new Map<string, { price: number; text: string; time: number }>());
  // Bumped (once a frame) as the view pans or zooms, so anchors follow it.
  const [view, setView] = useState(0);
  const [pivotLevels, setPivotLevels] = useState<Keyed<{ name: string; value: number }[]> | null>(null);
  // A drag: the line's own edits settle before the level is sent, and the screen's price waits.
  const settling = useRef(new Map<string, number>());
  const extras = useRef<{ vwap: string | null; pivots: Map<string, string> }>({ vwap: null, pivots: new Map() });
  // Bumped when one of our drawings is erased, so the next sync puts it back.
  const [lost, setLost] = useState(0);
  const placingNow = useRef(placing);
  const doneNow = useRef(onPlacingDone);
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
    const c = new Vela(el, {
      symbol: velaSymbol(w.symbol),
      timeframe: w.tf,
      bars: 1500,
      live: true,
      theme: THEME,
      drawings: { toolbar: false },
      volume: false,
      currentPriceLine: true,
      animations: { liveBar: false },
    });
    c.data.registerProvider(PROVIDER, p);
    // The axis and readouts in IST, whatever the browser's zone.
    c.renderer.set('timezone', 'Asia/Kolkata');
    c.marks.defineGroup({ id: 'fills', label: 'Trades' }).defineGroup({ id: 'calls', label: 'Telegram calls' });
    chart.current = c;
    const settle = settling.current;
    const ownedMap = owned.current;
    const handlerMap = handlers.current;
    const appliedMap = applied.current;
    let frame = 0;
    const offs = [
      c.on('load:end', ({ bars: n }) => setPainted({ key: want.current.key, value: n > 0 })),
      c.on('viewport:changed', () => {
        if (frame) return;
        frame = requestAnimationFrame(() => {
          frame = 0;
          setView((n) => n + 1);
        });
      }),
      // A level dragged: once its edits rest, where it was dropped goes to the engine.
      c.on('drawing:edited', ({ id }) => {
        const line = handlers.current.get(id);
        if (!line?.onMove) return;
        const prev = settle.get(id);
        if (prev != null) window.clearTimeout(prev);
        settle.set(
          id,
          window.setTimeout(() => {
            settle.delete(id);
            const price = c.drawings.all().find((d) => d.id === id)?.anchors[0]?.price;
            const was = applied.current.get(id)?.price;
            // Our own updates echo as edits: only a price that moved is a drag.
            if (price == null || (was != null && Math.abs(price - was) < 1e-9)) return;
            applied.current.delete(id);
            handlers.current.get(id)?.onMove?.(price);
          }, MOVE_SETTLE_MS),
        );
      }),
      // Erased by the user: never a removed level — it comes back on the next sync.
      c.on('drawing:removed', ({ id }) => {
        let ours = false;
        for (const [lineId, vid] of owned.current) {
          if (vid !== id) continue;
          owned.current.delete(lineId);
          ours = true;
        }
        handlers.current.delete(id);
        applied.current.delete(id);
        if (extras.current.vwap === id) extras.current.vwap = null;
        for (const [name, vid] of extras.current.pivots) if (vid === id) extras.current.pivots.delete(name);
        if (ours) setLost((n) => n + 1);
      }),
      // "+ Stop": the next click on the price pane places it.
      c.renderer.onClick((e) => {
        const now = placingNow.current;
        if (!now || e.price == null || e.paneKind === 'study') return;
        if (now.onPlace(e.price)) doneNow.current();
      }),
      c.renderer.onCrosshairMove((e) => bus.publish(chartId, e.time)),
      bus.subscribe(chartId, (t) => c.renderer.setExternalCrosshair(t)),
    ];
    return () => {
      offs.forEach((off) => off());
      if (frame) cancelAnimationFrame(frame);
      for (const t of settle.values()) window.clearTimeout(t);
      settle.clear();
      c.destroy();
      chart.current = null;
      provider.current = null;
      ownedMap.clear();
      handlerMap.clear();
      appliedMap.clear();
      extras.current = { vwap: null, pivots: new Map() };
    };
  }, [bus, chartId]);

  // Another symbol or timeframe: switched in place, from nothing.
  useEffect(() => {
    const c = chart.current;
    if (!c) return;
    if (first.current) {
      first.current = false;
      return;
    }
    provider.current?.forget();
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
    const token = getToken();
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
  });

  // Order lines: one horizontal-line drawing each, dragged with Vela's own handle.
  useEffect(() => {
    const c = chart.current;
    if (!c || !loaded || !bars) return;
    const at = anchorTime(c, bars[0].t);
    const tfMs = TF_MS[tf];
    const wanted = new Set<string>();
    const present = new Set(c.drawings.all().map((d) => d.id));
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
      let vid = owned.current.get(line.id);
      if (vid && !present.has(vid)) vid = undefined;
      if (!vid) {
        const d = c.drawings.add('hline', { anchors: [{ time: at, price }], style, text: lineText(text, line.data.color) });
        if (!d) continue;
        vid = d.id;
        owned.current.set(line.id, vid);
        c.drawings.lock(vid, !line.data.draggable);
        applied.current.set(vid, { price, text, time: at });
      } else if (!settling.current.has(vid)) {
        const prev = applied.current.get(vid);
        const patch: Partial<SerializedDrawing> = {};
        if (prev?.price !== price || prev?.time !== at) patch.anchors = [{ time: at, price }];
        if (prev?.text !== text) patch.text = lineText(text, line.data.color);
        if (Object.keys(patch).length > 0) c.drawings.update(vid, patch);
        c.drawings.lock(vid, !line.data.draggable);
        applied.current.set(vid, { price, text, time: at });
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
  }, [lines, bars, loaded, tf, lost, view]);

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

  // VWAP: a locked polyline through each bar's value — the option's own, or
  // the future's moved onto the index.
  useEffect(() => {
    const c = chart.current;
    if (!c || !loaded || !bars) return;
    const ex = extras.current;
    if (!vwap) {
      if (ex.vwap) c.drawings.remove(ex.vwap);
      ex.vwap = null;
      return;
    }
    const series = sym.kind === 'index' ? vwapOnIndex(futureBars, bars) : vwapOf(bars);
    const anchors = [...series].sort((a, b) => a[0] - b[0]).map(([time, price]) => ({ time, price }));
    if (anchors.length < 2) return;
    const present = ex.vwap && c.drawings.all().some((d) => d.id === ex.vwap);
    if (present && ex.vwap) {
      c.drawings.update(ex.vwap, { anchors });
    } else {
      const d = c.drawings.add('polyline', { anchors, style: { lineColor: C.indicator, lineWidth: 1, lineStyle: 'solid' } });
      if (!d) return;
      ex.vwap = d.id;
      c.drawings.lock(d.id, true);
    }
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
    for (const { name, value } of levels) {
      wanted.add(name);
      const text = `${name} (${value.toLocaleString('en-IN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })})`;
      const vid = ex.pivots.get(name);
      if (vid && present.has(vid)) {
        c.drawings.update(vid, { anchors: [{ time: at, price: value }], text: pivotText(text) });
      } else {
        const d = c.drawings.add('hline', { anchors: [{ time: at, price: value }], style: { lineColor: PIVOT, lineWidth: 1, lineStyle: 'solid' }, text: pivotText(text) });
        if (!d) continue;
        ex.pivots.set(name, d.id);
        c.drawings.lock(d.id, true);
      }
    }
    for (const [name, vid] of [...ex.pivots]) {
      if (wanted.has(name)) continue;
      c.drawings.remove(vid);
      ex.pivots.delete(name);
    }
  }, [pivots, pivotLevels, key, bars, loaded, view]);

  return <div ref={host} className="absolute inset-0" />;
}
