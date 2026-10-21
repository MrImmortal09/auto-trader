import { price } from '../lib/format';
import type { TrendAnswer } from '../lib/types';
import { ToneChip } from '../ui/Chip';
import { biasWord, lakh, scoreRow, scoreTone, signedScore, windowLabel, type Basis, type ScoreSettings } from './model';

function ScoreChip({ overall }: { overall: number }) {
  return <ToneChip tone={scoreTone(overall)}>{`${signedScore(overall)} · ${biasWord(overall).toLowerCase()}`}</ToneChip>;
}

/**
 * "15-minute checks": each check of the day, newest first, scored the way
 * the engine scores it — on the strikes and basis chosen above.
 */
export function Checks({ trend, s, basis, window, desktop }: {
  trend: TrendAnswer | null;
  /** The engine's settings: interval and thresholds. */
  s: ScoreSettings;
  basis: Basis;
  window: number;
  desktop: boolean;
}) {
  const rows = trend?.rows ?? [];
  const differs = basis !== s.basis || window !== s.window;
  const side = basis === 'chg' ? 'ΔOI' : 'OI';
  return (
    <section className="rounded-12 bg-panel p-4">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h3 className="type-title text-fg">{s.interval}-minute checks</h3>
        <span className="type-caption text-fg-muted">
          The engine sizes a call on the last closed check · score = option signal + VWAP signal
        </span>
      </div>
      {differs && (
        <p className="mt-1 type-caption text-fg-muted">
          Scored here on {windowLabel(window)} and {basis === 'chg' ? 'change in OI' : 'total OI'}; the engine uses {windowLabel(s.window)} and{' '}
          {s.basis === 'chg' ? 'change in OI' : 'total OI'}.
        </p>
      )}
      {rows.length === 0 ? (
        <p className="py-8 text-center type-body text-fg-muted">No checks yet today. Snapshots are taken every minute from 09:15 to 15:30.</p>
      ) : desktop ? (
        <table className="mt-3 w-full table-fixed">
          <colgroup>
            <col className="w-[13%]" />
            <col className="w-[7%]" />
            <col className="w-[7%]" />
            <col className="w-[6%]" />
            <col className="w-[10%]" />
            <col className="w-[10%]" />
            <col />
            <col className="w-[15%]" />
          </colgroup>
          <thead>
            <tr className="border-b border-line type-caption text-fg-muted">
              <th className="py-2 text-left font-normal">Time</th>
              <th className="py-2 text-right font-normal">Calls {side}</th>
              <th className="py-2 text-right font-normal">Puts {side}</th>
              <th className="py-2 text-right font-normal">PCR</th>
              <th className="py-2 text-right font-normal">Futures</th>
              <th className="py-2 text-right font-normal">VWAP</th>
              <th />
              <th className="py-2 text-left font-normal">Score</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => {
              const sc = scoreRow(r, s, basis);
              return (
                <tr key={r.ts} className="border-b border-line last:border-0">
                  <td className="py-2.5 num-medium text-fg">
                    {r.time}
                    {r.live && <span className="ml-2 type-caption text-fg-muted">forming</span>}
                  </td>
                  <td className="py-2.5 text-right num-medium text-fg">{lakh(sc.call)}</td>
                  <td className="py-2.5 text-right num-medium text-fg">{lakh(sc.put)}</td>
                  <td className="py-2.5 text-right num-medium text-fg">{sc.ratio == null ? '—' : sc.ratio.toFixed(2)}</td>
                  <td className="py-2.5 text-right num-medium text-fg">{r.fut_ltp == null ? '—' : price(r.fut_ltp)}</td>
                  <td className="py-2.5 text-right num-medium text-fg">{r.fut_vwap == null ? '—' : price(r.fut_vwap)}</td>
                  <td />
                  <td className="py-2.5">
                    <ScoreChip overall={sc.overall} />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      ) : (
        <ul className="mt-2">
          {rows.map((r) => {
            const sc = scoreRow(r, s, basis);
            return (
              <li key={r.ts} className="border-b border-line py-2.5 last:border-0">
                <div className="flex items-center gap-3">
                  <span className="num-medium text-fg">{r.time}</span>
                  {r.live && <span className="type-caption text-fg-muted">forming</span>}
                  <span className="num-medium text-fg-muted">PCR {sc.ratio == null ? '—' : sc.ratio.toFixed(2)}</span>
                  <span className="flex-1" />
                  <ScoreChip overall={sc.overall} />
                </div>
                <div className="mt-1 type-caption text-fg-muted tnum">
                  Calls {lakh(sc.call)} · Puts {lakh(sc.put)} · Fut {r.fut_ltp == null ? '—' : price(r.fut_ltp)} · VWAP {r.fut_vwap == null ? '—' : price(r.fut_vwap)}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
