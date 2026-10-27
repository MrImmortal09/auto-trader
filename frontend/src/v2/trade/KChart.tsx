import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { dispose, init, type AxisCreateRangeCallback, type Chart, type KLineData, type OverlayEvent } from 'klinecharts';
import { apiUrl } from '../../lib/api';
import { streamToken } from '../lib/api';
import { fetchBars, fetchDays, toK, upsert } from './bars';
import type { CrosshairBus } from './bus';
import { CHART_STYLES, PIVOT_SERIES, THOUSANDS, VWAP_SERIES, type MarkerData, type OrderLineData } from './chartSetup';
import { pivotsByDay } from './pivots';
import {
  barAt,
  countdown,
  futureName,
  inSession,
  symbolName,
  TF_MS,
  vwapOf,
  vwapOnIndex,
  type Bar,
  type ChartSym,
  type Tf,
} from './model';

/** A horizontal order line: at a price, or at the bar close at a time (where
 * the index was when a scalp was bought). */
export interface ChartLine {
  id: string;
  price: number | { at: number };
  data: OrderLineData;
  /** Dropped after a drag, at this price. */
  onMove?: (price: number) => void;
  /** One of its pills was clicked. */
  onPart?: (key: string) => void;
  zLevel?: number;
}

/** A B/E/T/S marker, or a Telegram call: at a price, or at the bar's low
 * (buys) or high (the rest). */
export interface ChartMarker {
  id: string;
  t: number;
  price?: number;
  data: MarkerData;
}

/** Put a new line where the next click or tap lands. */
export interface Placing {
  /** Stays the same while the same level is being placed. */
  key: string;
  data: OrderLineData;
  /** `false` leaves nothing set there: place again. */
  onPlace: (price: number) => boolean;
}

/** The default price range, stretched to take in every order line. */
function rangeWithLines(prices: { current: number[] }): AxisCreateRangeCallback {
  return ({ defaultRange }) => {
    const xs = prices.current;
    if (xs.length === 0) return defaultRange;
    const lo = Math.min(...xs);
    const hi = Math.max(...xs);
    if (lo >= defaultRange.from && hi <= defaultRange.to) return defaultRange;
    const pad = (Math.max(hi, defaultRange.to) - Math.min(lo, defaultRange.from)) * 0.05;
    const from = lo < defaultRange.from ? lo - pad : defaultRange.from;
    const to = hi > defaultRange.to ? hi + pad : defaultRange.to;
    const range = to - from;
    return { from, to, range, realFrom: from, realTo: to, realRange: range, displayFrom: from, displayTo: to, displayRange: range };
  };
}

/** What either chart (KLineChart or Vela) is given. */
export interface ChartProps {
  chartId: string;
  serverBase: string;
  sym: ChartSym;
  tf: Tf;
  lines: ChartLine[];
  markers: ChartMarker[];
  vwap: boolean;
  /** Standard pivot points, daily. */
  pivots: boolean;
  placing: Placing | null;
  onPlacingDone: () => void;
  bus: CrosshairBus;
  /** Loading, an error, or the latest bar's close. */
  onState?: (s: { loading: boolean; error: string | null; last: Bar | null }) => void;
}

/**
 * One KLineChart: `sym` at `tf`, with its order lines, markers and VWAP. The
 * bars come from the candle service and follow its stream; an index chart's
 * VWAP is its future's, moved onto the index by the basis.
 */
export function KChart({ chartId, serverBase, sym, tf, lines, markers, vwap, pivots, placing, onPlacingDone, bus, onState }: ChartProps) {
  const host = useRef<HTMLDivElement>(null);
  const chart = useRef<Chart | null>(null);
  const pushBar = useRef<((k: KLineData) => void) | null>(null);
  const [bars, setBars] = useState<Bar[] | null>(null);
  const [futureBars, setFutureBars] = useState<Bar[]>([]);
  const [error, setError] = useState<string | null>(null);
  const tfRef = useRef(tf);
  // What the chart should be showing; a slow answer for anything else is dropped.
  const want = useRef({ base: serverBase, symbol: symbolName(sym), tf });
  // Line callbacks by overlay id, so overlays never hold stale closures.
  const handlers = useRef(new Map<string, ChartLine>());
  // The price each overlay was last put at, to move only what changed.
  const applied = useRef(new Map<string, number>());
  const dragging = useRef<string | null>(null);
  const vwapVersion = useRef(0);
  const pivotVersion = useRef(0);
  // The lines' prices: the price axis always takes them in, so a stop or
  // target far from the candles is still on screen.
  const linePrices = useRef<number[]>([]);
  // The latest "+ Stop" handler and label: the line placing it outlives renders.
  const placingNow = useRef<Placing | null>(placing);
  const doneNow = useRef(onPlacingDone);

  const symbol = symbolName(sym);
  const future = sym.kind === 'index' ? futureName(sym.index) : null;

  useEffect(() => {
    tfRef.current = tf;
    want.current = { base: serverBase, symbol, tf };
  }, [serverBase, symbol, tf]);

  // The chart itself, for the component's lifetime.
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    const c = init(el, {
      styles: CHART_STYLES,
      timezone: 'Asia/Kolkata',
      locale: 'en-US',
      thousandsSeparator: THOUSANDS,
      // Charts are watched, not driven from the keyboard.
      hotkey: { enabled: false },
      formatter: {
        formatExtendText: ({ data }) => {
          const now = Date.now();
          return inSession(now) ? countdown(data.timestamp + TF_MS[tfRef.current], now) : '';
        },
      },
    });
    if (!c) return;
    chart.current = c;
    const placed = applied.current;
    c.overrideYAxis({ paneId: 'candle_pane', createRange: rangeWithLines(linePrices) });
    c.setDataLoader({
      // A few days of bars, then no more: the candle service keeps those.
      getBars: async ({ type, callback }) => {
        if (type !== 'init') return callback([], false);
        const asked = want.current;
        try {
          const loaded = await fetchBars(asked.base, asked.symbol, asked.tf);
          if (want.current !== asked) return;
          setBars(loaded);
          callback(loaded.map(toK), false);
        } catch (e) {
          if (want.current !== asked) return;
          setError(e instanceof Error ? e.message : 'Bars unavailable');
          callback([], false);
        }
      },
      subscribeBar: ({ callback }) => {
        pushBar.current = callback;
      },
      unsubscribeBar: () => {
        pushBar.current = null;
      },
    });
    const onCross = (data?: unknown) => {
      const x = (data as { x?: number } | undefined)?.x;
      if (x == null) return bus.publish(chartId, null);
      const point = c.convertFromPixel([{ x }], { paneId: 'candle_pane' }) as Array<{ timestamp?: number }>;
      bus.publish(chartId, point[0]?.timestamp ?? null);
    };
    c.subscribeAction('onCrosshairChange', onCross);
    const unsub = bus.subscribe(chartId, (t) => {
      if (t == null) return c.executeAction('onCrosshairChange', {});
      const p = c.convertToPixel({ timestamp: t }, { paneId: 'candle_pane' }) as { x?: number };
      c.executeAction('onCrosshairChange', p.x == null ? {} : { x: p.x, paneId: 'candle_pane' });
    });
    return () => {
      unsub();
      c.unsubscribeAction('onCrosshairChange', onCross);
      VWAP_SERIES.delete(chartId);
      PIVOT_SERIES.delete(chartId);
      dispose(el);
      chart.current = null;
      placed.clear();
    };
  }, [bus, chartId]);

  // This symbol and timeframe: the chart loads the history (see the loader),
  // the stream keeps the forming bar and the future's bars live.
  useEffect(() => {
    const c = chart.current;
    if (!c) return;
    let live = true;
    applied.current.clear();
    c.removeOverlay();
    c.setSymbol({ ticker: symbol, pricePrecision: 2, volumePrecision: 0 });
    c.setPeriod({ type: 'minute', span: TF_MS[tf] / 60_000 });
    if (future) {
      fetchBars(serverBase, future, tf)
        .then((fut) => live && setFutureBars(fut))
        .catch(() => live && setFutureBars([]));
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
          setBars((prev) => (prev ? upsert(prev, u.bar) : prev));
          pushBar.current?.(toK(u.bar));
        } else if (u.symbol === future) {
          setFutureBars((prev) => upsert(prev, u.bar));
        }
      }
    });
    return () => {
      live = false;
      es.close();
      // The next symbol starts from nothing.
      setBars(null);
      setFutureBars([]);
      setError(null);
    };
  }, [serverBase, symbol, future, tf]);

  const loaded = bars != null;

  useEffect(() => {
    onState?.({ loading: !loaded && !error, error, last: bars?.[bars.length - 1] ?? null });
  }, [loaded, error, bars, onState]);

  // VWAP: the option's own, or the future's moved onto the index.
  useEffect(() => {
    const c = chart.current;
    if (!c || !bars) return;
    if (!vwap) {
      c.removeIndicator({ name: 'v2Vwap' });
      return;
    }
    VWAP_SERIES.set(chartId, sym.kind === 'index' ? vwapOnIndex(futureBars, bars) : vwapOf(bars));
    vwapVersion.current += 1;
    if (c.getIndicators({ name: 'v2Vwap' }).length === 0) {
      c.createIndicator({ name: 'v2Vwap', paneId: 'candle_pane', extendData: { key: chartId }, calcParams: [vwapVersion.current] }, true);
    } else {
      c.overrideIndicator({ name: 'v2Vwap', calcParams: [vwapVersion.current] });
    }
  }, [vwap, bars, futureBars, chartId, sym.kind]);

  // Standard pivots, from a week of 15-minute bars so the trading day before
  // is there on a Monday or after a holiday. Fixed for the day; read again
  // every half hour in case the page stays open past midnight.
  useEffect(() => {
    const c = chart.current;
    if (!c) return;
    const show = () => {
      pivotVersion.current += 1;
      if (c.getIndicators({ name: 'v2Pivots' }).length === 0) {
        c.createIndicator({ name: 'v2Pivots', paneId: 'candle_pane', extendData: { key: chartId }, calcParams: [pivotVersion.current] }, true);
      } else {
        c.overrideIndicator({ name: 'v2Pivots', calcParams: [pivotVersion.current] });
      }
    };
    // Another symbol's levels never show on this one, even for a moment.
    PIVOT_SERIES.delete(chartId);
    if (!pivots) {
      c.removeIndicator({ name: 'v2Pivots' });
      return;
    }
    show();
    let live = true;
    const load = () =>
      fetchDays(serverBase, symbol, '15m', 8)
        .then((bars) => {
          if (!live) return;
          PIVOT_SERIES.set(chartId, pivotsByDay(bars));
          show();
        })
        .catch(() => {});
    void load();
    const timer = window.setInterval(() => void load(), 30 * 60_000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [pivots, symbol, serverBase, chartId]);

  // Order lines and markers, kept in step with what the screen says.
  useEffect(() => {
    const c = chart.current;
    if (!c || !bars || bars.length === 0) return;
    const lastT = bars[bars.length - 1].t;
    const tfMs = TF_MS[tf];
    const wanted = new Set<string>();

    const prices: number[] = [];
    for (const line of lines) {
      const id = `line:${line.id}`;
      const price = typeof line.price === 'number' ? line.price : barAt(bars, line.price.at, tfMs)?.c;
      if (price == null || !Number.isFinite(price)) continue;
      wanted.add(id);
      prices.push(price);
      handlers.current.set(id, line);
      const exists = c.getOverlays({ id }).length > 0;
      if (!exists) {
        c.createOverlay({
          id,
          name: 'v2OrderLine',
          points: [{ timestamp: lastT, value: price }],
          extendData: line.data,
          zLevel: line.zLevel ?? 0,
          onPressedMoveStart: () => {
            dragging.current = id;
          },
          onPressedMoveEnd: (e: OverlayEvent<unknown>) => {
            dragging.current = null;
            const value = e.overlay.points[0]?.value;
            // Put back on the next sync unless the screen moves it.
            applied.current.delete(id);
            if (value != null) handlers.current.get(id)?.onMove?.(value);
          },
          onClick: (e: OverlayEvent<unknown>) => {
            const key = e.figure?.key;
            if (key && key !== 'label' && key !== 'line') handlers.current.get(id)?.onPart?.(key);
          },
        });
        applied.current.set(id, price);
      } else if (dragging.current !== id) {
        const moved = applied.current.get(id) !== price;
        c.overrideOverlay({ id, extendData: line.data, ...(moved ? { points: [{ timestamp: lastT, value: price }] } : {}) });
        applied.current.set(id, price);
      }
    }

    for (const m of markers) {
      const id = `mark:${m.id}`;
      const bar = barAt(bars, m.t, tfMs);
      const value = m.price ?? (bar ? (m.data.above ? bar.h : bar.l) : undefined);
      if (value == null || !bar) continue;
      wanted.add(id);
      if (c.getOverlays({ id }).length === 0) {
        c.createOverlay({ id, name: 'v2Marker', lock: true, points: [{ timestamp: bar.t, value }], extendData: m.data, zLevel: -1 });
      } else {
        c.overrideOverlay({ id, points: [{ timestamp: bar.t, value }], extendData: m.data });
      }
    }

    const key = (xs: number[]) => xs.map((x) => x.toFixed(2)).sort().join(',');
    if (key(prices) !== key(linePrices.current)) {
      linePrices.current = prices;
      // Asking again makes the axis take the new prices in.
      c.overrideYAxis({ paneId: 'candle_pane', createRange: rangeWithLines(linePrices) });
    }

    for (const o of c.getOverlays()) {
      if (o.id === 'placing') continue;
      if (!wanted.has(o.id)) {
        c.removeOverlay({ id: o.id });
        applied.current.delete(o.id);
        handlers.current.delete(o.id);
      }
    }
  }, [lines, markers, bars, tf]);

  useEffect(() => {
    placingNow.current = placing;
    doneNow.current = onPlacingDone;
    // Its label reads the latest prices.
    if (placing) chart.current?.overrideOverlay({ id: 'placing', extendData: placing.data });
  });

  // "+ Stop": a line that follows the pointer until the next click or tap. A
  // spot not taken leaves it following, to tap again.
  const placingKey = placing?.key ?? null;
  useEffect(() => {
    const c = chart.current;
    if (!c || !placingKey || !loaded) return;
    const start = () =>
      c.createOverlay({
        id: 'placing',
        name: 'v2OrderLine',
        extendData: placingNow.current?.data,
        zLevel: 10,
        onDrawEnd: (e: OverlayEvent<unknown>) => {
          const value = e.overlay.points[0]?.value;
          c.removeOverlay({ id: 'placing' });
          if (value != null && placingNow.current?.onPlace(value)) doneNow.current();
          else if (placingNow.current) start();
        },
      });
    start();
    return () => {
      c.removeOverlay({ id: 'placing' });
    };
  }, [placingKey, loaded]);

  return <div ref={host} className="absolute inset-0" />;
}
