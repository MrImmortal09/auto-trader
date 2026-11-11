import { price, signedPct, signedPrice, toneOf } from '../lib/format';
import { useTickFlash } from '../lib/useTickFlash';
import { hasVolume, symbolName, type ChartSym, type DaySummary } from './model';

// The line under a chart's header (plan Phase 3): the charted instrument's
// last price, the day's change, its open, high and low, and its volume —
// from the bars the chart drew, so it is right for a stock, a future or an
// option as much as for an index. An index's figures come from the ladder
// stream where it has them (its value ticks faster than its bars).

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;

/** "8.42 L", "1.20 Cr", "31,250" — a traded volume, Indian style. */
function compactCount(n: number): string {
  if (n >= 1e7) return `${(n / 1e7).toFixed(2)} Cr`;
  if (n >= 1e5) return `${(n / 1e5).toFixed(2)} L`;
  return new Intl.NumberFormat('en-IN', { maximumFractionDigits: 0 }).format(n);
}

/** The LTP, flashing its tint as it ticks. */
function Last({ value, identity }: { value: number | null; identity: string }) {
  const flash = useTickFlash(value, true, identity);
  return (
    <span key={flash.key} className={`-mx-1 rounded-4 px-1 num-small text-fg ${flash.className}`}>
      {value == null ? '—' : price(value)}
    </span>
  );
}

export function QuoteStrip({ sym, day, spot = null, changePct = null }: {
  sym: ChartSym;
  /** The instrument's day from its bars; null until they load. */
  day: DaySummary | null;
  /** An index's value from the ladder stream, which ticks ahead of its bars. */
  spot?: number | null;
  /** An index's change today from the stream, as a percent. */
  changePct?: number | null;
}) {
  const last = spot ?? day?.close ?? null;
  const prev = day?.prevClose ?? null;
  const points = last != null && prev != null && prev > 0 ? last - prev : null;
  const pct = changePct ?? (points != null && prev != null ? (points / prev) * 100 : null);
  const tone = TONE_TEXT[toneOf(points ?? pct)];
  // The strip is its own container (the widths below are its content box,
  // 24 px inside the chart): a narrow chart — two or three side by side, a
  // phone — drops the day's points, then the volume, then the open, rather
  // than run off the edge.
  const cell = (label: string, value: string, extra = 'flex') => (
    <span className={`shrink-0 items-baseline gap-1 ${extra}`}>
      <span className="type-caption text-fg-muted">{label}</span>
      <span className="num-small text-fg tnum">{value}</span>
    </span>
  );
  return (
    <div aria-label="Quote" className="@container flex h-6 shrink-0 items-center gap-x-2 overflow-hidden border-b border-line px-3 whitespace-nowrap">
      <Last value={last} identity={symbolName(sym)} />
      <span className={`shrink-0 num-small ${tone}`}>
        {points != null && <span className="hidden @[34rem]:inline">{signedPrice(points)} </span>}
        {pct == null ? '—' : points != null ? <span className="@[34rem]:before:content-['('] @[34rem]:after:content-[')']">{signedPct(pct)}</span> : signedPct(pct)}
      </span>
      <span className="min-w-0 flex-1" />
      {cell('O', day ? price(day.open) : '—', 'hidden @[25rem]:flex')}
      {cell('H', day ? price(day.high) : '—')}
      {cell('L', day ? price(day.low) : '—')}
      {hasVolume(sym) && cell('Vol', day?.volume != null ? compactCount(day.volume) : '—', 'hidden @[29rem]:flex')}
    </div>
  );
}
