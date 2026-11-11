// What each Trade chart draws: a scalp's position line with its stop and
// target, Telegram trades' read-only lines, and the day's markers.

import type { ChartLevel, ChartTarget, ExecutePosition } from '../../types';
import { chartTargets, targetLotsLabel } from '../../lib/format';
import { level, price, signedMoney } from '../lib/format';
import type { CallMark, ChainAnswer, FillMark, TelegramLine } from '../lib/types';
import { C, type LinePart, type MarkerData, type OrderLineData } from './chartSetup';
import type { ChartLine, ChartMarker } from './KChart';
import { approxMoney, isOwnChart, istMs, levelCrossed, levelPnl, optionOf, sameSym, underlyingOf, type ChartSym } from './model';

export type LevelKind = 'stop' | 'target';

/** One of a scalp's levels: its stop, or one of its targets. */
export type LevelRef = { kind: 'stop' } | { kind: 'target'; target: ChartTarget };

/** Most targets one trade may have (the server's `MAX_CHART_TARGETS`). */
export const MAX_TARGETS = 5;

/** Whole lots `p` holds. */
export function heldLots(p: Pick<ExecutePosition, 'qty' | 'lot_size'>): number {
  return p.lot_size > 0 ? Math.floor(p.qty / p.lot_size) : 0;
}

/** The lots a new target on `p` may sell — those no other target has — or
 * `null` when every lot already has one (an "all left" target covers them
 * all) or the trade has its {@link MAX_TARGETS}. Its top value is "all left". */
export function targetRoom(p: ExecutePosition): number | null {
  const targets = chartTargets(p.chart);
  if (targets.length >= MAX_TARGETS || targets.some((t) => t.lots == null)) return null;
  const free = heldLots(p) - targets.reduce((n, t) => n + (t.lots ?? 0), 0);
  return free >= 1 ? free : null;
}

/** What a target of `lots` (`null`: all left) on `p` sells, if the other
 * targets go first. `id` is its own, to leave it out of the others. */
export function targetQty(p: ExecutePosition, lots: number | null, id?: string): number {
  if (lots != null) return Math.min(p.qty, lots * p.lot_size);
  const others = chartTargets(p.chart)
    .filter((t) => t.id !== id)
    .reduce((n, t) => n + (t.lots ?? 0), 0);
  return Math.max(0, p.qty - others * p.lot_size);
}

/** A target line's words: "Target ≈ +₹2,400" for a lone "all left" one,
 * as before targets had lots; "Target · 2 lots ≈ +₹1,200" otherwise. While
 * it is past the price, what letting go sells. `count`: targets on the trade. */
export function targetText(lots: number | null, count: number, past: boolean, approx: string, release: 'on release' | 'here'): string {
  const what = lots == null ? 'all' : targetLotsLabel(lots);
  if (past) return `Target · past the price, sells ${what} ${release}`;
  return lots == null && count <= 1 ? `Target ${approx}` : `Target · ${targetLotsLabel(lots)} ${approx}`;
}

/** Which layers a chart shows (its Layers menu). */
export interface Layers {
  myTrades: boolean;
  telegramTrades: boolean;
  telegramCalls: boolean;
  vwap: boolean;
  /** Standard pivot points, daily. */
  pivots: boolean;
  /** RSI (14) and CCI (20), each in a pane under the candles. */
  rsi: boolean;
  cci: boolean;
  /** KLineChart's other indicators picked for this chart, by name (`studies.ts`). */
  studies: string[];
}

export const DEFAULT_LAYERS: Layers = { myTrades: true, telegramTrades: false, telegramCalls: false, vwap: true, pivots: true, rsi: false, cci: false, studies: [] };

/** What the screen can do to a scalp's levels from a chart. */
export interface LevelActions {
  /** Levels can be set (`chart_stops` on); clearing works either way. */
  canSet: boolean;
  /** Dragged to `lvl`: a target keeps its lots. */
  move: (p: ExecutePosition, ref: LevelRef, lvl: ChartLevel) => void;
  clear: (p: ExecutePosition, ref: LevelRef) => void;
  /** "+ Stop": put one where the next click lands, on this chart. */
  place: (p: ExecutePosition, kind: LevelKind, trigger: ChartLevel['trigger']) => void;
}

export interface LineContext {
  sym: ChartSym;
  /** Scalps, with any level just set or cleared already applied. */
  scalps: ExecutePosition[];
  /** Each index's nearest chain: the expiry a row from before expiries were recorded falls back to. */
  chains: Record<string, ChainAnswer | undefined>;
  /** A position's own expiry's chain, for its delta. */
  chainFor: (p: ExecutePosition) => ChainAnswer | undefined;
  /** Each index's value now. */
  spot: Record<string, number | null>;
  fills: FillMark[];
  calls: CallMark[];
  telegram: TelegramLine[];
  layers: Layers;
  actions: LevelActions;
}

/** The delta of `p`'s option on `chain` (its own expiry's), and whether it is only assumed. */
export function deltaFor(p: Pick<ExecutePosition, 'strike' | 'option_type'>, chain: ChainAnswer | undefined): { value: number; assumed: boolean } {
  const side = p.option_type === 'PE' ? 'pe' : 'ce';
  const row = chain?.strikes.find((s) => s.strike === p.strike);
  return row?.[side]?.delta ?? { value: p.option_type === 'PE' ? -0.5 : 0.5, assumed: true };
}

/** When `p` was first bought today, if it was. */
export function firstBuy(p: ExecutePosition, fills: FillMark[]): number | null {
  const buys = fills.filter((f) => f.scalp && f.action === 'BUY' && f.index === p.index && f.strike === p.strike && f.side === p.option_type);
  return buys.length ? Math.min(...buys.map((f) => istMs(f.t))) : null;
}

/** Whether the levels on `p` can be moved now. */
function editable(p: ExecutePosition): boolean {
  return !p.halted && !p.exiting && !p.placing && p.qty > 0;
}

function levelLine(p: ExecutePosition, ref: LevelRef, lvl: ChartLevel, cx: LineContext): ChartLine {
  const { kind } = ref;
  const color = kind === 'stop' ? C.loss : C.profit;
  const delta = deltaFor(p, cx.chainFor(p)).value;
  const draggable = cx.actions.canSet && editable(p);
  const count = chartTargets(p.chart).length;
  const data: OrderLineData = {
    color,
    dashed: true,
    draggable,
    parts: (at) => {
      const moved = { price: at, trigger: lvl.trigger };
      const indexNow = cx.spot[p.index] ?? null;
      // Dropped past the price it sells at once: all of it for the stop, a target's lots for a target.
      const past = levelCrossed(kind, moved, p.option_type, p.ltp, indexNow) === true;
      const text =
        ref.kind === 'stop'
          ? past
            ? 'Stop · past the price, sells all on release'
            : `Stop ${approxMoney(levelPnl(moved, p, indexNow, delta))}`
          : targetText(ref.target.lots, count, past, approxMoney(levelPnl(moved, { ...p, qty: targetQty(p, ref.target.lots, ref.target.id) }, indexNow, delta)), 'on release');
      const parts: LinePart[] = [{ key: 'label', text, color, filled: past, icon: draggable ? 'grip' : 'lock' }];
      if (editable(p)) parts.push({ key: 'remove', text: '×', color });
      return parts;
    },
    axisText: (at) => price(at),
  };
  return {
    id: ref.kind === 'stop' ? `${p.id}:stop` : `${p.id}:target:${ref.target.id}`,
    price: lvl.price,
    data,
    zLevel: 2,
    onMove: (at) => cx.actions.move(p, ref, { price: at, trigger: lvl.trigger }),
    onPart: (key) => key === 'remove' && cx.actions.clear(p, ref),
  };
}

/** A scalp's position line: avg on its option's chart, where the underlying
 * was when it was bought on the underlying's own chart (the index, or the
 * stock's price). "+ Stop" / "+ Target" ride on it. */
function positionLine(p: ExecutePosition, at: number | { at: number }, trigger: ChartLevel['trigger'], cx: LineContext): ChartLine {
  const pnlColor = p.pnl == null ? C.fg : p.pnl < 0 ? C.loss : C.profit;
  const add = cx.actions.canSet && editable(p);
  const data: OrderLineData = {
    color: C.fg,
    dashed: false,
    draggable: false,
    parts: () => {
      const parts: LinePart[] = [
        { key: 'label', text: `${p.qty} ${p.option_type ?? ''} · avg ${price(p.avg)}`.replace('  ', ' '), color: C.fg },
        { key: 'pnl', text: p.pnl == null ? '—' : signedMoney(p.pnl), color: pnlColor },
      ];
      if (add && !p.chart?.stop) parts.push({ key: 'add-stop', text: '+ Stop', color: C.loss });
      if (add && targetRoom(p) != null) parts.push({ key: 'add-target', text: '+ Target', color: C.profit });
      return parts;
    },
    axisText: (value) => price(value),
  };
  return {
    id: `${p.id}:position`,
    price: at,
    data,
    zLevel: 1,
    onPart: (key) => {
      if (key === 'add-stop') cx.actions.place(p, 'stop', trigger);
      if (key === 'add-target') cx.actions.place(p, 'target', trigger);
    },
  };
}

/** The lines on one chart. */
export function chartLines(cx: LineContext): ChartLine[] {
  const out: ChartLine[] = [];
  if (cx.layers.myTrades) {
    for (const p of cx.scalps) {
      if (p.qty <= 0) continue;
      // The position's own expiry; the nearest only for rows from before it was recorded.
      const expiry = p.expiry ?? cx.chains[p.index]?.expiry;
      const option = expiry ? optionOf(p, expiry) : null;
      const onOption = option != null && sameSym(option, cx.sym);
      const onIndex = isOwnChart(cx.sym, p.index);
      if (!onOption && !onIndex) continue;
      const trigger: ChartLevel['trigger'] = onOption ? 'premium' : 'index';
      if (onOption) out.push(positionLine(p, p.avg, trigger, cx));
      else {
        const bought = firstBuy(p, cx.fills);
        if (bought != null) out.push(positionLine(p, { at: bought }, trigger, cx));
      }
      const stop = p.chart?.stop;
      if (stop && stop.trigger === trigger) out.push(levelLine(p, { kind: 'stop' }, stop, cx));
      for (const target of chartTargets(p.chart)) {
        if (target.trigger === trigger) out.push(levelLine(p, { kind: 'target', target }, target, cx));
      }
    }
  }
  if (cx.layers.telegramTrades && cx.sym.kind === 'option') {
    const s = cx.sym;
    for (const t of cx.telegram) {
      if (t.index !== s.index || t.strike !== s.strike || t.side !== s.side || t.expiry !== s.expiry) continue;
      out.push(...telegramLines(t));
    }
  }
  return out;
}

/** A Telegram trade on its option's chart: read-only, with a lock. */
function telegramLines(t: TelegramLine): ChartLine[] {
  const locked = (color: string, text: string): OrderLineData => ({
    color,
    dashed: true,
    draggable: false,
    parts: () => [{ key: 'label', text, color, icon: 'lock' }],
    axisText: (v) => price(v),
  });
  const held = t.state !== 'waiting';
  const lines: ChartLine[] = [
    {
      id: `tg:${t.id}:entry`,
      price: held ? t.avg : t.entry,
      zLevel: 1,
      data: {
        color: C.fg,
        dashed: !held,
        draggable: false,
        parts: () => [
          { key: 'label', text: held ? `${t.channel} · ${t.qty} · avg ${price(t.avg)}` : `${t.channel} · buys at ${level(t.entry)}`, color: C.fg, icon: 'lock' },
          ...(held && t.pnl != null ? [{ key: 'pnl', text: signedMoney(t.pnl), color: t.pnl < 0 ? C.loss : C.profit }] : []),
        ],
        axisText: (v) => price(v),
      },
    },
  ];
  if (t.stop > 0) lines.push({ id: `tg:${t.id}:stop`, price: t.stop, data: locked(C.loss, `SL ${level(t.stop)}`), zLevel: 2 });
  t.targets.forEach((x, i) => lines.push({ id: `tg:${t.id}:t${i + 1}`, price: x, data: locked(C.profit, `T${i + 1} ${level(x)}`), zLevel: 2 }));
  return lines;
}

function markerKind(f: FillMark): MarkerData['kind'] {
  if (f.action === 'BUY') return 'B';
  const r = f.reason ?? '';
  if (r.startsWith('TGT') || r === 'CHART_TARGET') return 'T';
  if (r === 'SL_HIT' || r.includes('TRAIL') || r === 'CHART_STOP') return 'S';
  return 'E';
}

/** The day's markers on one chart: fills (scalps under "My scalp trades",
 * Telegram's under "Telegram trades") and when Telegram calls arrived. */
export function chartMarkers(cx: LineContext): ChartMarker[] {
  const out: ChartMarker[] = [];
  const s = cx.sym;
  const underlying = underlyingOf(s);
  for (const f of cx.fills) {
    if (f.index !== underlying) continue;
    if (f.scalp ? !cx.layers.myTrades : !cx.layers.telegramTrades) continue;
    if (s.kind === 'option' && (f.strike !== s.strike || f.side !== s.side)) continue;
    const kind = markerKind(f);
    out.push({
      id: `fill:${f.id}`,
      t: istMs(f.t),
      price: s.kind === 'option' ? f.price : undefined,
      data: { kind, above: kind !== 'B' },
    });
  }
  if (cx.layers.telegramCalls && s.kind === 'index') {
    for (const c of cx.calls) {
      if (c.index === s.index) out.push({ id: `call:${c.id}`, t: istMs(c.t), data: { kind: 'call', above: true } });
    }
  }
  return out;
}
