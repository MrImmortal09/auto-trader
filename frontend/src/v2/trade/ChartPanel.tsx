import { lazy, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronUp, Layers as LayersIcon, Maximize2, Minimize2 } from 'lucide-react';
import { INDEX_ORDER } from '../../lib/execute';
import { price, signedPct, toneOf } from '../lib/format';
import type { ChainAnswer } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { CheckboxRow } from '../ui/CheckboxRow';
import { Popover } from '../ui/Popover';
import { Segment } from '../ui/Segment';
import type { CrosshairBus } from './bus';
import { markChartActive, useChartKeymap } from './chartKeymap';
import { DrawBanner, DrawMenu } from './DrawMenu';
import { useDrawing, type DrawApi } from './drawings';
import { expiryChip, type ExpiryRow } from './expiries';
import { KChart, type ChartLine, type ChartMarker, type Placing } from './KChart';
import type { Layers } from './lines';
import { CANDLE_STYLES, CHART_LIBS, sameSym, symbolLabel, symbolName, type Bar, type CandleStyle, type ChartLib, type ChartSym, type Tf } from './model';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;
const TFS: Tf[] = ['1m', '5m', '15m'];

// Vela is heavy and optional: only downloaded once a chart is switched to it.
const VChart = lazy(() => import('./VChart'));

/** A target being placed: how many lots it sells, 1 to `max` — the lots no
 * other target has. At `max` it sells all that is left when it is reached. */
export interface LotsPick {
  value: number;
  max: number;
  /** The trade has other targets already. */
  others: boolean;
  set: (lots: number) => void;
}

/** "2 lots", or at the top "all 4 lots" / "all left (2)". */
function lotsWords(l: LotsPick): string {
  if (l.value >= l.max) return l.others ? `all left (${l.max})` : `all ${l.max} ${l.max === 1 ? 'lot' : 'lots'}`;
  return `${l.value} ${l.value === 1 ? 'lot' : 'lots'}`;
}

/** The lots stepper in the "Tap where the target goes" prompt. */
function LotsStepper({ lots }: { lots: LotsPick }) {
  const step = 'grid size-6 place-items-center rounded-4 border border-line text-fg hover:bg-raised disabled:opacity-40';
  return (
    <span className="flex items-center gap-1.5" aria-label="Lots this target sells">
      <button type="button" className={step} disabled={lots.value <= 1} onClick={() => lots.set(lots.value - 1)} aria-label="Fewer lots">
        −
      </button>
      <span className="min-w-[88px] text-center type-label tnum text-profit">{lotsWords(lots)}</span>
      <button type="button" className={step} disabled={lots.value >= lots.max} onClick={() => lots.set(lots.value + 1)} aria-label="More lots">
        +
      </button>
    </span>
  );
}

/** One chart: its header (instrument, timeframe, Layers) and the chart. */
export function ChartPanel({ chartId, serverBase, sym, tf, onSym, onTf, layers, onLayers, style, onStyle, spot, change, expiries, chainAt, onNeedChain, lines, markers, placing, onPlacingDone, onCancelPlacing, bus, lib, onLib, compact, onHide, fullscreen = false, onFullscreen }: {
  chartId: string;
  serverBase: string;
  sym: ChartSym;
  tf: Tf;
  onSym: (s: ChartSym) => void;
  onTf: (tf: Tf) => void;
  layers: Layers;
  onLayers: (l: Layers) => void;
  style: CandleStyle;
  onStyle: (s: CandleStyle) => void;
  /** Each index's value and its change today. */
  spot: Record<string, number | null>;
  change: Record<string, number | null>;
  /** The chart's index's listed expiries, for the picker. */
  expiries: ExpiryRow[] | undefined;
  /** The chart's index's strikes for an expiry (null: the nearest), once loaded. */
  chainAt: (expiry: string | null) => ChainAnswer | undefined;
  /** The picker wants an expiry's strikes the screen hasn't loaded. */
  onNeedChain: (expiry: string | null) => void;
  lines: ChartLine[];
  markers: ChartMarker[];
  placing: (Placing & { what: string; lots?: LotsPick }) | null;
  onPlacingDone: () => void;
  onCancelPlacing: () => void;
  bus: CrosshairBus;
  /** Which library draws this chart. */
  lib: ChartLib;
  /** Phone: the library switch lives in the Layers menu. */
  onLib?: (lib: ChartLib) => void;
  /** Phone: a timeframe menu instead of the switch. */
  compact: boolean;
  /** Phone: folds the chart away, for more of the ladder. */
  onHide?: () => void;
  fullscreen?: boolean;
  onFullscreen?: (on: boolean) => void;
}) {
  const [menu, setMenu] = useState<'pick' | 'layers' | 'tf' | 'draw' | null>(null);
  const [chartState, setChartState] = useState<{ loading: boolean; error: string | null; last: Bar | null }>({ loading: true, error: null, last: null });
  // Drawing tools (KLineChart or Vela), while it draws this chart.
  const drawApi = useRef<DrawApi | null>(null);
  const { drawing, pick: pickTool, cancel: cancelDrawing } = useDrawing(useCallback(() => drawApi.current, []));
  useChartKeymap(chartId, useCallback(() => drawApi.current, []), placing != null);

  // Escape leaves full screen, but not while a level is being placed, a
  // drawing tool is being used, or a menu or dialog is open.
  useEffect(() => {
    if (!fullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (placing != null || drawing != null || menu != null || document.querySelector('[role="dialog"]')) return;
      onFullscreen?.(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [fullscreen, placing, drawing, menu, onFullscreen]);

  // The expiry the picker lists strikes for: what was picked on this
  // chart's current instrument, else the charted option's, else the nearest.
  const [picked, setPicked] = useState<{ on: string; expiry: string | null } | null>(null);
  const [undoState, setUndoState] = useState({ canUndo: false, canRedo: false });
  const strikeList = useRef<HTMLDivElement>(null);
  const atmRow = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setMenu(null), []);
  const onState = useCallback((s: { loading: boolean; error: string | null; last: Bar | null }) => setChartState(s), []);

  useEffect(() => {
    if (menu === 'draw') {
      setUndoState({
        canUndo: drawApi.current?.canUndo() ?? false,
        canRedo: drawApi.current?.canRedo() ?? false,
      });
    }
  }, [menu]);

  const index = sym.index;
  const symbolKey = sym.kind === 'option' ? `${sym.index}:${sym.expiry}` : sym.index;
  const pickExpiry = picked?.on === symbolKey ? picked.expiry : sym.kind === 'option' ? sym.expiry : null;
  const setPickExpiry = (expiry: string | null) => setPicked({ on: symbolKey, expiry });
  useEffect(() => {
    if (menu === 'pick') onNeedChain(pickExpiry);
  }, [menu, pickExpiry, onNeedChain]);

  const shown = sym.kind === 'index' ? spot[index] ?? chartState.last?.c ?? null : chartState.last?.c ?? null;
  // A different instrument is a new figure, not a tick.
  const flash = useTickFlash(shown, true, symbolName(sym));
  const pct = sym.kind === 'index' ? change[index] ?? null : null;
  const spotNow = spot[index];
  const chain = chainAt(pickExpiry);
  const strikes = chain?.strikes ?? [];
  const atm = chain && spotNow != null ? strikes.reduce((best, s, i, all) => (Math.abs(s.strike - spotNow) < Math.abs(all[best].strike - spotNow) ? i : best), 0) : -1;
  const all = expiries ?? [];
  const pickedDate = pickExpiry ?? all[0]?.date ?? chain?.expiry ?? null;

  // The list opens with ATM in the middle.
  useEffect(() => {
    if (menu !== 'pick') return;
    const box = strikeList.current;
    const row = atmRow.current;
    if (box && row) box.scrollTop = row.offsetTop - box.clientHeight / 2 + row.offsetHeight / 2;
  }, [menu, chain, atm]);

  return (
    // Not clipped as a whole: its menus (the symbol picker, Layers) open over
    // whatever is below a short chart. Only the chart itself is clipped.
    <section
      onPointerDown={() => markChartActive(chartId)}
      onPointerEnter={() => markChartActive(chartId)}
      className={`flex min-h-0 min-w-0 flex-1 flex-col ${
        fullscreen ? 'bg-canvas' : compact ? 'border-y border-line bg-panel' : 'bg-panel'
      }`}
    >
      {/* The controls on the right never shrink, so Hide chart stays on screen;
          the instrument's name and the price give way instead. */}
      <header className={`@container flex h-10 shrink-0 items-center border-b border-line ${compact ? 'gap-1.5 pr-1 pl-3' : 'gap-2 px-3'}`}>
        <div className="relative min-w-0">
          <button type="button" aria-expanded={menu === 'pick'} onClick={() => setMenu(menu === 'pick' ? null : 'pick')} title={symbolLabel(sym)} className="flex max-w-full items-center gap-1 text-left text-fg">
            {sym.kind === 'option' ? (
              // The index over the strike, so the strike is never cut off.
              <span className="flex min-w-0 flex-col">
                <span className="truncate type-caption leading-none text-fg-muted">{sym.index}</span>
                <span className="truncate type-body-strong leading-tight">
                  {sym.strike} {sym.side}
                </span>
              </span>
            ) : (
              <span className="truncate type-body-strong">{symbolLabel(sym)}</span>
            )}
            <ChevronDown size={14} className="shrink-0 text-fg-muted" />
          </button>
          <Popover open={menu === 'pick'} onClose={close} backdrop={compact}>
            <div className="px-2 pt-1 pb-1.5 type-caption text-fg-muted">Index</div>
            <div className="grid grid-cols-3 gap-1">
              {INDEX_ORDER.map((i) => {
                const s: ChartSym = { kind: 'index', index: i };
                return (
                  <button key={i} type="button" onClick={() => (onSym(s), close())} className={`h-8 rounded-6 px-2 type-label ${sameSym(s, sym) ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}>
                    {i}
                  </button>
                );
              })}
            </div>
            <div className="px-2 pt-3 pb-1.5 type-caption text-fg-muted">{index} options</div>
            {all.length > 0 && (
              <div role="tablist" aria-label="Expiry" className="flex gap-1 overflow-x-auto pb-1.5">
                {all.map((e, i) => {
                  const on = e.date === pickedDate;
                  return (
                    <button
                      key={e.date}
                      type="button"
                      role="tab"
                      aria-selected={on}
                      onClick={() => setPickExpiry(i === 0 ? null : e.date)}
                      className={`h-7 shrink-0 whitespace-nowrap rounded-6 px-2 type-caption tnum ${on ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}
                    >
                      {expiryChip(e, all)}
                    </button>
                  );
                })}
              </div>
            )}
            {strikes.length === 0 ? (
              <p className="px-2 pb-2 type-caption text-fg-subtle">{all.length === 0 ? 'The option list loads after the Kotak login.' : 'Loading strikes…'}</p>
            ) : (
              <div ref={strikeList} className="relative max-h-64 overflow-y-auto">
                {strikes.map((s, i) => (
                  <div key={s.strike} ref={i === atm ? atmRow : undefined} className={`flex items-center gap-1 ${i === atm ? 'rounded-6 bg-raised' : ''}`}>
                    {(['CE', 'PE'] as const).map((side) => {
                      const opt: ChartSym = { kind: 'option', index, expiry: chain!.expiry, strike: s.strike, side };
                      const has = side === 'CE' ? s.ce : s.pe;
                      return (
                        <button
                          key={side}
                          type="button"
                          disabled={!has}
                          onClick={() => (onSym(opt), close())}
                          className={`h-8 flex-1 rounded-6 px-2 text-left num-small ${sameSym(opt, sym) ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'} disabled:opacity-50`}
                        >
                          {s.strike} {side}
                        </button>
                      );
                    })}
                  </div>
                ))}
              </div>
            )}
          </Popover>
        </div>
        {/* On a narrow header the price is left to the chart's own axis label,
            and the day's change to the index tabs above. */}
        <span key={flash.key} className={`-mx-1 hidden shrink-0 rounded-4 px-1 num-medium text-fg ${flash.className} ${compact ? '@[21rem]:inline' : '@[28rem]:inline'}`}>{shown == null ? '—' : price(shown)}</span>
        {pct != null && !compact && <span className={`hidden shrink-0 num-small @[34rem]:inline ${TONE_TEXT[toneOf(pct)]}`}>{signedPct(pct)}</span>}
        <span className="min-w-0 flex-1" />
        {compact ? (
          <div className="relative shrink-0">
            <button type="button" aria-expanded={menu === 'tf'} onClick={() => setMenu(menu === 'tf' ? null : 'tf')} className="flex h-8 items-center gap-1 rounded-6 border border-line px-2.5 type-label text-fg transition-colors duration-120 hover:bg-raised/40">
              {tf}
              <ChevronDown size={14} className="text-fg-muted" />
            </button>
            <Popover open={menu === 'tf'} onClose={close} align="right" backdrop>
              {TFS.map((t) => (
                <button key={t} type="button" onClick={() => (onTf(t), close())} className={`flex h-10 w-full items-center rounded-6 px-3 type-label ${t === tf ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}>
                  {t}
                </button>
              ))}
            </Popover>
          </div>
        ) : (
          <div role="tablist" aria-label="Timeframe" className="flex gap-0.5 rounded-6 border border-line bg-inset p-0.5">
            {TFS.map((t) => (
              <button key={t} type="button" role="tab" aria-selected={t === tf} onClick={() => onTf(t)} className={`h-6 rounded-4 px-2.5 type-label transition-colors duration-120 ${t === tf ? 'bg-panel text-accent shadow-sm' : 'text-fg-muted hover:text-fg'}`}>
                {t}
              </button>
            ))}
          </div>
        )}
        {lib !== 'vela' && (
          <DrawMenu
            open={menu === 'draw'}
            onToggle={() => setMenu(menu === 'draw' ? null : 'draw')}
            onClose={close}
            onPick={pickTool}
            onClear={() => drawApi.current?.clear()}
            onUndo={() => drawApi.current?.undo()}
            onRedo={() => drawApi.current?.redo()}
            canUndo={undoState.canUndo}
            canRedo={undoState.canRedo}
            active={drawing}
            disabled={placing != null}
            compact={compact}
          />
        )}
        {onFullscreen && (
          <button
            type="button"
            aria-label={fullscreen ? 'Exit full screen' : 'Full screen'}
            title={fullscreen ? 'Exit full screen' : 'Full screen'}
            onClick={() => onFullscreen(!fullscreen)}
            className="grid size-8 shrink-0 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg"
          >
            {fullscreen ? <Minimize2 size={16} /> : <Maximize2 size={16} />}
          </button>
        )}
        <div className="relative shrink-0">
          <button type="button" aria-label="Layers" aria-expanded={menu === 'layers'} onClick={() => setMenu(menu === 'layers' ? null : 'layers')} className="grid size-8 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg">
            <LayersIcon size={16} />
          </button>
          <Popover open={menu === 'layers'} onClose={close} align="right" backdrop={compact} scroll>
            <div className="px-2 pt-1 pb-1 type-caption text-fg-muted">Show on this chart</div>
            <CheckboxRow checked={layers.myTrades} title="My trades" note="Entry, stop, target · drag to move" onChange={(v) => onLayers({ ...layers, myTrades: v })} />
            <CheckboxRow checked={layers.telegramTrades} title="Telegram trades" note="Lines show on the option chart" onChange={(v) => onLayers({ ...layers, telegramTrades: v })} />
            <CheckboxRow checked={false} disabled title="Drag Telegram stop and targets" note="Not available yet · Telegram lines are read-only" onChange={() => {}} />
            <CheckboxRow checked={layers.telegramCalls} title="Telegram call markers" note="When each call arrived" onChange={(v) => onLayers({ ...layers, telegramCalls: v })} />
            <CheckboxRow
              checked={layers.vwap}
              title="VWAP"
              note={sym.kind === 'index' ? `From ${index} futures` : "From this option's own trades"}
              onChange={(v) => onLayers({ ...layers, vwap: v })}
            />
            <CheckboxRow
              checked={layers.pivots}
              title="Pivot points"
              note="Standard, from the day before: P, R1–R5, S1–S5"
              onChange={(v) => onLayers({ ...layers, pivots: v })}
            />
            <CheckboxRow checked={layers.rsi} title="RSI" note="14 bars, lines at 70 and 30" onChange={(v) => onLayers({ ...layers, rsi: v })} />
            <CheckboxRow checked={layers.cci} title="CCI" note="20 bars, lines at +100 and −100" onChange={(v) => onLayers({ ...layers, cci: v })} />
            <div className="flex items-center justify-between gap-3 px-2 pt-3 pb-1">
              <span className="type-label text-fg">Candle style</span>
              <Segment label="Candle style" size="sm" value={style} onChange={onStyle} options={CANDLE_STYLES} />
            </div>
            {onLib && (
              <div className="flex items-center justify-between gap-3 px-2 pt-1 pb-1">
                <span className="type-label text-fg">Chart library</span>
                <Segment label="Chart library" size="sm" value={lib} onChange={onLib} options={CHART_LIBS} />
              </div>
            )}
          </Popover>
        </div>
        {onHide && !fullscreen && (
          <button type="button" aria-label="Hide chart" title="Hide chart" onClick={onHide} className="grid size-8 shrink-0 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg">
            <ChevronUp size={16} />
          </button>
        )}
      </header>
      <div className="relative min-h-0 flex-1 overflow-hidden">
        {lib === 'vela' ? (
          <Suspense fallback={null}>
            <VChart
              chartId={chartId}
              serverBase={serverBase}
              sym={sym}
              tf={tf}
              lines={lines}
              markers={markers}
              vwap={layers.vwap}
              pivots={layers.pivots}
              rsi={layers.rsi}
              cci={layers.cci}
              style={style}
              placing={placing}
              onPlacingDone={onPlacingDone}
              bus={bus}
              onState={onState}
              onStyle={onStyle}
              onStudies={(s) => {
                if (s.rsi !== layers.rsi || s.cci !== layers.cci) onLayers({ ...layers, ...s });
              }}
              drawRef={drawApi}
            />
          </Suspense>
        ) : (
          <KChart
            chartId={chartId}
            serverBase={serverBase}
            sym={sym}
            tf={tf}
            lines={lines}
            markers={markers}
            vwap={layers.vwap}
            pivots={layers.pivots}
            rsi={layers.rsi}
            cci={layers.cci}
            style={style}
            placing={placing}
            onPlacingDone={onPlacingDone}
            bus={bus}
            onState={onState}
            compact={compact}
            drawRef={drawApi}
          />
        )}
        {drawing && !placing && lib !== 'vela' && <DrawBanner tool={drawing} onCancel={cancelDrawing} />}
        {placing && (
          <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center">
            <div className="pointer-events-auto flex items-center gap-3 rounded-8 border border-line bg-panel px-3 py-1.5 shadow-md">
              <span className="type-label text-fg">Tap where the {placing.what} goes</span>
              {placing.lots && <LotsStepper lots={placing.lots} />}
              <button type="button" onClick={onCancelPlacing} className="type-label text-fg-muted hover:text-fg">
                Cancel
              </button>
            </div>
          </div>
        )}
        {(chartState.loading || chartState.error) && (
          <div className="pointer-events-none absolute inset-0 grid place-items-center">
            <span className={`rounded-8 bg-panel/90 px-3 py-1.5 type-caption ${chartState.error ? 'text-loss' : 'text-fg-subtle'}`}>{chartState.error ?? 'Loading bars…'}</span>
          </div>
        )}
      </div>
    </section>
  );
}
