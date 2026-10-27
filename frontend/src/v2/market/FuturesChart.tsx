import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { dispose, init, type AxisCreateRangeCallback, type Chart, type KLineData } from 'klinecharts';
import { ChevronDown, Layers as LayersIcon } from 'lucide-react';
import { apiUrl } from '../../lib/api';
import { streamToken } from '../lib/api';
import { price, signedPct, toneOf } from '../lib/format';
import type { MarketIndex } from '../lib/types';
import { fetchBars, toK, upsert } from '../trade/bars';
import { CHART_STYLES, THOUSANDS, VWAP_SERIES } from '../trade/chartSetup';
import { countdown, futureName, inSession, TF_MS, vwapOf, type Bar, type Tf } from '../trade/model';
import { CheckboxRow } from '../ui/CheckboxRow';
import { Popover } from '../ui/Popover';
import { dayChange, MARKET_INDICES, pcrByBar } from './model';
import { PCR_SERIES, thresholdLines } from './pcrIndicator';

const CHART_ID = 'market-futures';
const PCR_PANE = 'pcr_pane';
const TFS: Tf[] = ['1m', '5m', '15m'];
const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

export interface MarketLayers {
  vwap: boolean;
  pcr: boolean;
}

/** The PCR pane's range, stretched to take in the thresholds. */
function rangeWith(values: { current: number[] }): AxisCreateRangeCallback {
  return ({ defaultRange }) => {
    const xs = values.current;
    if (xs.length === 0) return defaultRange;
    const known = defaultRange.to > defaultRange.from;
    const lo = Math.min(...xs, known ? defaultRange.from : Infinity);
    const hi = Math.max(...xs, known ? defaultRange.to : -Infinity);
    const pad = Math.max((hi - lo) * 0.12, 0.05);
    const from = lo - pad;
    const to = hi + pad;
    const range = to - from;
    return { from, to, range, realFrom: from, realTo: to, realRange: range, displayFrom: from, displayTo: to, displayRange: range };
  };
}

/**
 * The index future on KLineChart, with its VWAP and a PCR pane — the two
 * things the indicator scores. Bars come from the candle service; the PCR
 * from the checks.
 */
export function FuturesChart({ serverBase, index, onIndex, tf, onTf, checks, buyAbove, sellBelow, layers, onLayers, compact }: {
  serverBase: string;
  index: MarketIndex;
  onIndex: (i: MarketIndex) => void;
  tf: Tf;
  onTf: (tf: Tf) => void;
  /** PCR at each check: when its snapshot was taken (Unix ms). */
  checks: { t: number; v: number }[];
  buyAbove: number;
  sellBelow: number;
  layers: MarketLayers;
  onLayers: (l: MarketLayers) => void;
  /** Phone: a timeframe menu instead of the switch. */
  compact: boolean;
}) {
  const host = useRef<HTMLDivElement>(null);
  const chart = useRef<Chart | null>(null);
  const pushBar = useRef<((k: KLineData) => void) | null>(null);
  const symbol = futureName(index);
  const want = useRef({ base: serverBase, symbol, tf });
  const tfRef = useRef(tf);
  const pcrRange = useRef<number[]>([]);
  const version = useRef({ vwap: 0, pcr: 0 });
  const [bars, setBars] = useState<Bar[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<'pick' | 'tf' | 'layers' | null>(null);
  const close = useCallback(() => setMenu(null), []);

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
    c.setDataLoader({
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
    return () => {
      VWAP_SERIES.delete(CHART_ID);
      PCR_SERIES.delete(CHART_ID);
      dispose(el);
      chart.current = null;
    };
  }, []);

  // This future and timeframe: the loader fetches the history, the stream
  // keeps the forming bar live.
  useEffect(() => {
    const c = chart.current;
    if (!c) return;
    c.setSymbol({ ticker: symbol, pricePrecision: 2, volumePrecision: 0 });
    c.setPeriod({ type: 'minute', span: TF_MS[tf] / 60_000 });
    const token = streamToken();
    const params = new URLSearchParams({ symbols: symbol, tf });
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
        if (u.symbol !== symbol) continue;
        setBars((prev) => (prev ? upsert(prev, u.bar) : prev));
        pushBar.current?.(toK(u.bar));
      }
    });
    return () => {
      es.close();
      setBars(null);
      setError(null);
    };
  }, [serverBase, symbol, tf]);

  // VWAP, from the future's own volume.
  useEffect(() => {
    const c = chart.current;
    if (!c || !bars) return;
    if (!layers.vwap) {
      c.removeIndicator({ name: 'v2Vwap' });
      return;
    }
    VWAP_SERIES.set(CHART_ID, vwapOf(bars));
    version.current.vwap += 1;
    if (c.getIndicators({ name: 'v2Vwap' }).length === 0) {
      c.createIndicator({ name: 'v2Vwap', paneId: 'candle_pane', extendData: { key: CHART_ID }, calcParams: [version.current.vwap] }, true);
    } else {
      c.overrideIndicator({ name: 'v2Vwap', calcParams: [version.current.vwap] });
    }
  }, [layers.vwap, bars]);

  // PCR, in its own pane, with the thresholds it is read against.
  useEffect(() => {
    const c = chart.current;
    if (!c || !bars) return;
    if (!layers.pcr) {
      c.removeIndicator({ name: 'v2Pcr' });
      return;
    }
    PCR_SERIES.set(CHART_ID, { values: pcrByBar(bars, checks, TF_MS[tf]), buyAbove, sellBelow });
    version.current.pcr += 1;
    pcrRange.current = thresholdLines({ buyAbove, sellBelow }).map((l) => l.value);
    if (c.getIndicators({ name: 'v2Pcr' }).length === 0) {
      c.createIndicator({
        name: 'v2Pcr',
        paneId: PCR_PANE,
        extendData: { key: CHART_ID },
        calcParams: [version.current.pcr],
        styles: { lastValueMark: { show: true, text: { show: true } } },
      });
      c.setPaneOptions({ id: PCR_PANE, height: compact ? 72 : 120, minHeight: 56 });
    } else {
      c.overrideIndicator({ name: 'v2Pcr', calcParams: [version.current.pcr] });
    }
    c.overrideYAxis({ paneId: PCR_PANE, createRange: rangeWith(pcrRange) });
  }, [layers.pcr, bars, checks, buyAbove, sellBelow, tf, compact]);

  const last = bars?.[bars.length - 1] ?? null;
  const change = bars ? dayChange(bars) : null;
  const label = `${index} futures`;

  return (
    <section className="flex h-full min-h-0 min-w-0 flex-col overflow-hidden rounded-12 border border-line bg-inset">
      <header className="flex h-11 shrink-0 items-center gap-2 border-b border-line px-3">
        <div className="relative">
          <button type="button" aria-expanded={menu === 'pick'} onClick={() => setMenu(menu === 'pick' ? null : 'pick')} className="flex items-center gap-1 type-body-strong text-fg">
            {label}
            <ChevronDown size={14} className="text-fg-muted" />
          </button>
          <Popover open={menu === 'pick'} onClose={close}>
            {MARKET_INDICES.map((i) => (
              <button
                key={i.id}
                type="button"
                onClick={() => (onIndex(i.id), close())}
                className={`flex h-9 w-full items-center rounded-6 px-2 type-label ${i.id === index ? 'bg-raised text-fg' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}
              >
                {i.id} futures
              </button>
            ))}
          </Popover>
        </div>
        <span className="num-medium text-fg">{last ? price(last.c) : '—'}</span>
        {change != null && <span className={`num-small ${TONE_TEXT[toneOf(change)]}`}>{signedPct(change)}</span>}
        <span className="flex-1" />
        {compact ? (
          <div className="relative">
            <button type="button" aria-expanded={menu === 'tf'} onClick={() => setMenu(menu === 'tf' ? null : 'tf')} className="flex h-8 items-center gap-1 rounded-6 border border-line px-2.5 type-label text-fg">
              {tf}
              <ChevronDown size={14} className="text-fg-muted" />
            </button>
            <Popover open={menu === 'tf'} onClose={close} align="right">
              {TFS.map((t) => (
                <button key={t} type="button" onClick={() => (onTf(t), close())} className={`flex h-10 w-full items-center rounded-6 px-3 type-label ${t === tf ? 'bg-raised text-fg' : 'text-fg-muted'}`}>
                  {t}
                </button>
              ))}
            </Popover>
          </div>
        ) : (
          <div role="tablist" aria-label="Timeframe" className="flex gap-0.5 rounded-6 bg-canvas p-0.5">
            {TFS.map((t) => (
              <button key={t} type="button" role="tab" aria-selected={t === tf} onClick={() => onTf(t)} className={`h-6 rounded-4 px-2.5 type-label ${t === tf ? 'bg-raised text-fg' : 'text-fg-muted hover:text-fg'}`}>
                {t}
              </button>
            ))}
          </div>
        )}
        <div className="relative">
          <button type="button" aria-label="Layers" aria-expanded={menu === 'layers'} onClick={() => setMenu(menu === 'layers' ? null : 'layers')} className="grid size-8 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg">
            <LayersIcon size={16} />
          </button>
          <Popover open={menu === 'layers'} onClose={close} align="right">
            <div className="px-2 pt-1 pb-1 type-caption text-fg-muted">Show on this chart</div>
            <CheckboxRow checked={layers.vwap} title="VWAP" note="From the future's own trades, today" onChange={(v) => onLayers({ ...layers, vwap: v })} />
            <CheckboxRow checked={layers.pcr} title="PCR" note="At each check, with the lines it is read against" onChange={(v) => onLayers({ ...layers, pcr: v })} />
          </Popover>
        </div>
      </header>
      <div className="relative min-h-0 flex-1">
        <div ref={host} className="absolute inset-0" />
        {(bars == null || error) && (
          <div className="pointer-events-none absolute inset-0 grid place-items-center">
            <span className={`rounded-8 bg-inset/90 px-3 py-1.5 type-caption ${error ? 'text-loss' : 'text-fg-muted'}`}>{error ?? 'Loading bars…'}</span>
          </div>
        )}
      </div>
    </section>
  );
}
