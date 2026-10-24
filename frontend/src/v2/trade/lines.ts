// What each Trade chart draws: a scalp's position line with its stop and
// target, Telegram trades' read-only lines, and the day's markers.

import type { ChartLevel, ExecutePosition } from '../../types';
import { level, price, signedMoney } from '../lib/format';
import type { CallMark, ChainAnswer, FillMark, TelegramLine } from '../lib/types';
import { C, type LinePart, type MarkerData, type OrderLineData } from './chartSetup';
import type { ChartLine, ChartMarker } from './KChart';
import { approxMoney, istMs, levelCrossed, levelPnl, optionOf, sameSym, type ChartSym } from './model';

export type LevelKind = 'stop' | 'target';

/** Which layers a chart shows (its Layers menu). */
export interface Layers {
  myTrades: boolean;
  telegramTrades: boolean;
  telegramCalls: boolean;
  vwap: boolean;
  /** Standard pivot points, daily. */
  pivots: boolean;
}

export const DEFAULT_LAYERS: Layers = { myTrades: true, telegramTrades: false, telegramCalls: false, vwap: true, pivots: true };

/** What the screen can do to a scalp's levels from a chart. */
export interface LevelActions {
  /** Levels can be set (`chart_stops` on); clearing works either way. */
  canSet: boolean;
  move: (p: ExecutePosition, kind: LevelKind, lvl: ChartLevel) => void;
  clear: (p: ExecutePosition, kind: LevelKind) => void;
  /** "+ Stop": put one where the next click lands, on this chart. */
  place: (p: ExecutePosition, kind: LevelKind, trigger: ChartLevel['trigger']) => void;
}

export interface LineContext {
  sym: ChartSym;
  /** Scalps, with any level just set or cleared already applied. */
  scalps: ExecutePosition[];
  chains: Record<string, ChainAnswer | undefined>;
  /** Each index's value now. */
  spot: Record<string, number | null>;
  fills: FillMark[];
  calls: CallMark[];
  telegram: TelegramLine[];
  layers: Layers;
  actions: LevelActions;
}

/** The delta of `p`'s option, and whether it is only assumed. */
export function deltaFor(p: Pick<ExecutePosition, 'index' | 'strike' | 'option_type'>, chains: LineContext['chains']): { value: number; assumed: boolean } {
  const side = p.option_type === 'PE' ? 'pe' : 'ce';
  const row = chains[p.index]?.strikes.find((s) => s.strike === p.strike);
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

function levelLine(p: ExecutePosition, kind: LevelKind, lvl: ChartLevel, cx: LineContext): ChartLine {
  const color = kind === 'stop' ? C.loss : C.profit;
  const word = kind === 'stop' ? 'Stop' : 'Target';
  const delta = deltaFor(p, cx.chains).value;
  const draggable = cx.actions.canSet && editable(p);
  const data: OrderLineData = {
    color,
    dashed: true,
    draggable,
    parts: (at) => {
      const moved = { price: at, trigger: lvl.trigger };
      // Dropped past the price it sells the whole holding at once.
      const past = levelCrossed(kind, moved, p.option_type, p.ltp, cx.spot[p.index] ?? null) === true;
      const parts: LinePart[] = [
        {
          key: 'label',
          text: past ? `${word} · past the price, sells all on release` : `${word} ${approxMoney(levelPnl(moved, p, cx.spot[p.index] ?? null, delta))}`,
          color,
          icon: draggable ? 'grip' : 'lock',
        },
      ];
      if (editable(p)) parts.push({ key: 'remove', text: '×', color });
      return parts;
    },
    axisText: (at) => price(at),
  };
  return {
    id: `${p.id}:${kind}`,
    price: lvl.price,
    data,
    zLevel: 2,
    onMove: (at) => cx.actions.move(p, kind, { price: at, trigger: lvl.trigger }),
    onPart: (key) => key === 'remove' && cx.actions.clear(p, kind),
  };
}

/** A scalp's position line: avg on its option's chart, where the index was
 * when it was bought on the index's chart. "+ Stop" / "+ Target" ride on it. */
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
      if (add && !p.chart?.target) parts.push({ key: 'add-target', text: '+ Target', color: C.profit });
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
      const onIndex = cx.sym.kind === 'index' && cx.sym.index === p.index;
      if (!onOption && !onIndex) continue;
      const trigger: ChartLevel['trigger'] = onOption ? 'premium' : 'index';
      if (onOption) out.push(positionLine(p, p.avg, trigger, cx));
      else {
        const bought = firstBuy(p, cx.fills);
        if (bought != null) out.push(positionLine(p, { at: bought }, trigger, cx));
      }
      for (const kind of ['stop', 'target'] as const) {
        const lvl = p.chart?.[kind];
        if (lvl && lvl.trigger === trigger) out.push(levelLine(p, kind, lvl, cx));
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
  for (const f of cx.fills) {
    if (f.index !== s.index) continue;
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
