import { price, shortDate, signedPct, signedPrice, toneOf } from '../lib/format';
import type { InstrumentKind, InstrumentRow, Quote, SearchAnswer } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { Chip } from '../ui/Chip';

// The pieces a search result is drawn with, shared by the desktop popover
// and the phone sheet: the symbol with the typed fragment underlined, the
// exchange and kind chips, the price with its change flashing as it ticks,
// and the note under the list. The answer's shape is `searchModel.ts`.

/** `text` with the first occurrence of `fragment` (the query's first word,
 * case-insensitive) underlined. */
export function Highlight({ text, fragment }: { text: string; fragment: string }) {
  const needle = fragment.trim().split(/\s+/)[0] ?? '';
  const at = needle ? text.toLowerCase().indexOf(needle.toLowerCase()) : -1;
  if (at < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, at)}
      <span className="underline decoration-current decoration-1 underline-offset-2">{text.slice(at, at + needle.length)}</span>
      {text.slice(at + needle.length)}
    </>
  );
}

const KIND_CHIP: Partial<Record<InstrumentKind, string>> = { FUT: 'FUT', CE: 'CE', PE: 'PE' };

/** "NSE" and, on a derivative, "FUT" / "CE" / "PE" — outline chips. */
export function KindChips({ row }: { row: InstrumentRow }) {
  const kind = KIND_CHIP[row.kind];
  return (
    <>
      <Chip>{row.exchange}</Chip>
      {kind && <Chip>{kind}</Chip>}
    </>
  );
}

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;

/** The LTP and, under or beside it, the change — toned, flashing its tint
 * as it ticks. A dash until the hub has a price. */
export function PriceCell({ quote, identity, size = 'small', stacked = true, muted = false }: {
  quote: Quote | null;
  /** What the figure is (the row's key): a different key is never a tick. */
  identity: string;
  size?: 'small' | 'medium';
  /** The change under the price (rows) or after it (one line). */
  stacked?: boolean;
  /** The row is highlighted: the price takes the highlight's ink. */
  muted?: boolean;
}) {
  const flash = useTickFlash(quote?.ltp, true, identity);
  const num = size === 'medium' ? 'num-medium' : 'num-small';
  if (!quote) return <span className={`${num} text-fg-subtle`}>—</span>;
  const change = quote.change ?? (quote.close != null ? quote.ltp - quote.close : null);
  const pct = quote.per_change ?? (quote.close ? ((quote.ltp - quote.close) / quote.close) * 100 : null);
  const tone = TONE_TEXT[toneOf(change)];
  return (
    <span className={`flex shrink-0 ${stacked ? 'flex-col items-end' : 'items-baseline gap-1.5'}`}>
      <span key={flash.key} className={`-mx-1 rounded-4 px-1 ${num} ${muted ? 'text-current' : 'text-fg'} ${flash.className}`}>
        {price(quote.ltp)}
      </span>
      {change != null && (
        <span className={`num-small ${tone}`}>
          {signedPrice(change)}
          {pct != null && <span className="ml-1">{signedPct(pct)}</span>}
        </span>
      )}
    </span>
  );
}

/** "catalog from 7 Nov" while the catalog is not today's; "Closed · prices as
 * of 15:30" after hours — the line under a result list. Nothing when
 * neither applies. */
export function AnswerNote({ answer, closedAsOf }: { answer: SearchAnswer; closedAsOf: string | null }) {
  const stale = answer.catalog_day && answer.catalog_day !== answer.today;
  if (!stale && answer.market_open) return null;
  return (
    <>
      {stale && <span>catalog from {shortDate(answer.catalog_day!)}</span>}
      {!answer.market_open && (
        <span className="flex items-center gap-1.5">
          <Chip kind="neutral">Closed</Chip>
          {closedAsOf && <span>prices as of {closedAsOf.slice(11, 16)}</span>}
        </span>
      )}
    </>
  );
}
