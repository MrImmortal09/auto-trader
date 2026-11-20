// The caller's Hyperliquid perps on the Trade screen: their account (polled
// every second while the `hyperliquid` switch is on), what can be done to it,
// and what the perps' charts draw — the position with its stop, targets and
// liquidation, resting limits and line orders, each line dragged to move it.

import { useCallback, useState } from 'react';
import {
  getPerpAccount,
  perpCancelLine,
  perpCancelOrder,
  perpClose,
  perpLevels,
  perpLine,
  perpMoveOrder,
  perpOrder,
  perpReset,
  perpUpdateLine,
  type PerpLevelsUpdate,
  type PerpLineBody,
  type PerpOrderBody,
  type Sent,
} from '../../lib/api';
import type { PerpAccount, PerpLevels, PerpLine, PerpOrder, PerpPosition, PerpSide, PerpTarget } from '../../lib/types';
import { usePoll } from '../../lib/usePoll';
import { useGuard } from '../../shell/useGuard';
import { useToast } from '../../ui/toast';
import { C, type LinePart, type OrderLineData } from '../chartSetup';
import type { PlacingPrompt } from '../ChartPanel';
import type { ChartLine } from '../KChart';
import type { PerpKey } from '../model';
import { usePerpDraft, type PerpDraft } from './draft';
import { estLiq, perpLabel, perpPrice, pnlAt, roundPrice, sideWord, signedUsd, sizeText } from './model';

/** A step of the ticket's "Buy on a line": the line, then its stop, then
 * its target, each one tap on the chart. */
export type LineStep = 'line' | 'stop' | 'target';

/** How a picked price is asked for: its words on the chart, and the step
 * of a "Buy on a line" it is. */
export interface PickOptions {
  /** The prompt's words at a price (`what · price` when left out). */
  label?: (price: number) => string;
  color?: string;
  step?: LineStep;
}

/** A level being put on a perp's chart: the next click sets it. `pick`
 * hands the price to `onPick` (the ticket's limit or line price, or a step
 * of a "Buy on a line"); `onPick` answering `true` keeps the chart taking
 * clicks (it has asked for the next step, or refused this one). */
export type PerpPlacing =
  | { key: PerpKey; kind: 'stop' | 'target' }
  | { key: PerpKey; kind: 'pick'; what: string; onPick: (price: number) => boolean | void; options?: PickOptions };

export interface PerpActions {
  order: (body: PerpOrderBody) => Promise<boolean>;
  close: (market: PerpKey, size?: number) => Promise<boolean>;
  setLevels: (market: PerpKey, update: PerpLevelsUpdate) => Promise<boolean>;
  line: (body: PerpLineBody) => Promise<boolean>;
  updateLine: (id: string, update: PerpLevelsUpdate) => Promise<boolean>;
  moveOrder: (id: string, price: number) => Promise<boolean>;
  cancelOrder: (id: string) => Promise<boolean>;
  cancelLine: (id: string) => Promise<boolean>;
  reset: () => Promise<boolean>;
}

export interface Perps {
  /** Null until loaded, or while the switch is off. */
  account: PerpAccount | null;
  error: string | null;
  actions: PerpActions;
  placing: PerpPlacing | null;
  /** Put a stop or target on `key`'s chart with the next click. */
  place: (key: PerpKey, kind: 'stop' | 'target') => void;
  /** Take the next click on `key`'s chart as a price for `onPick`. */
  pick: (key: PerpKey, what: string, onPick: (price: number) => boolean | void, options?: PickOptions) => void;
  cancelPlacing: () => void;
  /** What `key`'s chart draws. */
  lines: (key: PerpKey) => ChartLine[];
  /** The prompt for `key`'s chart, while something is being placed on it. */
  prompt: (key: PerpKey) => PlacingPrompt | null;
}

const lock = (editable: boolean) => (editable ? ('grip' as const) : ('lock' as const));

/** A position's or a line order's levels as the chart draws them. */
function levelLines(
  key: PerpKey,
  owner: string,
  side: PerpSide,
  size: number,
  entry: number,
  levels: PerpLevels | undefined,
  onLevels: (update: PerpLevelsUpdate) => void,
  canRemoveTarget: (count: number) => boolean,
  editable = true,
): ChartLine[] {
  const out: ChartLine[] = [];
  const stop = levels?.stop;
  if (stop != null) {
    out.push({
      id: `${owner}:stop`,
      price: stop,
      zLevel: 2,
      data: {
        color: C.loss,
        dashed: true,
        draggable: editable,
        parts: (at) => {
          const parts: LinePart[] = [{ key: 'label', text: `Stop ${signedUsd(pnlAt(side, size, entry, at))}`, color: C.loss, icon: lock(editable) }];
          if (editable) parts.push({ key: 'remove', text: '×', color: C.loss });
          return parts;
        },
        axisText: (at) => perpPrice(at),
      },
      onMove: (at) => onLevels({ stop: roundPrice(key, at) }),
      onPart: (part) => part === 'remove' && onLevels({ stop: null }),
    });
  }
  const targets: PerpTarget[] = levels?.targets ?? [];
  const sized = targets.reduce((n, t) => n + (t.size ?? 0), 0);
  for (const t of targets) {
    const sells = t.size ?? Math.max(0, size - sized);
    const parts = (at: number): LinePart[] => {
      const out: LinePart[] = [{ key: 'label', text: `Target · ${t.size == null ? 'all left' : sizeText(key, t.size)} ${signedUsd(pnlAt(side, sells, entry, at))}`, color: C.profit, icon: lock(editable) }];
      if (editable && canRemoveTarget(targets.length)) out.push({ key: 'remove', text: '×', color: C.profit });
      return out;
    };
    out.push({
      id: `${owner}:target:${t.id}`,
      price: t.price,
      zLevel: 2,
      data: { color: C.profit, dashed: true, draggable: editable, parts, axisText: (at) => perpPrice(at) },
      onMove: (at) => onLevels({ set_target: { id: t.id, price: roundPrice(key, at) } }),
      onPart: (part) => part === 'remove' && onLevels({ remove_target: t.id }),
    });
  }
  return out;
}

export function usePerps({ serverBase, enabled, onShow, market }: {
  serverBase: string;
  enabled: boolean;
  /** The ticket's perp: LIVE, the account says what a new position in it can use. */
  market?: PerpKey | null;
  /** Something is about to be placed on `key`'s chart: make sure one shows it. */
  onShow?: (key: PerpKey) => void;
}): Perps {
  const guard = useGuard();
  const toast = useToast();
  const poll = usePoll(() => getPerpAccount(serverBase, market), 1_000, serverBase, enabled);
  const account = enabled ? poll.data : null;
  // Every line moves and cancels, on paper and LIVE alike (LIVE: on
  // Hyperliquid, by its order id).
  const editable = true;
  const where = account?.mode === 'live' ? `LIVE · ${account.network}` : 'paper';
  const [placing, setPlacing] = useState<PerpPlacing | null>(null);
  // The ticket's order being set up: its liquidation goes on the chart.
  const draft = usePerpDraft();
  const cancelPlacing = useCallback(() => setPlacing(null), []);

  async function run(act: () => Promise<Sent>, fail: string, done?: (body: unknown) => void): Promise<boolean> {
    const sent = await guard('Sign in to trade perps on paper.', act, fail, 'signed');
    if (!sent?.ok) return false;
    done?.(sent.body);
    // LIVE: what Hyperliquid refused beside the order (a stop or target not placed) is said loudly.
    for (const w of (sent.body as { warnings?: string[] } | null)?.warnings ?? []) toast({ tone: 'loss', title: 'Check the position on Hyperliquid', body: w });
    void poll.refresh();
    return true;
  }

  /** "Bought 0.01 BTC-PERP at 63,768.5" for what an order filled now. */
  function filledToast(body: unknown, fallback: { title: string; body: string }) {
    const legs = ((body as { filled?: { action: string; size: number; price: number; market: string }[] } | null)?.filled ?? []).filter(Boolean);
    if (legs.length === 0) {
      toast({ tone: 'neutral', ...fallback });
      return;
    }
    for (const l of legs) toast({ tone: l.action === 'BUY' ? 'profit' : 'loss', title: `${l.action === 'BUY' ? 'Bought' : 'Sold'} ${l.size} ${perpLabel(l.market)}`, body: `At ${perpPrice(l.price)} (${where})` });
  }

  const actions: PerpActions = {
    order: (body) =>
      run(
        () => perpOrder(serverBase, body),
        `${perpLabel(body.market)} order not placed`,
        (b) => filledToast(b, { title: `Limit ${body.side} ${body.size} ${perpLabel(body.market)} resting`, body: `At ${perpPrice(body.price ?? 0)} — it fills when the price reaches it` }),
      ),
    close: (market, size) => run(() => perpClose(serverBase, market, size), `${perpLabel(market)} not closed`, (b) => filledToast(b, { title: `${perpLabel(market)} closed`, body: '' })),
    setLevels: (market, update) => run(() => perpLevels(serverBase, market, update), `${perpLabel(market)} stop or target not set`),
    line: (body) =>
      run(() => perpLine(serverBase, body), `${perpLabel(body.market)} line order not placed`, () =>
        toast({ tone: 'neutral', title: `Line order: ${body.side} ${body.size} ${perpLabel(body.market)}`, body: `At ${perpPrice(body.price)} — a market order once the price crosses it` }),
      ),
    updateLine: (id, update) => run(() => perpUpdateLine(serverBase, id, update), 'Line order not changed'),
    moveOrder: (id, price) => run(() => perpMoveOrder(serverBase, id, price), 'Limit not moved', (b) => filledToast(b, { title: 'Limit moved', body: `To ${perpPrice(price)}` })),
    cancelOrder: (id) => run(() => perpCancelOrder(serverBase, id), 'Limit not cancelled'),
    cancelLine: (id) => run(() => perpCancelLine(serverBase, id), 'Line order not cancelled'),
    reset: () => run(() => perpReset(serverBase), 'Perps wallet not reset', () => toast({ tone: 'neutral', title: 'Perps wallet started over', body: 'Back to 10,000 USDC (paper)' })),
  };

  function positionLines(p: PerpPosition): ChartLine[] {
    const key = p.market;
    const pnlColor = p.upnl == null ? C.fg : p.upnl < 0 ? C.loss : C.profit;
    const out: ChartLine[] = [
      {
        id: `${p.id}:position`,
        price: p.entry,
        zLevel: 1,
        data: {
          color: C.fg,
          dashed: false,
          draggable: false,
          parts: () => {
            const parts: LinePart[] = [
              { key: 'label', text: `${sideWord(p.side)} ${sizeText(key, p.size)} · ${p.leverage}x · entry ${perpPrice(p.entry)}`, color: C.fg },
              { key: 'pnl', text: p.upnl == null ? '—' : signedUsd(p.upnl), color: pnlColor },
            ];
            // LIVE: stops that don't cover the position as one.
            if (p.stop_note) parts.push({ key: 'stop-note', text: '⚠ stop not over all of it', color: C.loss });
            if (editable && p.levels?.stop == null) parts.push({ key: 'add-stop', text: '+ Stop', color: C.loss });
            if (editable) parts.push({ key: 'add-target', text: '+ Target', color: C.profit });
            if (editable) parts.push({ key: 'close', text: 'Close', color: C.loss, filled: true });
            return parts;
          },
          axisText: (at) => perpPrice(at),
        },
        onPart: (part) => {
          if (part === 'add-stop') setPlacing({ key, kind: 'stop' });
          if (part === 'add-target') setPlacing({ key, kind: 'target' });
          if (part === 'close') void actions.close(key);
        },
      },
    ];
    if (p.liq != null) out.push(liqLine(key, `${p.id}:liq`, p.liq, `Liquidation · ${signedUsd(-p.margin)}`));
    out.push(...levelLines(key, p.id, p.side, p.size, p.entry, p.levels, (u) => void actions.setLevels(key, u), () => true, editable));
    return out;
  }

  /** A dotted line where a position would be liquidated: never dragged. */
  function liqLine(key: PerpKey, id: string, at: number, text: string): ChartLine {
    return {
      id,
      price: roundPrice(key, at),
      zLevel: 1,
      data: {
        color: C.loss,
        dashed: false,
        dotted: true,
        draggable: false,
        parts: () => [{ key: 'label', text, color: C.loss, icon: 'lock' }],
        axisText: (v) => perpPrice(v),
      },
    };
  }

  /** Where an order that opens a position (a resting limit, an armed line)
   * would have it liquidated once filled at `entry` — while nothing is held
   * in it (an add or a flip lands elsewhere) and its leverage is known. */
  function fillLiqLine(key: PerpKey, id: string, side: PerpSide, entry: number, leverage: number, what: string): ChartLine[] {
    if (leverage <= 0 || account?.positions.some((p) => p.market === key)) return [];
    const liq = estLiq(key, side, entry, leverage);
    return liq == null ? [] : [liqLine(key, id, liq, `Liquidation if ${what} fills · ${leverage}x`)];
  }

  /** The ticket's order being set up: a line order's line, stop and target
   * (not yet armed: dashed, fixed), and where it would be liquidated. */
  function draftLines(key: PerpKey, d: PerpDraft): ChartLine[] {
    const out: ChartLine[] = [];
    const fixed = (id: string, at: number, color: string, text: string): ChartLine => ({
      id: `perp:${key}:draft-${id}`,
      price: roundPrice(key, at),
      zLevel: 2,
      data: { color, dashed: true, draggable: false, parts: () => [{ key: 'label', text, color }], axisText: (v) => perpPrice(v) },
    });
    if (d.line) {
      const what = d.size != null ? ` ${sizeText(key, d.size)}` : '';
      out.push(fixed('line', d.entry, C.fg, `${sideWord(d.side)}${what} on this line · ${d.leverage}x · not armed yet`));
      const pnl = (at: number) => (d.size != null ? ` ${signedUsd(pnlAt(d.side, d.size, d.entry, at))}` : '');
      if (d.stop != null) out.push(fixed('stop', d.stop, C.loss, `Stop${pnl(d.stop)}`));
      if (d.target != null) out.push(fixed('target', d.target, C.profit, `Target${pnl(d.target)}`));
    }
    if (d.liq != null) {
      const loss = d.margin != null ? ` · ${signedUsd(-d.margin)}` : '';
      out.push(liqLine(key, `perp:${key}:draft-liq`, d.liq, `Liquidation if ${d.line ? 'the line fills' : 'filled'} at ${perpPrice(d.entry)} · ${d.leverage}x${loss}`));
    }
    return out;
  }

  function orderLine(o: PerpOrder): ChartLine {
    const key = o.market;
    const data: OrderLineData = {
      color: C.accent,
      dashed: true,
      draggable: editable,
      parts: () => {
        const lev = o.reduce_only ? ' · reduce only' : o.leverage > 0 ? ` · ${o.leverage}x` : '';
        const parts: LinePart[] = [{ key: 'label', text: `Limit ${o.side === 'long' ? 'buy' : 'sell'} ${sizeText(key, o.size)}${lev}`, color: C.accent, icon: lock(editable) }];
        if (editable) parts.push({ key: 'remove', text: '×', color: C.accent });
        return parts;
      },
      axisText: (at) => perpPrice(at),
    };
    return {
      id: `${o.id}:order`,
      price: o.price,
      zLevel: 2,
      data,
      onMove: (at) => void actions.moveOrder(o.id, roundPrice(key, at)),
      onPart: (part) => part === 'remove' && void actions.cancelOrder(o.id),
    };
  }

  function lineLines(l: PerpLine): ChartLine[] {
    const key = l.market;
    const until = l.valid_until ? ` · till ${l.valid_until.slice(11, 16)}` : '';
    const out: ChartLine[] = [
      {
        id: `${l.id}:line`,
        price: l.price,
        zLevel: 2,
        data: {
          color: C.fg,
          dashed: true,
          draggable: editable,
          parts: () => {
            const lev = l.leverage > 0 ? ` · ${l.leverage}x` : '';
            const parts: LinePart[] = [{ key: 'label', text: `${sideWord(l.side)} ${sizeText(key, l.size)}${lev} when ${l.rising ? '≥' : '≤'} line${until}`, color: C.fg, icon: lock(editable) }];
            if (editable) parts.push({ key: 'remove', text: '×', color: C.fg });
            return parts;
          },
          axisText: (at) => perpPrice(at),
        },
        onMove: (at) => void actions.updateLine(l.id, { price: roundPrice(key, at) }),
        onPart: (part) => part === 'remove' && void actions.cancelLine(l.id),
      },
    ];
    out.push(...levelLines(key, l.id, l.side, l.size, l.price, l.levels, (u) => void actions.updateLine(l.id, u), () => true, editable));
    return out;
  }

  function lines(key: PerpKey): ChartLine[] {
    if (!account) return [];
    const out = [
      ...account.positions.filter((p) => p.market === key).flatMap(positionLines),
      ...account.orders.filter((o) => o.market === key).flatMap((o) => [orderLine(o), ...(o.reduce_only ? [] : fillLiqLine(key, `${o.id}:liq`, o.side, o.price, o.leverage, 'the limit'))]),
      ...account.lines.filter((l) => l.market === key).flatMap((l) => [...lineLines(l), ...fillLiqLine(key, `${l.id}:liq`, l.side, l.price, l.leverage, 'the line')]),
    ];
    // The order being set up in the ticket: where it would be liquidated —
    // unless it is placed already (an armed line or a resting limit there
    // has its own).
    const placed = (x: { side: PerpSide; price: number; leverage: number }) => draft != null && x.side === draft.side && x.price === draft.entry && x.leverage === draft.leverage;
    if (draft && draft.key === key && !account.lines.some((l) => l.market === key && placed(l)) && !account.orders.some((o) => o.market === key && placed(o))) {
      out.push(...draftLines(key, draft));
    }
    return out;
  }

  function prompt(key: PerpKey): PlacingPrompt | null {
    if (!placing || placing.key !== key) return null;
    if (!editable && placing.kind !== 'pick') return null;
    if (placing.kind === 'pick') {
      const { onPick, what, options } = placing;
      const color = options?.color ?? C.accent;
      return {
        key: `perp:${key}:pick:${options?.step ?? ''}:${what}`,
        what,
        data: {
          color,
          dashed: true,
          draggable: false,
          parts: (at) => [{ key: 'label', text: options?.label ? options.label(roundPrice(key, at)) : `${what} · ${perpPrice(roundPrice(key, at))}`, color }],
          axisText: (at) => perpPrice(at),
        },
        // `true` from `onPick`: more taps to come, so the chart keeps taking them.
        onPlace: (at) => onPick(roundPrice(key, at)) !== true,
      };
    }
    const p = account?.positions.find((x) => x.market === key);
    if (!p) return null;
    const kind = placing.kind;
    const color = kind === 'stop' ? C.loss : C.profit;
    return {
      key: `perp:${key}:${kind}`,
      what: kind,
      data: {
        color,
        dashed: true,
        draggable: false,
        parts: (at) => [{ key: 'label', text: `${kind === 'stop' ? 'Stop' : 'Target · all left'} ${signedUsd(pnlAt(p.side, p.size, p.entry, at))}`, color }],
        axisText: (at) => perpPrice(at),
      },
      onPlace: (at) => {
        const price = roundPrice(key, at);
        void actions.setLevels(key, kind === 'stop' ? { stop: price } : { set_target: { price, size: null } });
        return true;
      },
    };
  }

  return {
    account,
    error: enabled ? poll.error : null,
    actions,
    placing,
    place: (key, kind) => {
      onShow?.(key);
      setPlacing({ key, kind });
    },
    pick: (key, what, onPick, options) => {
      onShow?.(key);
      setPlacing({ key, kind: 'pick', what, onPick, options });
    },
    cancelPlacing,
    lines,
    prompt,
  };
}
