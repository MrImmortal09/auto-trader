import { useEffect, useState } from 'react';
import { getIndicator, getMarketLive, getOptionChain, getSettings, getTrend } from '../lib/api';
import { usePoll } from '../lib/usePoll';
import type { IndicatorReading, MarketIndex, TrendAnswer, TrendRow } from '../lib/types';
import { istMs, type Tf } from '../scalp/model';
import { Segment } from '../ui/Segment';
import { BiasCard } from './BiasCard';
import { Checks } from './Checks';
import { FuturesChart, type MarketLayers } from './FuturesChart';
import { MARKET_INDICES, nextCheck, pcr, scoreSettings, sides, WINDOWS, type Basis } from './model';
import { OiByStrike } from './OiByStrike';

const STORE = 'v2.market';

interface Saved {
  index: MarketIndex;
  tf: Tf;
  layers: MarketLayers;
}

const DEFAULTS: Saved = { index: 'NIFTY', tf: '5m', layers: { vwap: true, pcr: true } };

function loadSaved(): Saved {
  try {
    const raw = JSON.parse(localStorage.getItem(STORE) ?? 'null') as Partial<Saved> | null;
    if (!raw) return DEFAULTS;
    return {
      index: MARKET_INDICES.some((i) => i.id === raw.index) ? (raw.index as MarketIndex) : DEFAULTS.index,
      tf: raw.tf === '1m' || raw.tf === '15m' ? raw.tf : '5m',
      layers: { vwap: raw.layers?.vwap !== false, pcr: raw.layers?.pcr !== false },
    };
  } catch {
    return DEFAULTS;
  }
}

/** The check a reading was scored on, for its futures and VWAP. */
function checkOf(trend: TrendAnswer | null, reading: IndicatorReading | null): TrendRow | undefined {
  if (!trend || !reading) return undefined;
  return trend.rows.find((r) => r.ts === reading.ts) ?? trend.rows.find((r) => !r.live && r.time === reading.check_time);
}

const BASES: { value: Basis; label: string }[] = [
  { value: 'chg', label: 'Change in OI' },
  { value: 'total', label: 'Total OI' },
];

/**
 * Market: each index's VWAP + PCR reading as the engine sees it, the future
 * with its VWAP and PCR, open interest by strike, and the day's checks. All
 * read-only, from Upstox through `/api/market/*`.
 */
export function Market({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const [saved, setSaved] = useState(loadSaved);
  // The strikes and basis looked at; until changed, the engine's own.
  const [view, setView] = useState<{ window: number; basis: Basis } | null>(null);

  useEffect(() => {
    try {
      localStorage.setItem(STORE, JSON.stringify(saved));
    } catch {
      // Private mode: the choice lasts the visit.
    }
  }, [saved]);

  const index = saved.index;
  const settings = usePoll(() => getSettings(serverBase), 60_000, serverBase);
  const s = scoreSettings(settings.data);
  const window = view?.window ?? s.window;
  const basis = view?.basis ?? s.basis;
  const ready = settings.data != null;

  const indicator = usePoll(() => getIndicator(serverBase), 15_000, serverBase);
  const live = usePoll(() => getMarketLive(serverBase), 30_000, serverBase);
  const chain = usePoll(() => getOptionChain(serverBase, index), 30_000, `${serverBase}|${index}`);
  const table = usePoll(() => getTrend(serverBase, index, s.interval, window), 30_000, `${serverBase}|${index}|${s.interval}|${window}`, ready);
  const fine = usePoll(() => getTrend(serverBase, index, 5, window), 30_000, `${serverBase}|${index}|5|${window}`, ready);
  // Each card's own checks, on the engine's strikes — phone shows one card.
  const cardKey = (i: MarketIndex) => `${serverBase}|${i}|${s.interval}|${s.window}`;
  const cardTrend: Record<MarketIndex, TrendAnswer | null> = {
    NIFTY: usePoll(() => getTrend(serverBase, 'NIFTY', s.interval, s.window), 30_000, cardKey('NIFTY'), ready && (desktop || index === 'NIFTY')).data,
    BANKNIFTY: usePoll(() => getTrend(serverBase, 'BANKNIFTY', s.interval, s.window), 30_000, cardKey('BANKNIFTY'), ready && (desktop || index === 'BANKNIFTY')).data,
    SENSEX: usePoll(() => getTrend(serverBase, 'SENSEX', s.interval, s.window), 30_000, cardKey('SENSEX'), ready && (desktop || index === 'SENSEX')).data,
  };

  const configured = (indicator.data?.configured ?? live.data?.configured) !== false;
  const sizingOn = indicator.data?.enabled === true;
  const answerOf = (i: MarketIndex) => indicator.data?.indices.find((x) => x.index === i);
  const reading = answerOf(index)?.reading ?? null;
  const next = reading ? nextCheck(reading.check_time, s.interval) : null;
  const caption = !configured
    ? "Upstox isn't set up on the server, so there's no market data yet."
    : reading
      ? `Upstox option chain · checked ${reading.check_time}${next ? ` · next check ${next}` : ''}`
      : 'Upstox option chain · no closed check yet';

  const checks = (fine.data?.rows ?? [])
    .map((r) => {
      const [call, put] = sides(r, basis);
      return { t: istMs(r.ts), v: pcr(call, put) };
    })
    .filter((p): p is { t: number; v: number } => p.v != null && Number.isFinite(p.t));

  const setIndex = (i: MarketIndex) => setSaved((x) => ({ ...x, index: i }));
  const card = (i: MarketIndex, label: string) => (
    <BiasCard
      key={i}
      label={label}
      answer={answerOf(i)}
      check={checkOf(cardTrend[i], answerOf(i)?.reading ?? null)}
      s={s}
      sizingOn={sizingOn}
      selected={desktop && i === index}
      onSelect={desktop ? () => setIndex(i) : undefined}
    />
  );
  const chart = (
    <FuturesChart
      serverBase={serverBase}
      index={index}
      onIndex={setIndex}
      tf={saved.tf}
      onTf={(tf) => setSaved((x) => ({ ...x, tf }))}
      checks={checks}
      buyAbove={s.pcrBuyAbove}
      sellBelow={s.pcrSellBelow}
      layers={saved.layers}
      onLayers={(layers) => setSaved((x) => ({ ...x, layers }))}
      compact={!desktop}
    />
  );
  const strikes = (
    <Segment label="Strikes" value={String(window)} onChange={(v) => setView({ window: Number(v), basis })} options={WINDOWS} />
  );
  const bases = <Segment label="Basis" value={basis} onChange={(b) => setView({ window, basis: b })} options={BASES} />;
  const oi = <OiByStrike chain={chain.data} basis={basis} window={window} configured={configured} />;
  const table15 = <Checks trend={table.data} s={s} basis={basis} window={window} desktop={desktop} />;

  if (!desktop) {
    return (
      <div className="flex flex-col gap-3 p-4">
        <Segment label="Index" size="lg" stretch value={index} onChange={setIndex} options={MARKET_INDICES.map((i) => ({ value: i.id, label: i.label }))} />
        {!configured && <p className="rounded-12 bg-panel p-4 type-body text-fg-muted">{caption}</p>}
        {card(index, MARKET_INDICES.find((i) => i.id === index)!.label)}
        <div className="h-[300px]">{chart}</div>
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2 overflow-x-auto">{strikes}</div>
          <div className="flex items-center gap-2">{bases}</div>
        </div>
        {oi}
        {table15}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4 p-6">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Segment label="Index" value={index} onChange={setIndex} options={MARKET_INDICES.map((i) => ({ value: i.id, label: i.label }))} />
        <span className="type-caption text-fg-muted">{caption}</span>
        <span className="flex-1" />
        <span className="type-label text-fg-muted">Strikes</span>
        {strikes}
        <span className="ml-2 type-label text-fg-muted">Basis</span>
        {bases}
      </div>
      <div className="grid grid-cols-3 gap-4">{MARKET_INDICES.map((i) => card(i.id, i.label))}</div>
      <div className="grid grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-4">
        <div className="h-[380px] min-w-0">{chart}</div>
        {oi}
      </div>
      {table15}
    </div>
  );
}
