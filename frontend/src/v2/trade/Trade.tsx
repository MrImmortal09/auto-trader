import { useCallback, useEffect, useMemo, useState } from 'react';
import { ChartCandlestick, ChevronDown, Columns2, Columns3, Square } from 'lucide-react';
import type { ChartLevel, ExecutePosition } from '../../types';
import { useExecuteStream } from '../../hooks/useExecuteStream';
import { buyBlockedReason, contractLabel, INDEX_ORDER, MAX_LOTS, readStored, writeStored } from '../../lib/execute';
import { executeBuy, executeExitAll, exitPosition, getChain, getExpiries, getFlags, getMarks, sellQty, setChartLevels, type Sent } from '../lib/api';
import { price, signedPct, toneOf, todayIst } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { ChainAnswer, ExpiriesAnswer } from '../lib/types';
import { useGuard } from '../shell/useGuard';
import { Segment } from '../ui/Segment';
import { useToast } from '../ui/toast';
import { C, type OrderLineData } from './chartSetup';
import { ChartPanel } from './ChartPanel';
import { CrosshairBus } from './bus';
import { ExpiryPicker } from './ExpiryPicker';
import type { Placing } from './KChart';
import { Ladder } from './Ladder';
import { chartLines, chartMarkers, DEFAULT_LAYERS, deltaFor, firstBuy, type LevelActions, type LevelKind, type Layers } from './lines';
import { approxMoney, CHART_LIBS, istClock, levelCrossed, levelPnl, optionOf, sameSym, symbolLabel, toTick, type ChartLib, type ChartSym, type Tf } from './model';
import { PositionSheet, ScalpBar, ScalpPositionsPanel, type PositionHandlers } from './Positions';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

interface Slot {
  sym: ChartSym;
  tf: Tf;
  layers: Layers;
}

interface Saved {
  layout: 1 | 2 | 3;
  slots: Slot[];
  index: string;
  lots: Record<string, number>;
  /** Phone: the chart folded away, the whole ladder shown instead. */
  chartHidden: boolean;
  /** The ladder's expiry per index, `YYYY-MM-DD`; missing means the nearest. */
  expiry: Record<string, string | null>;
  /** Which library draws every chart. */
  chartLib: ChartLib;
}

const STORE_KEY = 'v2.trade';
/** The screen's name before it became Trade; read once, for its saved layout. */
const OLD_STORE_KEY = 'v2.scalp';
const indexSlot = (index: string): Slot => ({ sym: { kind: 'index', index }, tf: '5m', layers: DEFAULT_LAYERS });
const DEFAULTS: Saved = { layout: 2, slots: [indexSlot('NIFTY'), indexSlot('BANKNIFTY'), indexSlot('SENSEX')], index: 'NIFTY', lots: {}, chartHidden: false, expiry: {}, chartLib: 'kline' };

/** The saved layout, with any option that has since expired back on its
 * index, and any chosen expiry that has passed back to the nearest. */
function loadSaved(): Saved {
  const saved = readStored<Partial<Saved>>(STORE_KEY, readStored<Partial<Saved>>(OLD_STORE_KEY, {}));
  const today = todayIst();
  const slots = DEFAULTS.slots.map((d, i) => {
    const s = saved.slots?.[i];
    if (!s?.sym) return d;
    const sym = s.sym.kind === 'option' && s.sym.expiry < today ? { kind: 'index' as const, index: s.sym.index } : s.sym;
    return { sym, tf: s.tf ?? '5m', layers: { ...DEFAULT_LAYERS, ...s.layers } };
  });
  const expiry = Object.fromEntries(Object.entries(saved.expiry ?? {}).filter(([index, date]) => INDEX_ORDER.includes(index) && typeof date === 'string' && date >= today));
  return {
    layout: saved.layout === 1 || saved.layout === 3 ? saved.layout : 2,
    slots,
    index: INDEX_ORDER.includes(saved.index ?? '') ? saved.index! : 'NIFTY',
    lots: saved.lots ?? {},
    chartHidden: saved.chartHidden === true,
    expiry,
    chartLib: saved.chartLib === 'vela' ? 'vela' : 'kline',
  };
}

/** A level set or cleared here, shown until the stream has it. */
interface Pending {
  level: ChartLevel | null;
  at: number;
}

const PENDING_FOR_MS = 3_000;

/** A scalp with the levels just set or cleared from here applied. */
function withPending(p: ExecutePosition, pending: Record<string, Pending>, now: number): ExecutePosition {
  const pick = (kind: LevelKind) => {
    const pend = pending[`${p.id}:${kind}`];
    return pend && now - pend.at < PENDING_FOR_MS ? pend.level : p.chart?.[kind] ?? null;
  };
  return { ...p, chart: { stop: pick('stop'), target: pick('target') } };
}

/** Figma "Index tab": the index, its value and the day's change. */
function IndexTabs({ indices, selected, onSelect }: { indices: { index: string; spot: number | null; change_pct: number | null }[]; selected: string; onSelect: (i: string) => void }) {
  const by = new Map(indices.map((i) => [i.index, i]));
  return (
    <div role="tablist" aria-label="Index" className="flex min-w-0 gap-2 overflow-x-auto">
      {INDEX_ORDER.map((index) => {
        const i = by.get(index);
        const on = index === selected;
        return (
          <button
            key={index}
            type="button"
            role="tab"
            aria-selected={on}
            onClick={() => onSelect(index)}
            className={`flex min-w-[124px] shrink-0 flex-col rounded-8 border px-3 py-2 text-left ${on ? 'border-line-strong bg-raised' : 'border-line hover:bg-raised/50'}`}
          >
            <span className="type-caption text-fg-muted">{index}</span>
            <span className="flex items-baseline gap-1.5">
              <span className="num-medium text-fg">{i?.spot == null ? '—' : price(i.spot)}</span>
              {i?.change_pct != null && <span className={`num-small ${TONE_TEXT[toneOf(i.change_pct)]}`}>{signedPct(i.change_pct)}</span>}
            </span>
          </button>
        );
      })}
    </div>
  );
}

/** `NIFTY:2026-11-03`: how a chain other than an index's nearest is keyed. */
const chainKeyOf = (index: string, expiry: string) => `${index}:${expiry}`;

/**
 * Trade: up to three charts (an index or any of its options, 1m/5m/15m)
 * with each manual trade's position line, stop and target — dragged to
 * move, behind `chart_stops` — the ladder of any listed expiry, every
 * strike (one tap = market buy), and the trades themselves with −1 lot,
 * Exit and a held Exit all.
 */
export function Trade({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const [saved, setSaved] = useState<Saved>(loadSaved);
  const [bus] = useState(() => new CrosshairBus());
  const [pending, setPending] = useState<Record<string, Pending>>({});
  const [sending, setSending] = useState<Record<string, boolean>>({});
  const [exitAllBusy, setExitAllBusy] = useState(false);
  const [placingAt, setPlacingAt] = useState<{ slot: number; id: string; kind: LevelKind; trigger: ChartLevel['trigger'] } | null>(null);
  const [sheet, setSheet] = useState(false);
  // The strike the ladder scrolled to, for the ladder it was scrolled on.
  const [centreAt, setCentreAt] = useState<{ key: string; strike: number } | null>(null);
  // Chains the chart pickers asked for, beyond the nearest: `INDEX:EXPIRY`.
  const [asked, setAsked] = useState<Record<string, true>>({});
  const guard = useGuard();
  const toast = useToast();

  const save = useCallback((next: (s: Saved) => Saved) => {
    setSaved((prev) => {
      const s = next(prev);
      writeStored(STORE_KEY, s);
      return s;
    });
  }, []);

  const ladderExpiry = saved.expiry[saved.index] ?? null;
  const ladderKey = `${saved.index}:${ladderExpiry ?? ''}`;
  const centre = centreAt?.key === ladderKey ? centreAt.strike : null;
  const { frame, stale, now } = useExecuteStream(serverBase, saved.index, ladderExpiry, centre);
  const flags = usePoll(() => getFlags(serverBase), 15_000, serverBase);
  const marks = usePoll(() => getMarks(serverBase), 5_000, serverBase);
  const chartStopsOn = flags.data?.chart_stops ?? false;

  const scalps = useMemo(() => (frame?.positions ?? []).filter((p) => p.manual).map((p) => withPending(p, pending, now)), [frame, pending, now]);
  const shownSlots = desktop ? saved.slots.slice(0, saved.layout) : saved.slots.slice(0, 1);
  const board = frame?.board && frame.board.index === saved.index ? frame.board : null;

  // Each index in use: its nearest chain (deltas, tick size) and its expiries.
  const wantedIndices = [...new Set([saved.index, ...shownSlots.map((s) => s.sym.index), ...scalps.map((p) => p.index)])].sort();
  const indicesKey = `${serverBase}|${wantedIndices.join(',')}`;
  const chainsPoll = usePoll(
    async () => {
      const got = await Promise.all(wantedIndices.map((i) => getChain(serverBase, i).catch(() => undefined)));
      return Object.fromEntries(wantedIndices.map((i, n) => [i, got[n]])) as Record<string, ChainAnswer | undefined>;
    },
    60_000,
    indicesKey,
  );
  const chains = chainsPoll.data ?? {};
  const expiriesPoll = usePoll(
    async () => {
      const got = await Promise.all(wantedIndices.map((i) => getExpiries(serverBase, i).catch(() => undefined)));
      return Object.fromEntries(wantedIndices.map((i, n) => [i, got[n]])) as Record<string, ExpiriesAnswer | undefined>;
    },
    60_000,
    `exp|${indicesKey}`,
  );
  const expiries = expiriesPoll.data ?? {};

  // Chains other than the nearest: the ladder's expiry, charted options,
  // positions on later expiries, and whatever a picker is looking at.
  const extraKeys = [
    ...new Set([
      ...(board ? [chainKeyOf(board.index, board.expiry)] : []),
      ...(ladderExpiry ? [chainKeyOf(saved.index, ladderExpiry)] : []),
      ...shownSlots.flatMap((s) => (s.sym.kind === 'option' ? [chainKeyOf(s.sym.index, s.sym.expiry)] : [])),
      ...scalps.flatMap((p) => (p.expiry ? [chainKeyOf(p.index, p.expiry)] : [])),
      ...Object.keys(asked),
    ]),
  ]
    .filter((key) => {
      const [index, expiry] = key.split(':');
      return chains[index]?.expiry !== expiry;
    })
    .sort();
  const extraPoll = usePoll(
    async () => {
      const got = await Promise.all(
        extraKeys.map((key) => {
          const [index, expiry] = key.split(':');
          return getChain(serverBase, index, expiry).catch(() => undefined);
        }),
      );
      return Object.fromEntries(extraKeys.map((key, n) => [key, got[n]])) as Record<string, ChainAnswer | undefined>;
    },
    60_000,
    `at|${serverBase}|${extraKeys.join(',')}`,
    extraKeys.length > 0,
  );
  const chainsAt = extraPoll.data ?? {};

  /** An index's strikes on `expiry` (null: the nearest), once loaded. */
  const chainAt = (index: string, expiry: string | null): ChainAnswer | undefined => {
    const near = chains[index];
    if (!expiry || near?.expiry === expiry) return near;
    return chainsAt[chainKeyOf(index, expiry)];
  };
  const needChain = useCallback((index: string, expiry: string | null) => {
    if (!expiry) return;
    const key = chainKeyOf(index, expiry);
    setAsked((a) => (a[key] ? a : { ...a, [key]: true }));
  }, []);

  const spot: Record<string, number | null> = Object.fromEntries((frame?.indices ?? []).map((i) => [i.index, i.spot]));
  const change: Record<string, number | null> = Object.fromEntries((frame?.indices ?? []).map((i) => [i.index, i.change_pct]));
  const held = new Map(scalps.map((p) => [p.symbol, p]));
  const blocked = buyBlockedReason(frame, stale);
  const lots = Math.max(1, Math.min(MAX_LOTS, saved.lots[saved.index] ?? 1));

  // Escape drops a "+ Stop" that hasn't been placed yet.
  useEffect(() => {
    if (!placingAt) return;
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setPlacingAt(null);
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [placingAt]);

  async function busy(id: string, run: () => Promise<Sent | null>): Promise<Sent | null> {
    setSending((s) => ({ ...s, [id]: true }));
    try {
      return await run();
    } finally {
      setSending((s) => {
        const next = { ...s };
        delete next[id];
        return next;
      });
    }
  }

  async function buy(sym: string, label: string) {
    const qty = lots * (board?.lot_size ?? 0);
    const sent = await busy(sym, () => guard('Placing orders needs write access.', () => executeBuy(serverBase, sym, lots), `${label} not bought`));
    if (sent?.ok) {
      const added = (sent.body as { action?: string } | null)?.action === 'added';
      toast({ tone: 'neutral', title: `${added ? 'Adding' : 'Buying'} ${qty} ${label}`, body: 'Market order sent — the fill shows here when it is done.' });
    }
  }

  const handlers: PositionHandlers = {
    sending,
    exitAllBusy,
    onLessLot: (p) => {
      const label = contractLabel(p.index, p.strike, p.option_type);
      void busy(p.id, () => guard('Selling needs write access.', () => sellQty(serverBase, p.id, p.lot_size), `${label} not sold`));
    },
    onExit: (p) => {
      const label = contractLabel(p.index, p.strike, p.option_type);
      void busy(p.id, () => guard('Exiting needs write access.', () => exitPosition(serverBase, p.id), `${label} not exited`));
    },
    onExitAll: () => {
      setExitAllBusy(true);
      void guard('Exiting needs write access.', () => executeExitAll(serverBase), 'Exit all not sent').finally(() => setExitAllBusy(false));
    },
  };

  async function sendLevel(p: ExecutePosition, kind: LevelKind, lvl: ChartLevel | null) {
    const key = `${p.id}:${kind}`;
    setPending((s) => ({ ...s, [key]: { level: lvl, at: Date.now() } }));
    const sent = await guard(
      'Moving a stop or target needs write access.',
      () => setChartLevels(serverBase, p.id, { [kind]: lvl }),
      `${kind === 'stop' ? 'Stop' : 'Target'} not ${lvl ? 'set' : 'removed'}`,
    );
    if (!sent?.ok) {
      // Back to what the engine has.
      setPending((s) => {
        const next = { ...s };
        delete next[key];
        return next;
      });
      return;
    }
    if ((sent.body as { sells_now?: boolean } | null)?.sells_now) {
      const label = contractLabel(p.index, p.strike, p.option_type);
      toast({ tone: 'loss', title: `${label}: already past the ${kind}`, body: 'Selling the whole holding at market now.' });
    }
  }

  const tick = (p: ExecutePosition) => chainAt(p.index, p.expiry)?.tick_size ?? chains[p.index]?.tick_size ?? 0.05;

  /** Set a level where it was dropped, on the tick. One past the price sells
   * the whole holding at market on the engine's next tick — a quick exit. */
  function trySet(p: ExecutePosition, kind: LevelKind, lvl: ChartLevel): boolean {
    void sendLevel(p, kind, { ...lvl, price: toTick(lvl.price, lvl.trigger === 'premium' ? tick(p) : 0.05) });
    return true;
  }

  const actions = (slot: number): LevelActions => ({
    canSet: chartStopsOn,
    move: (p, kind, lvl) => void trySet(p, kind, lvl),
    clear: (p, kind) => void sendLevel(p, kind, null),
    place: (p, kind, trigger) => setPlacingAt({ slot, id: p.id, kind, trigger }),
  });

  // What "+ Stop" is placing, rebuilt each render so its checks see the
  // latest prices; the chart keeps one line for it while `key` stays the same.
  const placingPos = placingAt ? scalps.find((p) => p.id === placingAt.id) ?? null : null;
  const placing = ((): (Placing & { what: string }) | null => {
    if (!placingAt || !placingPos) return null;
    const { kind, trigger } = placingAt;
    const p = placingPos;
    const color = kind === 'stop' ? C.loss : C.profit;
    const delta = deltaFor(p, chains).value;
    const word = kind === 'stop' ? 'Stop' : 'Target';
    const data: OrderLineData = {
      color,
      dashed: true,
      draggable: false,
      parts: (at) => {
        const lvl = { price: at, trigger };
        const indexNow = spot[p.index] ?? null;
        const past = levelCrossed(kind, lvl, p.option_type, p.ltp, indexNow) === true;
        return [{ key: 'label', text: past ? `${word} · past the price, sells all here` : `${word} ${approxMoney(levelPnl(lvl, p, indexNow, delta))}`, color }];
      },
      axisText: (at) => price(at),
    };
    return {
      key: `${placingAt.slot}:${p.id}:${kind}:${trigger}`,
      what: kind,
      data,
      onPlace: (at) => trySet(p, kind, { price: at, trigger }),
    };
  })();

  const setSlot = (i: number, change: Partial<Slot>) =>
    save((s) => ({ ...s, slots: s.slots.map((slot, n) => (n === i ? { ...slot, ...change } : slot)) }));
  const setLib = (chartLib: ChartLib) => save((s) => ({ ...s, chartLib }));

  /** "+ Stop" from the positions panel: placed on the chart showing the
   * trade's option (a premium level), else its index (an index level) — the
   * first chart is put on the index when none shows either. */
  function placeFromPanel(p: ExecutePosition, kind: LevelKind) {
    const option = p.expiry ? optionOf(p, p.expiry) : null;
    const onOption = option ? shownSlots.findIndex((s) => sameSym(s.sym, option)) : -1;
    if (onOption >= 0) return setPlacingAt({ slot: onOption, id: p.id, kind, trigger: 'premium' });
    let onIndex = shownSlots.findIndex((s) => s.sym.kind === 'index' && s.sym.index === p.index);
    if (onIndex < 0) {
      onIndex = 0;
      setSlot(0, { sym: { kind: 'index', index: p.index } });
    }
    if (!desktop) showChart(true);
    setPlacingAt({ slot: onIndex, id: p.id, kind, trigger: 'index' });
  }

  const showChart = (shown: boolean) => save((s) => ({ ...s, chartHidden: !shown }));
  const chartHidden = !desktop && saved.chartHidden;

  /** A price tapped on the ladder: chart that contract where its index is. */
  function chartContract(strike: number, side: 'CE' | 'PE') {
    if (!board) return;
    const sym: ChartSym = { kind: 'option', index: board.index, expiry: board.expiry, strike, side };
    const at = shownSlots.findIndex((s) => s.sym.index === board.index);
    setSlot(at >= 0 ? at : 0, { sym });
    if (!desktop) showChart(true);
  }

  function selectIndex(index: string) {
    save((s) => {
      const next = { ...s, index };
      // On phone the one chart follows the tabs, unless it shows this index's option.
      if (!desktop && s.slots[0].sym.index !== index) next.slots = s.slots.map((slot, n) => (n === 0 ? { ...slot, sym: { kind: 'index', index } } : slot));
      return next;
    });
  }

  /** The ladder's expiry: null is the nearest, which the server picks. */
  const selectExpiry = (date: string | null) =>
    save((s) => {
      const expiry = { ...s.expiry };
      if (date) expiry[s.index] = date;
      else delete expiry[s.index];
      return { ...s, expiry };
    });

  const charts = shownSlots.map((slot, i) => {
    const cx = {
      sym: slot.sym,
      scalps,
      chains,
      spot,
      fills: marks.data?.fills ?? [],
      calls: marks.data?.calls ?? [],
      telegram: marks.data?.telegram ?? [],
      layers: slot.layers,
      actions: actions(i),
    };
    const index = slot.sym.index;
    return (
      <ChartPanel
        key={i}
        chartId={`trade-${i}`}
        serverBase={serverBase}
        sym={slot.sym}
        tf={slot.tf}
        onSym={(sym) => setSlot(i, { sym })}
        onTf={(tf) => setSlot(i, { tf })}
        layers={slot.layers}
        onLayers={(layers) => setSlot(i, { layers })}
        spot={spot}
        change={change}
        expiries={expiries[index]?.expiries}
        chainAt={(expiry) => chainAt(index, expiry)}
        onNeedChain={(expiry) => needChain(index, expiry)}
        lines={chartLines(cx)}
        markers={chartMarkers(cx)}
        placing={placingAt?.slot === i ? placing : null}
        onPlacingDone={() => setPlacingAt(null)}
        onCancelPlacing={() => setPlacingAt(null)}
        bus={bus}
        lib={saved.chartLib}
        onLib={desktop ? undefined : setLib}
        compact={!desktop}
        onHide={desktop ? undefined : () => showChart(false)}
      />
    );
  });

  const expiryPicker = (size: 'sm' | 'lg') => (
    <ExpiryPicker expiries={expiries[saved.index]?.expiries} value={board?.expiry ?? ladderExpiry} onChange={selectExpiry} size={size} align={size === 'lg' ? 'left' : 'right'} />
  );

  const ladder = board ? (
    <Ladder
      board={board}
      chain={chainAt(board.index, board.expiry)}
      held={held}
      lots={lots}
      onLots={(n) => save((s) => ({ ...s, lots: { ...s.lots, [s.index]: n } }))}
      blocked={blocked}
      sending={sending}
      onBuy={(sym, label) => void buy(sym, label)}
      onChart={chartContract}
      onCentre={(strike) => setCentreAt(strike == null ? null : { key: ladderKey, strike })}
      desktop={desktop}
      header={desktop ? undefined : expiryPicker('lg')}
    />
  ) : (
    <p className="px-4 py-6 type-body text-fg-muted">{frame?.board_error ?? (frame ? 'No option chain yet — it loads after the Kotak login.' : 'Connecting…')}</p>
  );

  const sheetEl = sheet && (
    <PositionSheet
      scalps={scalps}
      h={handlers}
      indexNow={(i) => spot[i] ?? null}
      deltaOf={(p) => deltaFor(p, chains)}
      since={(p) => {
        const t = firstBuy(p, marks.data?.fills ?? []);
        return t == null ? null : istClock(t);
      }}
      canSet={chartStopsOn}
      onRemove={(p, kind) => void sendLevel(p, kind, null)}
      onAdd={(p, kind) => {
        setSheet(false);
        // Phone's one chart: put it on this scalp's index, and place there.
        if (saved.slots[0].sym.index !== p.index || saved.slots[0].sym.kind !== 'index') setSlot(0, { sym: { kind: 'index', index: p.index } });
        showChart(true);
        setPlacingAt({ slot: 0, id: p.id, kind, trigger: 'index' });
      }}
      onClose={() => setSheet(false)}
    />
  );

  if (!desktop) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="shrink-0 px-4 pt-3 pb-2">
          <IndexTabs indices={frame?.indices ?? []} selected={saved.index} onSelect={selectIndex} />
        </div>
        {chartHidden ? (
          <div className="shrink-0 px-2">
            <button type="button" onClick={() => showChart(true)} className="flex h-10 w-full items-center gap-2 rounded-12 border border-line bg-inset px-3 text-left">
              <ChartCandlestick size={16} className="text-fg-muted" />
              <span className="type-label text-fg">Show chart</span>
              <span className="min-w-0 flex-1 truncate type-caption text-fg-muted">
                {symbolLabel(saved.slots[0].sym)} · {saved.slots[0].tf}
              </span>
              <ChevronDown size={16} className="text-fg-muted" />
            </button>
          </div>
        ) : (
          <div className="flex h-[260px] shrink-0 px-2">{charts[0]}</div>
        )}
        <div className="flex min-h-0 flex-1 flex-col pt-3">{ladder}</div>
        <ScalpBar scalps={scalps} h={handlers} onOpen={() => setSheet(true)} />
        {sheetEl}
      </div>
    );
  }

  const layouts = [
    { n: 1 as const, icon: Square, label: 'One chart' },
    { n: 2 as const, icon: Columns2, label: 'Two charts' },
    { n: 3 as const, icon: Columns3, label: 'Three charts' },
  ];
  return (
    <div className="flex h-full min-h-0 flex-col gap-3 p-4">
      <div className="flex shrink-0 items-center gap-3">
        <IndexTabs indices={frame?.indices ?? []} selected={saved.index} onSelect={selectIndex} />
        <span className="flex-1" />
        <Segment label="Chart library" size="sm" value={saved.chartLib} onChange={setLib} options={CHART_LIBS} />
        <span className="type-label text-fg-muted">Charts</span>
        <div role="tablist" aria-label="Charts" className="flex gap-1 rounded-8 bg-inset p-1">
          {layouts.map(({ n, icon: Icon, label }) => (
            <button key={n} type="button" role="tab" aria-selected={saved.layout === n} aria-label={label} onClick={() => save((s) => ({ ...s, layout: n }))} className={`grid size-7 place-items-center rounded-6 ${saved.layout === n ? 'bg-raised text-fg' : 'text-fg-muted hover:text-fg'}`}>
              <Icon size={16} />
            </button>
          ))}
        </div>
      </div>
      <div className="flex min-h-[420px] flex-1 gap-3">
        <div className="flex min-w-0 flex-1 gap-3">{charts}</div>
        <aside className="flex w-[380px] shrink-0 flex-col rounded-12 border border-line bg-panel py-3">
          <div className="flex items-center gap-2 px-3 pb-3">
            <h2 className="type-title text-fg">{saved.index} options</h2>
            <span className="ml-auto">{expiryPicker('sm')}</span>
          </div>
          {ladder}
          {blocked && board && <p className="shrink-0 px-3 pt-2 type-caption text-loss">{blocked}</p>}
        </aside>
      </div>
      <ScalpPositionsPanel scalps={scalps} h={handlers} canSet={chartStopsOn} onAdd={placeFromPanel} onRemove={(p, kind) => void sendLevel(p, kind, null)} />
      {!chartStopsOn && scalps.length > 0 && (
        <p className="shrink-0 type-caption text-fg-muted">Chart stops are switched off: a stop or target already set can be removed, but none can be set or moved.</p>
      )}
    </div>
  );
}
