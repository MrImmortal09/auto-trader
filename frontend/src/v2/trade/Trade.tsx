import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChartCandlestick, ChevronDown, ChevronLeft, ChevronRight, ChevronUp, Columns2, Columns3, Square } from 'lucide-react';
import type { ChartLevel, ChartTarget, ExecutePosition } from '../../types';
import { chartTargets, targetLotsLabel } from '../../lib/format';
import { useExecuteStream } from '../../hooks/useExecuteStream';
import { buyBlockedReason, contractLabel, INDEX_ORDER, MAX_LOTS, readStored, writeStored } from '../../lib/execute';
import { useIdentity } from '../auth/identity';
import {
  cancelPosition,
  executeBuy,
  executeExitAll,
  exitPosition,
  getChain,
  getDesk,
  getExpiries,
  getFlags,
  getHistory,
  getMarks,
  getMyActivity,
  getMyDesk,
  getMyHistory,
  getMyMarks,
  paperBuy,
  paperClose,
  paperExitAll,
  paperSellLots,
  sellQty,
  setChartLevels,
  setPaperLevels,
  streamToken,
  type LevelsAnswer,
  type LevelsUpdate,
  type PaperFill,
  type Sent,
} from '../lib/api';
import { money, price, signedPct, toneOf, todayIst } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { ChainAnswer, ExpiriesAnswer } from '../lib/types';
import { useGuard } from '../shell/useGuard';
import { Segment } from '../ui/Segment';
import { useToast } from '../ui/toast';
import { C, type OrderLineData } from './chartSetup';
import { ChartPanel, type LotsPick } from './ChartPanel';
import { CrosshairBus } from './bus';
import { ExpiryPicker } from './ExpiryPicker';
import type { Placing } from './KChart';
import { Ladder } from './Ladder';
import { chartLines, chartMarkers, DEFAULT_LAYERS, deltaFor, firstBuy, targetQty, targetRoom, targetText, type LevelActions, type LevelKind, type LevelRef, type Layers } from './lines';
import { approxMoney, CHART_LIBS, istClock, levelCrossed, levelPnl, optionOf, sameSym, symbolLabel, toTick, type CandleStyle, type ChartLib, type ChartSym, type Tf } from './model';
import { clampPanelHeight, DEFAULT_PANEL_HEIGHT } from './panel/height';
import { usePhoneSplit } from './phoneSplit';
import { SplitHandle } from './SplitHandle';
import { TradingPanel } from './panel/TradingPanel';
import { PositionSheet, ScalpBar, type PaperAccount, type PositionHandlers } from './Positions';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

interface Slot {
  sym: ChartSym;
  tf: Tf;
  layers: Layers;
  style: CandleStyle;
}

interface Saved {
  layout: 1 | 2 | 3;
  slots: Slot[];
  index: string;
  lots: Record<string, number>;
  /** Phone: the chart folded away, the whole ladder shown instead. */
  chartHidden: boolean;
  /** Phone: the chart's height as last dragged, in px; null until it is. */
  phoneChartHeight: number | null;
  /** The ladder's expiry per index, `YYYY-MM-DD`; missing means the nearest. */
  expiry: Record<string, string | null>;
  /** Which library draws every chart. */
  chartLib: ChartLib;
  /** Desktop: the trading panel's body shown under the charts. */
  panelOpen: boolean;
  /** Desktop: the trading panel's height while open, in px. */
  panelHeight: number;
  /** Desktop: the index bar folded away, for room. */
  barHidden: boolean;
  /** Desktop: the options ladder folded away, the charts taking its width. */
  ladderHidden: boolean;
}

const STORE_KEY = 'v2.trade';
/** The screen's name before it became Trade; read once, for its saved layout. */
const OLD_STORE_KEY = 'v2.scalp';
const indexSlot = (index: string): Slot => ({ sym: { kind: 'index', index }, tf: '5m', layers: DEFAULT_LAYERS, style: 'candles' });
const DEFAULTS: Saved = { layout: 2, slots: [indexSlot('NIFTY'), indexSlot('BANKNIFTY'), indexSlot('SENSEX')], index: 'NIFTY', lots: {}, chartHidden: false, phoneChartHeight: null, expiry: {}, chartLib: 'kline', panelOpen: true, panelHeight: DEFAULT_PANEL_HEIGHT, barHidden: false, ladderHidden: false };

/** The saved layout, with any option that has since expired back on its
 * index, and any chosen expiry that has passed back to the nearest. */
function loadSaved(): Saved {
  const saved = readStored<Partial<Saved>>(STORE_KEY, readStored<Partial<Saved>>(OLD_STORE_KEY, {}));
  const today = todayIst();
  const slots = DEFAULTS.slots.map((d, i) => {
    const s = saved.slots?.[i];
    if (!s?.sym) return d;
    const sym = s.sym.kind === 'option' && s.sym.expiry < today ? { kind: 'index' as const, index: s.sym.index } : s.sym;
    const style: CandleStyle = s.style === 'bars' || s.style === 'area' ? s.style : 'candles';
    return { sym, tf: s.tf ?? '5m', layers: { ...DEFAULT_LAYERS, ...s.layers }, style };
  });
  const expiry = Object.fromEntries(Object.entries(saved.expiry ?? {}).filter(([index, date]) => INDEX_ORDER.includes(index) && typeof date === 'string' && date >= today));
  return {
    layout: saved.layout === 1 || saved.layout === 3 ? saved.layout : 2,
    slots,
    index: INDEX_ORDER.includes(saved.index ?? '') ? saved.index! : 'NIFTY',
    lots: saved.lots ?? {},
    chartHidden: saved.chartHidden === true,
    phoneChartHeight: typeof saved.phoneChartHeight === 'number' && saved.phoneChartHeight > 0 ? saved.phoneChartHeight : null,
    expiry,
    chartLib: saved.chartLib === 'vela' ? 'vela' : 'kline',
    panelOpen: saved.panelOpen !== false,
    panelHeight: clampPanelHeight(typeof saved.panelHeight === 'number' ? saved.panelHeight : DEFAULT_PANEL_HEIGHT),
    barHidden: saved.barHidden === true,
    ladderHidden: saved.ladderHidden === true,
  };
}

/** A level set or cleared here, shown until the stream has it: under
 * `ID:stop` the stop, under `ID:target:TARGET` one target (`null`: removed). */
interface Pending {
  stop?: ChartLevel | null;
  target?: ChartTarget | null;
  at: number;
}

const PENDING_FOR_MS = 3_000;
/** A new target's id until the server answers with its own. */
let newTargets = 0;

/** A scalp with the levels just set or cleared from here applied. */
function withPending(p: ExecutePosition, pending: Record<string, Pending>, now: number): ExecutePosition {
  const fresh = (key: string) => {
    const pend = pending[key];
    return pend && now - pend.at < PENDING_FOR_MS ? pend : undefined;
  };
  const stopPend = fresh(`${p.id}:stop`);
  const stop = stopPend ? (stopPend.stop ?? null) : (p.chart?.stop ?? null);
  const prefix = `${p.id}:target:`;
  const shown = chartTargets(p.chart);
  const targets = shown.flatMap((t) => {
    const pend = fresh(prefix + t.id);
    return pend ? (pend.target ? [pend.target] : []) : [t];
  });
  // Set from here, not in the stream yet.
  for (const [key, pend] of Object.entries(pending)) {
    const t = pend.target;
    if (key.startsWith(prefix) && t && now - pend.at < PENDING_FOR_MS && !shown.some((s) => s.id === t.id)) targets.push(t);
  }
  return { ...p, chart: { stop, targets } };
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
  // `lots`: what a target being placed sells — its top value is "all left".
  const [placingAt, setPlacingAt] = useState<{ slot: number; id: string; kind: LevelKind; trigger: ChartLevel['trigger']; lots: number } | null>(null);
  const [sheet, setSheet] = useState(false);
  // The strike the ladder scrolled to, for the ladder it was scrolled on.
  const [centreAt, setCentreAt] = useState<{ key: string; strike: number } | null>(null);
  // Chains the chart pickers asked for, beyond the nearest: `INDEX:EXPIRY`.
  const [asked, setAsked] = useState<Record<string, true>>({});
  // Which slot fills the whole screen; null when none does.
  const [fullscreenSlot, setFullscreenSlot] = useState<number | null>(null);
  const guard = useGuard();
  const toast = useToast();
  const { canReadOwner } = useIdentity();
  // Whose desk a tap trades on: the owner's engine for the admin, a user's
  // own paper portfolio otherwise (`/api/v2/me/*`, filled at once).
  const scope = canReadOwner ? 'admin' : 'user';
  const paper = !canReadOwner;

  const save = useCallback((next: (s: Saved) => Saved) => {
    setSaved((prev) => {
      const s = next(prev);
      writeStored(STORE_KEY, s);
      return s;
    });
  }, []);
  // Desktop: the charts row, which the trading panel grows up into.
  const chartsRef = useRef<HTMLDivElement>(null);
  const setPanelHeight = useCallback((panelHeight: number) => save((s) => ({ ...s, panelHeight })), [save]);
  // Phone: the chart and the ladder share the screen; a handle drags the split.
  const { attach: splitBox, height: chartHeight, min: splitMin, max: splitMax, dragging: splitDragging, handle: splitHandle } = usePhoneSplit(
    saved.phoneChartHeight,
    useCallback((phoneChartHeight: number) => save((s) => ({ ...s, phoneChartHeight })), [save]),
  );

  // Each index on screen: its listed expiries, before the stream opens.
  const shownSlots = desktop ? saved.slots.slice(0, saved.layout) : saved.slots.slice(0, 1);

  const toggleFullscreen = useCallback((slot: number, on: boolean) => {
    if (on) {
      document.documentElement.requestFullscreen?.().catch(() => {});
      setFullscreenSlot(slot);
    } else {
      if (document.fullscreenElement) {
        document.exitFullscreen?.().catch(() => {});
      }
      setFullscreenSlot(null);
    }
  }, []);

  useEffect(() => {
    const onChange = () => {
      if (!document.fullscreenElement) setFullscreenSlot(null);
    };
    document.addEventListener('fullscreenchange', onChange);
    return () => document.removeEventListener('fullscreenchange', onChange);
  }, []);

  const activeFullscreenSlot = fullscreenSlot !== null && fullscreenSlot < shownSlots.length ? fullscreenSlot : null;
  const expiryIndices = [...new Set([saved.index, ...shownSlots.map((s) => s.sym.index)])].sort();
  const expiriesPoll = usePoll(
    async () => {
      const got = await Promise.all(expiryIndices.map((i) => getExpiries(serverBase, i).catch(() => undefined)));
      return Object.fromEntries(expiryIndices.map((i, n) => [i, got[n]])) as Record<string, ExpiriesAnswer | undefined>;
    },
    60_000,
    `exp|${serverBase}|${expiryIndices.join(',')}`,
  );
  const expiries = expiriesPoll.data ?? {};
  // The chosen expiry, while the index's list still has it; else the nearest.
  const listedExpiries = expiries[saved.index]?.expiries;
  const chosenExpiry = saved.expiry[saved.index] ?? null;
  const ladderExpiry = chosenExpiry && (!listedExpiries || listedExpiries.some((e) => e.date === chosenExpiry)) ? chosenExpiry : null;
  const ladderKey = `${saved.index}:${ladderExpiry ?? ''}`;
  const centre = centreAt?.key === ladderKey ? centreAt.strike : null;
  const { frame, stale, now } = useExecuteStream(serverBase, saved.index, ladderExpiry, centre, streamToken());
  const flags = usePoll(() => getFlags(serverBase), 15_000, serverBase);
  // Whose figures the screen shows: the owner's desk for the admin, a user's
  // own paper portfolio otherwise.
  const viewer = `${serverBase}|${paper ? 'paper' : 'owner'}`;
  // The fills on the charts are the caller's own: the owner's (with the
  // Telegram calls) for the admin, a user's paper fills otherwise.
  const marks = usePoll(() => (paper ? getMyMarks(serverBase) : getMarks(serverBase)), 5_000, viewer);
  // `chart_stops` gates the owner's engine; a paper position always takes levels.
  const chartStopsOn = paper || (flags.data?.chart_stops ?? false);
  // The panel's account column and History tab.
  const desk = usePoll(() => (paper ? getMyDesk(serverBase) : getDesk(serverBase)), 5_000, viewer);
  const history = usePoll(() => (paper ? getMyHistory(serverBase, 'today', 'channel') : getHistory(serverBase, 'today', 'channel')), 30_000, viewer);
  // A user's fills, levels and resets, for the Activity tab.
  const myActivity = usePoll(() => getMyActivity(serverBase), 5_000, serverBase, paper);
  const account: PaperAccount | undefined = paper
    ? { balance: desk.data?.wallet.balance ?? null, today: desk.data?.kpis.net ?? null, activity: myActivity.data }
    : undefined;

  const scalps = useMemo(() => (frame?.positions ?? []).filter((p) => p.manual).map((p) => withPending(p, pending, now)), [frame, pending, now]);
  const activeScalps = useMemo(() => scalps.filter((p) => !p.placing), [scalps]);
  const board = frame?.board && frame.board.index === saved.index ? frame.board : null;

  // Each index in use: its nearest chain (deltas, tick size).
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
  /** A position's own expiry's chain, for its delta and tick. */
  const chainFor = (p: ExecutePosition) => chainAt(p.index, p.expiry);
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
    const order = () => (paper ? paperBuy(serverBase, sym, lots) : executeBuy(serverBase, sym, lots));
    const sent = await busy(sym, () => guard('Sign in to place paper orders.', order, `${label} not bought`, scope));
    if (!sent?.ok) return;
    if (paper) {
      // Filled already. Keyed as the status bar's fill toast is, so that one isn't shown again.
      const f = sent.body as PaperFill;
      toast({ tone: 'profit', title: `Bought ${f.qty} ${f.contract}`, body: `Filled at ${price(f.price)} · wallet ${money(f.balance)}`, key: `fill-${f.trade_id}` });
      return;
    }
    const added = (sent.body as { action?: string } | null)?.action === 'added';
    toast({ tone: 'neutral', title: `${added ? 'Adding' : 'Buying'} ${qty} ${label}`, body: 'Market order sent — the fill shows here when it is done.' });
  }

  // A paper sell fills at once and its toast, with the P&L, comes from the status bar.
  const handlers: PositionHandlers = {
    sending,
    exitAllBusy,
    onLessLot: (p) => {
      const label = contractLabel(p.index, p.strike, p.option_type);
      const sell = () => (paper ? paperSellLots(serverBase, p.id, 1) : sellQty(serverBase, p.id, p.lot_size));
      void busy(p.id, () => guard('Selling needs write access.', sell, `${label} not sold`, scope));
    },
    onExit: (p) => {
      const label = contractLabel(p.index, p.strike, p.option_type);
      const exit = () => (paper ? paperClose(serverBase, p.id) : exitPosition(serverBase, p.id));
      void busy(p.id, () => guard('Exiting needs write access.', exit, `${label} not exited`, scope));
    },
    onCancel: (p) => {
      const label = contractLabel(p.index, p.strike, p.option_type);
      void busy(p.id, () => guard('Cancelling needs write access.', () => cancelPosition(serverBase, p.id), `${label} not cancelled`));
    },
    onExitAll: () => {
      setExitAllBusy(true);
      const exitAll = () => (paper ? paperExitAll(serverBase) : executeExitAll(serverBase));
      void guard('Exiting needs write access.', exitAll, 'Exit all not sent', scope)
        .then((sent) => {
          const notSold = paper && sent?.ok ? ((sent.body as { not_sold?: string[] } | null)?.not_sold ?? []) : [];
          if (notSold.length) toast({ tone: 'loss', title: `${notSold.length} not sold`, body: notSold.join(' · ') });
        })
        .finally(() => setExitAllBusy(false));
    },
  };

  /** Send `update` for `p`, showing `shown` (see {@link Pending}) until the
   * stream has it; what the server answered, or null when it refused. */
  async function sendLevels(p: ExecutePosition, update: LevelsUpdate, shown: Record<string, Omit<Pending, 'at'>>, failed: string): Promise<LevelsAnswer | null> {
    setPending((s) => {
      const at = Date.now();
      return { ...s, ...Object.fromEntries(Object.entries(shown).map(([k, v]) => [k, { ...v, at }])) };
    });
    const sent = await guard(
      'Moving a stop or target needs write access.',
      () => (paper ? setPaperLevels(serverBase, p.id, update) : setChartLevels(serverBase, p.id, update)),
      failed,
      scope,
    );
    if (!sent?.ok) {
      // Back to what the engine has.
      setPending((s) => {
        const next = { ...s };
        for (const key of Object.keys(shown)) delete next[key];
        return next;
      });
      return null;
    }
    const answer = (sent.body ?? {}) as LevelsAnswer;
    if (answer.sells_now) {
      const label = contractLabel(p.index, p.strike, p.option_type);
      const what = update.stop ? 'stop' : 'target';
      const sells = answer.sells_lots == null ? 'the whole holding' : targetLotsLabel(answer.sells_lots);
      toast({ tone: 'loss', title: `${label}: already past the ${what}`, body: `Selling ${sells} at market now.` });
    }
    return answer;
  }

  const tick = (p: ExecutePosition) => chainFor(p)?.tick_size ?? chains[p.index]?.tick_size ?? 0.05;
  /** A level where it was dropped, on the tick. */
  const ticked = (p: ExecutePosition, lvl: ChartLevel): ChartLevel => ({ ...lvl, price: toTick(lvl.price, lvl.trigger === 'premium' ? tick(p) : 0.05) });

  /** Set or clear the stop. One past the price sells the whole holding at
   * market on the engine's next tick — a quick exit. */
  function setStop(p: ExecutePosition, lvl: ChartLevel | null) {
    const stop = lvl && ticked(p, lvl);
    void sendLevels(p, { stop }, { [`${p.id}:stop`]: { stop } }, `Stop not ${lvl ? 'set' : 'removed'}`);
  }

  /** Move `existing` to `lvl` (it keeps its lots), or add a target there
   * selling `lots` (`null`: all left). One past the price sells its lots at
   * once. */
  async function setTarget(p: ExecutePosition, existing: ChartTarget | null, lvl: ChartLevel, lots: number | null = null) {
    const at = ticked(p, lvl);
    const id = existing?.id ?? `new-${++newTargets}`;
    const target: ChartTarget = { ...at, id, lots: existing ? existing.lots : lots };
    const update: LevelsUpdate = { set_target: existing ? { id: existing.id, ...at } : { ...at, lots } };
    const answer = await sendLevels(p, update, { [`${p.id}:target:${id}`]: { target } }, `Target not ${existing ? 'moved' : 'set'}`);
    const real = answer?.target_id;
    if (!existing && real) {
      // Shown under the server's id from now on.
      setPending((s) => {
        const next = { ...s };
        const pend = next[`${p.id}:target:${id}`];
        delete next[`${p.id}:target:${id}`];
        if (pend) next[`${p.id}:target:${real}`] = { ...pend, target: { ...target, id: real } };
        return next;
      });
    }
  }

  function removeTarget(p: ExecutePosition, t: ChartTarget) {
    void sendLevels(p, { remove_target: t.id }, { [`${p.id}:target:${t.id}`]: { target: null } }, 'Target not removed');
  }

  const clearLevel = (p: ExecutePosition, ref: LevelRef) => (ref.kind === 'stop' ? setStop(p, null) : removeTarget(p, ref.target));

  /** "+ Stop" / "+ Target": the next tap on chart `slot` places it — a target
   * selling all the lots no other target has, until the picker says fewer. */
  const startPlacing = (slot: number, p: ExecutePosition, kind: LevelKind, trigger: ChartLevel['trigger']) =>
    setPlacingAt({ slot, id: p.id, kind, trigger, lots: targetRoom(p) ?? 1 });

  const actions = (slot: number): LevelActions => ({
    canSet: chartStopsOn,
    move: (p, ref, lvl) => (ref.kind === 'stop' ? setStop(p, lvl) : void setTarget(p, ref.target, lvl)),
    clear: clearLevel,
    place: (p, kind, trigger) => startPlacing(slot, p, kind, trigger),
  });

  // What "+ Stop" is placing, rebuilt each render so its checks see the
  // latest prices; the chart keeps one line for it while `key` stays the same.
  const placingPos = placingAt ? scalps.find((p) => p.id === placingAt.id) ?? null : null;
  // A target's lots: up to what no other target has; the top is "all left".
  // With every lot covered meanwhile, there is nothing to place.
  const room = placingPos ? targetRoom(placingPos) : null;
  const placing = ((): (Placing & { what: string; lots?: LotsPick }) | null => {
    if (!placingAt || !placingPos) return null;
    const { kind, trigger } = placingAt;
    const p = placingPos;
    const isTarget = kind === 'target';
    if (isTarget && room == null) return null;
    const chosen = isTarget ? Math.max(1, Math.min(placingAt.lots, room!)) : 0;
    const lots = isTarget && chosen < room! ? chosen : null;
    const color = kind === 'stop' ? C.loss : C.profit;
    const delta = deltaFor(p, chainFor(p)).value;
    const count = chartTargets(p.chart).length + 1;
    const data: OrderLineData = {
      color,
      dashed: true,
      draggable: false,
      parts: (at) => {
        const lvl = { price: at, trigger };
        const indexNow = spot[p.index] ?? null;
        const past = levelCrossed(kind, lvl, p.option_type, p.ltp, indexNow) === true;
        const text = isTarget
          ? targetText(lots, count, past, approxMoney(levelPnl(lvl, { ...p, qty: targetQty(p, lots) }, indexNow, delta)), 'here')
          : past
            ? 'Stop · past the price, sells all here'
            : `Stop ${approxMoney(levelPnl(lvl, p, indexNow, delta))}`;
        return [{ key: 'label', text, color }];
      },
      axisText: (at) => price(at),
    };
    return {
      key: `${placingAt.slot}:${p.id}:${kind}:${trigger}`,
      what: kind,
      data,
      lots: isTarget
        ? { value: chosen, max: room!, others: count > 1, set: (n) => setPlacingAt((a) => a && { ...a, lots: Math.max(1, Math.min(n, room!)) }) }
        : undefined,
      onPlace: (at) => {
        if (isTarget) void setTarget(p, null, { price: at, trigger }, lots);
        else setStop(p, { price: at, trigger });
        return true;
      },
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
    if (onOption >= 0) return startPlacing(onOption, p, kind, 'premium');
    let onIndex = shownSlots.findIndex((s) => s.sym.kind === 'index' && s.sym.index === p.index);
    if (onIndex < 0) {
      onIndex = 0;
      setSlot(0, { sym: { kind: 'index', index: p.index } });
    }
    if (!desktop) showChart(true);
    startPlacing(onIndex, p, kind, 'index');
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
    const isFullscreen = activeFullscreenSlot === i;
    const cx = {
      sym: slot.sym,
      scalps,
      chains,
      chainFor,
      spot,
      fills: marks.data?.fills ?? [],
      calls: marks.data?.calls ?? [],
      telegram: marks.data?.telegram ?? [],
      layers: slot.layers,
      actions: actions(i),
    };
    const index = slot.sym.index;
    return (
      <div
        key={i}
        style={!desktop && !isFullscreen ? { height: chartHeight } : undefined}
        className={
          isFullscreen
            ? 'fixed inset-0 z-40 flex flex-col bg-canvas pt-safe pb-safe'
            : desktop
              ? 'flex min-h-0 min-w-0 flex-1 flex-col'
              : 'flex shrink-0'
        }
      >
        <ChartPanel
          chartId={`trade-${i}`}
          serverBase={serverBase}
          sym={slot.sym}
          tf={slot.tf}
          onSym={(sym) => setSlot(i, { sym })}
          onTf={(tf) => setSlot(i, { tf })}
          layers={slot.layers}
          onLayers={(layers) => setSlot(i, { layers })}
          style={slot.style}
          onStyle={(style) => setSlot(i, { style })}
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
          fullscreen={isFullscreen}
          onFullscreen={(on) => toggleFullscreen(i, on)}
        />
        {isFullscreen && activeScalps.length > 0 && (
          <ScalpBar scalps={activeScalps} h={handlers} onOpen={() => setSheet(true)} paper={account} />
        )}
      </div>
    );
  });

  const expiryPicker = (size: 'sm' | 'lg', short = false) => (
    <ExpiryPicker expiries={expiries[saved.index]?.expiries} value={board?.expiry ?? ladderExpiry} onChange={selectExpiry} size={size} align={size === 'lg' && !short ? 'left' : 'right'} short={short} />
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
      header={desktop ? undefined : expiryPicker('lg', true)}
    />
  ) : (
    <p className="px-4 py-6 type-body text-fg-muted">{frame?.board_error ?? (frame ? 'No option chain yet — it loads after the Kotak login.' : 'Connecting…')}</p>
  );

  const sheetEl = sheet && (
    <PositionSheet
      scalps={scalps}
      h={handlers}
      mode={frame?.mode ?? 'PAPER'}
      desk={desk.data}
      history={history.data?.calls ?? []}
      fills={marks.data?.fills ?? []}
      errors={frame?.errors ?? []}
      indexNow={(i) => spot[i] ?? null}
      deltaOf={(p) => deltaFor(p, chainFor(p))}
      since={(p) => {
        const t = firstBuy(p, marks.data?.fills ?? []);
        return t == null ? null : istClock(t);
      }}
      canSet={chartStopsOn}
      onRemove={clearLevel}
      onAdd={(p, kind) => {
        setSheet(false);
        const targetSlot = activeFullscreenSlot ?? 0;
        if (saved.slots[targetSlot].sym.index !== p.index || saved.slots[targetSlot].sym.kind !== 'index') setSlot(targetSlot, { sym: { kind: 'index', index: p.index } });
        if (!desktop) showChart(true);
        startPlacing(targetSlot, p, kind, 'index');
      }}
      onClose={() => setSheet(false)}
      paper={account}
    />
  );

  if (!desktop) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="shrink-0 px-4 pt-3 pb-2">
          <IndexTabs indices={frame?.indices ?? []} selected={saved.index} onSelect={selectIndex} />
        </div>
        {/* The chart and the ladder share this box: the chart is never so tall
            that the ladder loses its lots row and its first strikes. */}
        <div ref={splitBox} className="flex min-h-0 flex-1 flex-col">
          {chartHidden ? (
            <div className="shrink-0 px-2 pb-3">
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
            <>
              {charts[0]}
              <SplitHandle height={chartHeight} min={splitMin} max={splitMax} dragging={splitDragging} handle={splitHandle} />
            </>
          )}
          <div className="flex min-h-0 flex-1 flex-col">{ladder}</div>
        </div>
        <ScalpBar scalps={activeScalps} h={handlers} onOpen={() => setSheet(true)} paper={account} />
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
    <div className="flex h-full min-h-0 flex-col gap-2 p-2">
      {saved.barHidden ? (
        <button
          type="button"
          onClick={() => save((s) => ({ ...s, barHidden: false }))}
          aria-label="Show the index bar"
          title="Show the index bar"
          className="-mt-2 -mb-1 flex h-4 shrink-0 items-center justify-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg"
        >
          <ChevronDown size={14} />
        </button>
      ) : (
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
          <button
            type="button"
            onClick={() => save((s) => ({ ...s, barHidden: true }))}
            aria-label="Hide the index bar"
            title="Hide the index bar"
            className="grid size-8 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg"
          >
            <ChevronUp size={16} />
          </button>
        </div>
      )}
      <div ref={chartsRef} className="flex min-h-[200px] flex-1 gap-2">
        <div className="flex min-w-0 flex-1 gap-1">{charts}</div>
        {saved.ladderHidden ? (
          <button
            type="button"
            onClick={() => save((s) => ({ ...s, ladderHidden: false }))}
            aria-label={`Show the ${saved.index} options`}
            title={`Show the ${saved.index} options`}
            className="-mx-1 flex w-5 shrink-0 items-center justify-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg"
          >
            <ChevronLeft size={14} />
          </button>
        ) : (
          <aside className="flex w-[380px] shrink-0 flex-col rounded-12 border border-line bg-panel py-3">
            <div className="flex items-center gap-2 px-3 pb-3">
              <h2 className="type-title text-fg">{saved.index} options</h2>
              <span className="ml-auto">{expiryPicker('sm')}</span>
              <button
                type="button"
                onClick={() => save((s) => ({ ...s, ladderHidden: true }))}
                aria-label={`Hide the ${saved.index} options`}
                title={`Hide the ${saved.index} options`}
                className="grid size-7 shrink-0 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg"
              >
                <ChevronRight size={16} />
              </button>
            </div>
            {ladder}
            {blocked && board && <p className="shrink-0 px-3 pt-2 type-caption text-loss">{blocked}</p>}
          </aside>
        )}
      </div>
      <TradingPanel
        serverBase={serverBase}
        mode={frame?.mode ?? 'PAPER'}
        desk={desk.data}
        scalps={scalps}
        errors={frame?.errors ?? []}
        fills={marks.data?.fills ?? []}
        history={history.data?.calls ?? []}
        h={handlers}
        canSet={chartStopsOn}
        onAdd={placeFromPanel}
        onRemove={clearLevel}
        open={saved.panelOpen}
        onToggle={() => save((s) => ({ ...s, panelOpen: !s.panelOpen }))}
        height={saved.panelHeight}
        onHeight={setPanelHeight}
        chartsRef={chartsRef}
        layout={saved.barHidden ? 'bar hidden' : 'bar shown'}
        paper={account}
      />
      {!chartStopsOn && scalps.length > 0 && (
        <p className="shrink-0 type-caption text-fg-muted">Chart stops are switched off: a stop or target already set can be removed, but none can be set or moved.</p>
      )}
      {sheetEl}
    </div>
  );
}
