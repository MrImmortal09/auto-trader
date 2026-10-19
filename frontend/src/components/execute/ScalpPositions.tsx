import { LoaderCircle, Moon } from 'lucide-react';
import type { ExecutePosition } from '../../types';
import { contractLabel, fmtPnl } from '../../lib/execute';
import { fmt } from '../../lib/format';
import { PriceCell } from './PriceCell';

function Tag({ tone, children }: { tone: 'scalp' | 'signal' | 'busy' | 'bad' | 'night'; children: React.ReactNode }) {
  const cls = {
    scalp: 'bg-primary/10 text-primary',
    signal: 'bg-surface-container-high text-on-surface-variant',
    busy: 'bg-amber-100 text-amber-900',
    bad: 'bg-error-container text-on-error-container',
    night: 'bg-inverse-surface text-inverse-on-surface',
  }[tone];
  return (
    <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[9px] font-extrabold uppercase tracking-wider ${cls}`}>
      {children}
    </span>
  );
}

function PositionRow({ p, busy, nearClose, onExit, onSellLot }: {
  p: ExecutePosition;
  busy: boolean;
  nearClose: boolean;
  onExit: (p: ExecutePosition) => void;
  onSellLot: (p: ExecutePosition) => void;
}) {
  const label = contractLabel(p.index, p.strike, p.option_type);
  const pnl = p.pnl;
  const pct = pnl != null && p.avg > 0 && p.qty > 0 ? (pnl / (p.avg * p.qty)) * 100 : null;
  const locked = busy || p.placing || p.exiting || p.halted;
  const canTrimLot = p.qty > p.lot_size && !locked;
  const accent = pnl == null ? 'bg-outline-variant' : pnl >= 0 ? 'bg-secondary' : 'bg-error';

  return (
    <div className="relative flex flex-wrap md:flex-nowrap items-center gap-x-4 gap-y-2 pl-4 pr-3 py-2.5 border-t border-outline-variant/70 first:border-t-0">
      <span aria-hidden className={`absolute left-0 top-2 bottom-2 w-1 rounded-r ${accent}`} />

      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="font-bold text-sm text-on-surface whitespace-nowrap">{label}</span>
          <Tag tone={p.manual ? 'scalp' : 'signal'}>{p.manual ? 'Scalp' : 'Signal'}</Tag>
          {p.placing && <Tag tone="busy"><LoaderCircle size={10} className="animate-spin" />Placing</Tag>}
          {p.adding && <Tag tone="busy"><LoaderCircle size={10} className="animate-spin" />Adding</Tag>}
          {p.exiting && <Tag tone="busy"><LoaderCircle size={10} className="animate-spin" />Exiting</Tag>}
          {p.halted && <Tag tone="bad">Halted — see log</Tag>}
          {p.manual && nearClose && !p.placing && <Tag tone="night"><Moon size={10} />Carries overnight</Tag>}
        </div>
        <div className="mt-0.5 text-[11px] text-on-surface-variant font-mono-code tabular-nums flex flex-wrap items-center gap-x-1">
          {p.placing ? (
            <span>buying at market…</span>
          ) : (
            <>
              <span className="font-semibold text-on-surface">{p.qty}</span> qty @ {fmt(p.avg)}
              <span className="mx-1 text-outline">·</span>LTP
              <PriceCell value={p.ltp} className="font-semibold text-on-surface" />
            </>
          )}
        </div>
      </div>

      <div className="text-right min-w-[96px]">
        <div className={`font-mono-code text-base font-extrabold tabular-nums ${pnl == null ? 'text-outline' : pnl >= 0 ? 'text-secondary' : 'text-error'}`}>
          {pnl == null ? '—' : fmtPnl(pnl)}
        </div>
        {pct != null && (
          <div className={`text-[11px] font-semibold tabular-nums ${pct >= 0 ? 'text-secondary' : 'text-error'}`}>
            {pct >= 0 ? '+' : ''}{pct.toFixed(1)}%
          </div>
        )}
      </div>

      {!p.placing && (
      <div className="flex gap-1.5 w-full md:w-auto">
        {p.qty > p.lot_size && (
          <button
            type="button"
            onClick={() => onSellLot(p)}
            disabled={!canTrimLot}
            title={`Sell one lot (${p.lot_size}) at market`}
            className="flex-1 md:flex-none h-10 md:h-8 px-3 rounded-md bg-surface-container-high text-on-surface text-[11px] font-extrabold tracking-wide hover:bg-surface-container-highest active:scale-95 transition disabled:text-outline disabled:active:scale-100"
          >
            −1 LOT
          </button>
        )}
        <button
          type="button"
          onClick={() => onExit(p)}
          disabled={locked}
          title="Sell the whole position at market"
          className="flex-1 md:flex-none h-10 md:h-8 min-w-[76px] px-3 rounded-md bg-tertiary text-on-tertiary text-[11px] font-extrabold tracking-wide shadow-sm hover:bg-tertiary/90 active:scale-95 transition disabled:bg-surface-container-high disabled:text-outline disabled:shadow-none disabled:active:scale-100 inline-flex items-center justify-center gap-1"
        >
          {busy || p.exiting ? <LoaderCircle size={13} className="animate-spin" /> : 'EXIT'}
        </button>
      </div>
      )}
    </div>
  );
}

/** Everything held right now — manual scalps first — each with a one-tap exit. */
export function ScalpPositions({ positions, busyIds, nearClose, onExit, onSellLot }: {
  positions: ExecutePosition[];
  busyIds: Record<string, boolean>;
  nearClose: boolean;
  onExit: (p: ExecutePosition) => void;
  onSellLot: (p: ExecutePosition) => void;
}) {
  const sorted = [...positions].sort((a, b) => Number(b.manual) - Number(a.manual));
  const openPnl = positions.reduce((acc, p) => acc + (p.pnl ?? 0), 0);
  return (
    <section className="rounded-xl border border-outline-variant bg-surface-container-lowest shadow-sm overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 bg-surface-container-low border-b border-outline-variant">
        <h2 className="text-[11px] font-extrabold uppercase tracking-wider text-on-surface-variant">
          Open positions <span className="ml-1 text-on-surface">{positions.length}</span>
        </h2>
        {positions.length > 0 && (
          <span className={`font-mono-code text-xs font-bold tabular-nums ${openPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
            {fmtPnl(openPnl)}
          </span>
        )}
      </div>
      {sorted.length === 0 ? (
        <div className="px-4 py-5 text-center text-sm text-on-surface-variant">
          Flat. Tap <span className="font-bold text-secondary">BUY</span> on a strike below to open a scalp.
        </div>
      ) : (
        sorted.map((p) => (
          <PositionRow key={p.id} p={p} busy={Boolean(busyIds[p.id])} nearClose={nearClose} onExit={onExit} onSellLot={onSellLot} />
        ))
      )}
    </section>
  );
}
