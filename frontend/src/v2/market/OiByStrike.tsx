import { Fragment } from 'react';
import { MINUS, level } from '../lib/format';
import type { OptionChainAnswer } from '../lib/types';
import { lakh, pcr, strikesAround, strikeSides, windowLabel, type Basis } from './model';

/** "7.2", "−0.4": a bar's lakh, no plus. */
function barValue(n: number): string {
  const v = n / 1e5;
  return `${v < -0.05 ? MINUS : ''}${Math.abs(v).toFixed(1)}`;
}

/**
 * "Change in OI by strike": calls to the left of each strike, puts to the
 * right, ATM boxed and a line where the index is. From Upstox's latest
 * chain poll.
 */
export function OiByStrike({ chain, basis, window, configured }: {
  chain: OptionChainAnswer | null;
  basis: Basis;
  window: number;
  configured: boolean;
}) {
  const rows = chain ? strikesAround(chain.strikes, chain.atm_strike, window) : [];
  const values = rows.map((s) => strikeSides(s, basis));
  const calls = values.reduce((sum, [c]) => sum + c, 0);
  const puts = values.reduce((sum, [, p]) => sum + p, 0);
  const ratio = pcr(calls, puts);
  const widest = Math.max(1, ...values.flatMap(([c, p]) => [Math.abs(c), Math.abs(p)]));
  const spot = chain?.spot ?? null;
  // The spot line goes above the first strike over the index.
  const spotAt = spot == null ? -1 : rows.findIndex((s) => s.strike > spot);

  return (
    <section className="flex min-w-0 flex-col rounded-12 bg-panel p-4">
      <h3 className="type-title text-fg">{basis === 'chg' ? 'Change in OI by strike' : 'Total OI by strike'}</h3>
      <p className="mt-0.5 type-caption text-fg-muted">
        {windowLabel(window)} · in lakh{basis === 'chg' ? ', since yesterday' : ''}
      </p>
      {rows.length === 0 ? (
        <p className="py-8 text-center type-body text-fg-muted">
          {configured ? 'The option chain shows once Upstox has been polled, in market hours.' : "Upstox isn't set up on the server."}
        </p>
      ) : (
        <>
          <div className="mt-4 flex items-center gap-4 type-caption text-fg-muted">
            <span className="flex items-center gap-1.5">
              <span className="size-2.5 rounded-[2px] bg-fg-muted" /> Calls {lakh(calls)}
            </span>
            <span className="flex items-center gap-1.5">
              <span className="size-2.5 rounded-[2px] bg-chart-indicator" /> Puts {lakh(puts)}
            </span>
            <span className="flex-1" />
            <span className="type-body-strong text-fg tnum">PCR {ratio == null ? 'n/a' : ratio.toFixed(2)}</span>
          </div>
          <div className={`mt-3 grid grid-cols-[1fr_72px_1fr] items-center gap-y-1.5 ${window === 0 ? 'max-h-[420px] overflow-y-auto pr-1' : ''}`}>
            {rows.map((s, i) => {
              const [c, p] = values[i];
              const atm = s.strike === chain?.atm_strike;
              return (
                <Fragment key={s.strike}>
                  {i === spotAt && spot != null && (
                    <div className="col-span-3 flex items-center gap-2 py-0.5" aria-label={`Spot ${level(spot)}`}>
                      <span className="h-px flex-1 border-t border-dashed border-line-strong" />
                      <span className="type-label text-fg tnum">Spot {level(spot)}</span>
                      <span className="h-px flex-1 border-t border-dashed border-line-strong" />
                    </div>
                  )}
                  <div className="flex items-center justify-end gap-2">
                    <span className="num-small text-fg-muted">{barValue(c)}</span>
                    <span className={`h-2.5 rounded-l-[2px] bg-fg-muted ${c < 0 ? 'opacity-40' : ''}`} style={{ width: `calc((100% - 40px) * ${(Math.abs(c) / widest).toFixed(4)})` }} />
                  </div>
                  <div className="flex justify-center">
                    <span className={`rounded-4 px-1.5 num-small ${atm ? 'border border-line-strong text-fg' : 'text-fg-muted'}`}>{s.strike}</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className={`h-2.5 rounded-r-[2px] bg-chart-indicator ${p < 0 ? 'opacity-40' : ''}`} style={{ width: `calc((100% - 40px) * ${(Math.abs(p) / widest).toFixed(4)})` }} />
                    <span className="num-small text-fg-muted">{barValue(p)}</span>
                  </div>
                </Fragment>
              );
            })}
          </div>
        </>
      )}
    </section>
  );
}
