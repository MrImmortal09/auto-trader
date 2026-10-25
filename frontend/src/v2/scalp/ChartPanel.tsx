import { useCallback, useState } from 'react';
import { ChevronDown, ChevronUp, Layers as LayersIcon } from 'lucide-react';
import { INDEX_ORDER } from '../../lib/execute';
import { price, signedPct, toneOf } from '../lib/format';
import type { ChainAnswer } from '../lib/types';
import { CheckboxRow } from '../ui/CheckboxRow';
import { Popover } from '../ui/Popover';
import type { CrosshairBus } from './bus';
import { KChart, type ChartLine, type ChartMarker, type Placing } from './KChart';
import type { Layers } from './lines';
import { sameSym, symbolLabel, type Bar, type ChartSym, type Tf } from './model';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;
const TFS: Tf[] = ['1m', '5m', '15m'];

/** One chart: its header (instrument, timeframe, Layers) and the chart. */
export function ChartPanel({ chartId, serverBase, sym, tf, onSym, onTf, layers, onLayers, spot, change, chain, lines, markers, placing, onPlacingDone, onCancelPlacing, bus, compact, onHide }: {
  chartId: string;
  serverBase: string;
  sym: ChartSym;
  tf: Tf;
  onSym: (s: ChartSym) => void;
  onTf: (tf: Tf) => void;
  layers: Layers;
  onLayers: (l: Layers) => void;
  /** Each index's value and its change today. */
  spot: Record<string, number | null>;
  change: Record<string, number | null>;
  /** The chart's index's nearest-expiry strikes, for the picker. */
  chain: ChainAnswer | undefined;
  lines: ChartLine[];
  markers: ChartMarker[];
  placing: (Placing & { what: string }) | null;
  onPlacingDone: () => void;
  onCancelPlacing: () => void;
  bus: CrosshairBus;
  /** Phone: a timeframe menu instead of the switch. */
  compact: boolean;
  /** Phone: folds the chart away, for more of the ladder. */
  onHide?: () => void;
}) {
  const [menu, setMenu] = useState<'pick' | 'layers' | 'tf' | null>(null);
  const [chartState, setChartState] = useState<{ loading: boolean; error: string | null; last: Bar | null }>({ loading: true, error: null, last: null });
  const close = useCallback(() => setMenu(null), []);
  const onState = useCallback((s: { loading: boolean; error: string | null; last: Bar | null }) => setChartState(s), []);

  const index = sym.index;
  const shown = sym.kind === 'index' ? spot[index] ?? chartState.last?.c ?? null : chartState.last?.c ?? null;
  const pct = sym.kind === 'index' ? change[index] ?? null : null;
  const spotNow = spot[index];
  const atm = chain && spotNow != null ? chain.strikes.reduce((best, s, i, all) => (Math.abs(s.strike - spotNow) < Math.abs(all[best].strike - spotNow) ? i : best), 0) : -1;
  const near = chain && atm >= 0 ? chain.strikes.slice(Math.max(0, atm - 5), atm + 6) : [];

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-12 border border-line bg-inset">
      <header className="flex h-10 shrink-0 items-center gap-2 border-b border-line px-3">
        <div className="relative">
          <button type="button" aria-expanded={menu === 'pick'} onClick={() => setMenu(menu === 'pick' ? null : 'pick')} className="flex items-center gap-1 type-body-strong text-fg">
            {symbolLabel(sym)}
            <ChevronDown size={14} className="text-fg-muted" />
          </button>
          <Popover open={menu === 'pick'} onClose={close}>
            <div className="px-2 pt-1 pb-1.5 type-caption text-fg-muted">Index</div>
            <div className="grid grid-cols-3 gap-1">
              {INDEX_ORDER.map((i) => {
                const s: ChartSym = { kind: 'index', index: i };
                return (
                  <button key={i} type="button" onClick={() => (onSym(s), close())} className={`h-8 rounded-6 px-2 type-label ${sameSym(s, sym) ? 'bg-raised text-fg' : 'text-fg-muted hover:bg-raised hover:text-fg'}`}>
                    {i}
                  </button>
                );
              })}
            </div>
            <div className="px-2 pt-3 pb-1.5 type-caption text-fg-muted">{chain ? `${index} options · expiry ${chain.expiry.slice(8, 10)}/${chain.expiry.slice(5, 7)}` : `${index} options`}</div>
            {near.length === 0 ? (
              <p className="px-2 pb-2 type-caption text-fg-muted">The option list loads after the Kotak login.</p>
            ) : (
              <div className="max-h-64 overflow-y-auto">
                {near.map((s) => (
                  <div key={s.strike} className="flex items-center gap-1">
                    {(['CE', 'PE'] as const).map((side) => {
                      const opt: ChartSym = { kind: 'option', index, expiry: chain!.expiry, strike: s.strike, side };
                      const has = side === 'CE' ? s.ce : s.pe;
                      return (
                        <button
                          key={side}
                          type="button"
                          disabled={!has}
                          onClick={() => (onSym(opt), close())}
                          className={`h-8 flex-1 rounded-6 px-2 text-left num-small ${sameSym(opt, sym) ? 'bg-raised text-fg' : 'text-fg-muted hover:bg-raised hover:text-fg'} disabled:opacity-30`}
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
        <span className="num-medium text-fg">{shown == null ? '—' : price(shown)}</span>
        {pct != null && <span className={`num-small ${TONE_TEXT[toneOf(pct)]}`}>{signedPct(pct)}</span>}
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
            <CheckboxRow checked={layers.myTrades} title="My scalp trades" note="Entry, stop, target · drag to move" onChange={(v) => onLayers({ ...layers, myTrades: v })} />
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
          </Popover>
        </div>
        {onHide && (
          <button type="button" aria-label="Hide chart" title="Hide chart" onClick={onHide} className="grid size-8 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg">
            <ChevronUp size={16} />
          </button>
        )}
      </header>
      <div className="relative min-h-0 flex-1">
        <KChart
          chartId={chartId}
          serverBase={serverBase}
          sym={sym}
          tf={tf}
          lines={lines}
          markers={markers}
          vwap={layers.vwap}
          pivots={layers.pivots}
          placing={placing}
          onPlacingDone={onPlacingDone}
          bus={bus}
          onState={onState}
        />
        {placing && (
          <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center">
            <div className="pointer-events-auto flex items-center gap-3 rounded-8 border border-line bg-panel px-3 py-1.5 shadow-lg">
              <span className="type-label text-fg">Tap where the {placing.what} goes</span>
              <button type="button" onClick={onCancelPlacing} className="type-label text-fg-muted hover:text-fg">
                Cancel
              </button>
            </div>
          </div>
        )}
        {(chartState.loading || chartState.error) && (
          <div className="pointer-events-none absolute inset-0 grid place-items-center">
            <span className={`rounded-8 bg-inset/90 px-3 py-1.5 type-caption ${chartState.error ? 'text-loss' : 'text-fg-muted'}`}>{chartState.error ?? 'Loading bars…'}</span>
          </div>
        )}
      </div>
    </section>
  );
}
