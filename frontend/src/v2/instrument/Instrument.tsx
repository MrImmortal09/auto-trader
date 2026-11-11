import { useEffect } from 'react';
import { ArrowLeft, ChartLine, Plus } from 'lucide-react';
import { getInstrument } from '../lib/api';
import { dayLabel, price, shortDate, signedPct, signedPrice, todayIst, toneOf, whole } from '../lib/format';
import { chartOf, tradeIndexOf, type TradeIntent } from '../lib/openInstrument';
import type { Instrument as InstrumentAnswer, InstrumentKind, Quote } from '../lib/types';
import { usePoll } from '../lib/usePoll';
import { seedQuotes, useQuotes } from '../lib/useQuotes';
import { useTickFlash } from '../lib/useTickFlash';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Stat } from '../ui/Stat';

// The instrument page (`/v2/i/<key>`, plan Phase 1): one instrument's price
// as the hero, its day — open, high, low, previous close, volume, the
// 52-week range — its lot and tick size and nearest expiry, and where it
// can go: the chart and the ladder on the Trade screen for the six indices
// and their options now; stocks' Chart and Chain tabs light up in Phase 3,
// "+ Watchlist" in Phase 2. Read-only: nothing here places an order.

const KIND_WORD: Record<InstrumentKind, string> = { INDEX: 'Index', EQ: 'Stock', FUT: 'Future', CE: 'Call', PE: 'Put' };
const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;

/** "RELIANCE", "NIFTY 25600 CE", "RELIANCE NOV FUT". */
function titleOf(i: InstrumentAnswer): string {
  if (i.kind === 'EQ' || i.kind === 'INDEX') return i.symbol;
  const month = i.expiry ? shortDate(i.expiry).split(' ')[1].toUpperCase() : '';
  if (i.kind === 'FUT') return `${i.underlying} ${month} FUT`;
  return `${i.underlying} ${i.strike != null ? price(i.strike).replace(/\.00$/, '') : ''} ${i.kind}`;
}

/** The price's provenance: "Live · 11:42:17", "Close · as of 15:30, Fri 7 Nov", "Upstox · 11:42:17". */
function sourceLine(q: Quote): string {
  if (q.source === 'close') return `Close · as of ${q.as_of ? `${q.as_of.slice(11, 16)}, ${dayLabel(q.as_of)}` : 'the last session'}`;
  const at = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }).format(new Date(q.at));
  return `${q.source === 'upstox' ? 'Upstox' : 'Live'} · ${at}`;
}

/** The price and its change, flashing as it ticks: the page's hero, or a
 * derivative's underlying in a smaller size. */
function Figure({ quote, identity, hero }: { quote: Quote | null; identity: string; hero: boolean }) {
  const flash = useTickFlash(quote?.ltp, true, identity);
  if (!quote) return <span className={`${hero ? 'num-hero' : 'num-large'} text-fg-subtle`}>—</span>;
  const change = quote.change ?? (quote.close != null ? quote.ltp - quote.close : null);
  const pct = quote.per_change ?? (quote.close ? ((quote.ltp - quote.close) / quote.close) * 100 : null);
  return (
    <span className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
      <span key={flash.key} className={`-mx-1 rounded-4 px-1 ${hero ? 'num-hero' : 'num-large'} text-fg ${flash.className}`}>
        {price(quote.ltp)}
      </span>
      {change != null && (
        <span className={`${hero ? 'num-medium' : 'num-small'} ${TONE_TEXT[toneOf(change)]}`}>
          {signedPrice(change)}
          {pct != null && <span className="ml-1.5">({signedPct(pct)})</span>}
        </span>
      )}
    </span>
  );
}

/** One of the page's tabs: Overview (this page), Chart and Chain — the last
 * two open the Trade screen for an index family, and say "coming" for a
 * stock's until Phase 3. */
function Tab({ selected = false, coming = false, onClick, children }: { selected?: boolean; coming?: boolean; onClick?: () => void; children: string }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={selected}
      disabled={coming}
      title={coming ? `${children} for stocks comes in Phase 3` : undefined}
      onClick={onClick}
      className={`-mb-px flex h-11 items-center gap-2 border-b-2 px-3 type-body transition-colors duration-120 disabled:cursor-not-allowed ${
        selected ? 'border-accent text-fg' : 'border-transparent text-fg-muted hover:text-fg disabled:hover:text-fg-muted'
      }`}
    >
      {children}
      {coming && <Chip kind="neutral">coming</Chip>}
    </button>
  );
}

export function Instrument({ serverBase, desktop, id, onBack, onOpenTrade }: {
  serverBase: string;
  desktop: boolean;
  /** The catalog id: `nse_cm:2885`. */
  id: string;
  onBack: () => void;
  /** Open the Trade screen on a chart and/or the ladder. */
  onOpenTrade: (intent: TradeIntent) => void;
}) {
  // The facts (lot, tick, expiries) change with the daily catalog; the
  // prices come from the shared quotes poll.
  const page = usePoll(() => getInstrument(serverBase, id), 30_000, `${serverBase}|${id}`);
  const data = page.data;
  useEffect(() => {
    if (data) seedQuotes([data.quote, data.spot]);
  }, [data]);
  const quotes = useQuotes(serverBase, [data?.ws_key, data?.underlying_key !== data?.ws_key ? data?.underlying_key : null]);

  const pad = desktop ? 'px-6 py-5' : 'px-4 py-4';
  if (!data) {
    return (
      <div className={`flex flex-col gap-4 ${pad}`}>
        <Button size={desktop ? 'md' : 'lg'} kind="quiet" icon={<ArrowLeft size={desktop ? 14 : 18} />} onClick={onBack} className="self-start">
          Back
        </Button>
        <p className={`type-body ${page.error ? 'text-loss' : 'text-fg-subtle'}`}>{page.error ? `Couldn't load the instrument: ${page.error}` : 'Loading…'}</p>
      </div>
    );
  }

  const quote = quotes.get(data.ws_key) ?? data.quote;
  const spot = data.underlying_key && data.underlying_key !== data.ws_key ? quotes.get(data.underlying_key) ?? data.spot : null;
  const derivative = data.kind === 'FUT' || data.kind === 'CE' || data.kind === 'PE';
  const index = tradeIndexOf(data);
  const chart = chartOf(data);
  const ladder = index ? { index, expiry: derivative ? data.expiry : null } : null;
  const closed = quote?.source === 'close';
  const yearRange = quote?.year_low != null && quote?.year_high != null ? `${price(quote.year_low)} – ${price(quote.year_high)}` : null;
  const catalogNote = data.catalog_day && data.catalog_day !== todayIst() ? `catalog from ${shortDate(data.catalog_day)}` : null;
  const dash = '—';
  const stat = (v: number | null | undefined) => (v == null ? dash : price(v));

  return (
    <div className={`mx-auto flex w-full max-w-[960px] flex-col gap-5 ${pad}`}>
      <div className="flex items-start gap-3">
        <Button size={desktop ? 'md' : 'lg'} kind="quiet" icon={<ArrowLeft size={desktop ? 14 : 18} />} onClick={onBack} aria-label="Back" className={desktop ? 'px-2' : 'px-3'} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="type-title text-fg">{titleOf(data)}</h1>
            <Chip>{data.exchange}</Chip>
            <Chip>{KIND_WORD[data.kind]}</Chip>
            {data.kind === 'EQ' && data.has_fo && <Chip>F&O</Chip>}
            {closed && <Chip kind="neutral">Closed</Chip>}
          </div>
          <p className="mt-0.5 truncate type-body text-fg-muted">
            {data.name}
            {derivative && data.expiry && <span className="text-fg-subtle"> · expires {dayLabel(data.expiry)}</span>}
          </p>
        </div>
        {desktop && (
          <div className="flex shrink-0 items-center gap-2">
            <Button size="md" kind="accent" icon={<ChartLine size={14} />} disabled={!chart} title={chart ? undefined : 'Charts for stocks come in Phase 3'} onClick={() => chart && onOpenTrade({ chart, ladder: ladder ?? undefined })}>
              Open chart
            </Button>
            <Button size="md" kind="neutral" icon={<Plus size={14} />} disabled title="Watchlists come in Phase 2">
              Watchlist
            </Button>
          </div>
        )}
      </div>

      <div className="flex flex-col gap-1">
        <Figure quote={quote} identity={data.ws_key} hero />
        <span className="type-caption text-fg-subtle tnum">
          {quote ? sourceLine(quote) : 'Waiting for a price…'}
          {catalogNote && ` · ${catalogNote}`}
        </span>
      </div>

      {!desktop && (
        <div className="flex gap-2">
          <Button size="lg" kind="accent" icon={<ChartLine size={18} />} disabled={!chart} title={chart ? undefined : 'Charts for stocks come in Phase 3'} onClick={() => chart && onOpenTrade({ chart, ladder: ladder ?? undefined })} className="flex-1">
            Open chart
          </Button>
          <Button size="lg" kind="neutral" icon={<Plus size={18} />} disabled title="Watchlists come in Phase 2" className="flex-1">
            Watchlist
          </Button>
        </div>
      )}

      <div role="tablist" aria-label="Instrument" className="flex border-b border-line">
        <Tab selected>Overview</Tab>
        <Tab coming={!chart} onClick={() => chart && onOpenTrade({ chart, ladder: ladder ?? undefined })}>
          Chart
        </Tab>
        <Tab coming={!ladder} onClick={() => ladder && onOpenTrade({ ladder })}>
          Chain
        </Tab>
      </div>

      <section aria-label="Today" className="rounded-12 border border-line bg-panel p-4">
        <div className="grid grid-cols-2 gap-x-8 gap-y-4 sm:grid-cols-3 lg:grid-cols-4">
          <Stat label="Open" size="medium" value={stat(quote?.open)} />
          <Stat label="High" size="medium" value={stat(quote?.high)} />
          <Stat label="Low" size="medium" value={stat(quote?.low)} />
          <Stat label="Prev close" size="medium" value={stat(quote?.close)} />
          {data.kind !== 'INDEX' && <Stat label="Volume" size="medium" value={quote?.volume != null ? whole(quote.volume) : dash} />}
          {derivative && <Stat label="Open interest" size="medium" value={quote?.oi != null ? whole(quote.oi) : dash} />}
          {yearRange && <Stat label="52-week range" size="medium" value={yearRange} />}
        </div>
      </section>

      <section aria-label="Contract" className="rounded-12 border border-line bg-panel p-4">
        <div className="grid grid-cols-2 gap-x-8 gap-y-4 sm:grid-cols-3 lg:grid-cols-4">
          {derivative && data.expiry && <Stat label="Expiry" size="medium" value={dayLabel(data.expiry)} />}
          {data.strike != null && <Stat label="Strike" size="medium" value={price(data.strike).replace(/\.00$/, '')} />}
          <Stat label="Lot size" size="medium" value={whole(data.lot_size)} sub={data.lot_size === 1 ? 'one share' : undefined} />
          <Stat label="Tick size" size="medium" value={price(data.tick_size)} />
          {data.nearest_expiry && <Stat label="Nearest expiry" size="medium" value={dayLabel(data.nearest_expiry)} sub={data.kind === 'INDEX' ? 'options and futures' : 'F&O listed'} />}
          {data.kind === 'EQ' && !data.has_fo && <Stat label="F&O" size="medium" value="Not listed" />}
          {data.isin && <Stat label="ISIN" size="medium" value={<span className="type-mono text-fg">{data.isin}</span>} />}
        </div>
      </section>

      {derivative && (
        <section aria-label="Underlying" className="rounded-12 border border-line bg-panel p-4">
          <div className="flex flex-wrap items-end justify-between gap-3">
            <div className="flex flex-col gap-1">
              <span className="type-eyebrow text-fg-muted">Underlying</span>
              <span className="type-body-strong text-fg">{data.underlying}</span>
            </div>
            {data.underlying_key ? <Figure quote={spot} identity={data.underlying_key} hero={false} /> : <span className="type-caption text-fg-subtle">No cash or index row</span>}
          </div>
        </section>
      )}
    </div>
  );
}
