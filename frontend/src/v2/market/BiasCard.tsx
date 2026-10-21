import type { IndicatorAnswer, TrendRow } from '../lib/types';
import { ToneChip } from '../ui/Chip';
import { biasChip, futuresLine, pcrLine, signalWord, verdictLine, type ScoreSettings } from './model';

type IndexAnswer = IndicatorAnswer['indices'][number];

const METER = [-2, -1, 0, 1, 2];

/** The −2 … +2 meter: from the middle out to the score, in its colour. */
function Meter({ overall, known }: { overall: number; known: boolean }) {
  return (
    <div>
      <div className="flex gap-1.5" aria-hidden>
        {METER.map((m) => {
          const lit = known && (overall === 0 ? m === 0 : overall > 0 ? m > 0 && m <= overall : m < 0 && m >= overall);
          const colour = !lit ? 'bg-raised' : overall > 0 ? 'bg-profit' : overall < 0 ? 'bg-loss' : 'bg-fg-muted';
          return <span key={m} className={`h-1 flex-1 rounded-full ${colour}`} />;
        })}
      </div>
      <div className="mt-1.5 flex justify-between type-caption text-fg-muted">
        <span>−2 bearish</span>
        <span>+2 bullish</span>
      </div>
    </div>
  );
}

function SignalRow({ title, line, chip }: { title: string; line: string; chip: { text: string; tone: 'profit' | 'loss' | 'neutral' } }) {
  return (
    <div className="flex items-center gap-3 border-t border-line py-3">
      <div className="min-w-0 flex-1">
        <div className="type-caption text-fg-muted">{title}</div>
        <div className="mt-0.5 truncate type-body text-fg tnum">{line}</div>
      </div>
      <ToneChip tone={chip.tone}>{chip.text}</ToneChip>
    </div>
  );
}

const VERDICT_TONE = { strong: 'text-profit', plain: 'text-fg', muted: 'text-fg-muted' } as const;

/**
 * Figma "Bias card": an index's last closed check — the engine's own
 * reading — its two signals, and what a CE or PE call arriving now would get.
 */
export function BiasCard({ label, answer, check, s, sizingOn, selected, onSelect }: {
  label: string;
  answer: IndexAnswer | undefined;
  /** The check's snapshot, for its futures and VWAP. */
  check: TrendRow | undefined;
  s: ScoreSettings;
  sizingOn: boolean;
  selected: boolean;
  /** Desktop: choosing a card chooses its index. */
  onSelect?: () => void;
}) {
  const reading = answer?.reading ?? null;
  const chip = biasChip(reading);
  const overall = reading && !reading.opening ? reading.score.overall : 0;
  const body = (
    <>
      <div className="flex items-center gap-2">
        <h3 className="type-title text-fg">{label}</h3>
        <span className="flex-1" />
        <ToneChip tone={chip.tone}>{chip.text}</ToneChip>
      </div>
      <div className="mt-3 mb-3">
        <Meter overall={overall} known={reading != null} />
      </div>
      {reading ? (
        <>
          <SignalRow
            title={`Option signal · PCR on ${s.basis === 'chg' ? 'change in OI' : 'total OI'}`}
            line={pcrLine(reading.score, s)}
            chip={{ text: signalWord(reading.score.option, 'Neutral'), tone: reading.score.option > 0 ? 'profit' : reading.score.option < 0 ? 'loss' : 'neutral' }}
          />
          <SignalRow
            title="VWAP signal · futures vs VWAP"
            line={futuresLine(check?.fut_ltp ?? null, check?.fut_vwap ?? null)}
            chip={{ text: signalWord(reading.score.vwap, 'In band'), tone: reading.score.vwap > 0 ? 'profit' : reading.score.vwap < 0 ? 'loss' : 'neutral' }}
          />
        </>
      ) : (
        <p className="border-t border-line py-3 type-caption text-fg-muted">
          No closed check in the last few minutes, so a call is sized as neutral.
        </p>
      )}
      <div className="border-t border-line pt-3">
        <div className="type-caption text-fg-muted">If a Telegram call arrives now</div>
        {answer ? (
          (['CE', 'PE'] as const).map((side) => {
            const v = verdictLine(side === 'CE' ? answer.ce : answer.pe, side, reading, sizingOn);
            return (
              <div key={side} className="mt-2 flex gap-4 type-body">
                <span className="w-14 shrink-0 type-body-strong text-fg">{side} call</span>
                <span className={`min-w-0 ${VERDICT_TONE[v.tone]}`}>{v.text}</span>
              </div>
            );
          })
        ) : (
          <div className="mt-2 type-body text-fg-muted">—</div>
        )}
      </div>
    </>
  );
  const frame = `rounded-12 border bg-panel p-4 text-left ${selected ? 'border-line-strong' : 'border-transparent'}`;
  if (!onSelect) return <section className={frame}>{body}</section>;
  return (
    <section
      role="button"
      tabIndex={0}
      aria-pressed={selected}
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect();
        }
      }}
      className={`${frame} cursor-pointer transition-colors ${selected ? '' : 'hover:border-line'}`}
    >
      {body}
    </section>
  );
}
