// ---------------------------------------------------------------------------
// Execute screen: follow each tap from "sent" to its outcome by watching the
// positions in the live stream, and turn that into toasts.
// ---------------------------------------------------------------------------

import type { ExecuteFrame, ExecutePosition } from '../types';
import { fmtPnl } from './execute';

export type OrderKind = 'buy' | 'add' | 'exit' | 'sell';

export interface TrackedOrder {
  /** Also the id of the toast that shows it. */
  key: string;
  kind: OrderKind;
  positionId: string;
  label: string;
  /** Underlying the engine logs errors under (`NIFTY`). */
  index: string;
  /** Quantity held when the tap went out. */
  baseQty: number;
  /** Frame clock (server ms) at the tap — timeouts and error matching use it. */
  startTs: number;
  /** The position has been seen in a frame since the tap. */
  seen: boolean;
  lastPnl: number | null;
}

export type ToastTone = 'pending' | 'ok' | 'err';

export interface Toast {
  key: string;
  tone: ToastTone;
  title: string;
  detail?: string;
}

/** A tap with no outcome after this long is reported as unconfirmed. */
const GIVE_UP_MS = 30_000;
/** Grace before "nothing happened" counts as a failure. */
const SETTLE_MS = 2_500;

/** Why this order failed: an engine error about this very position, else
 * the latest one on its underlying that names no position (labelled as such). */
function errorFor(frame: ExecuteFrame, o: TrackedOrder): string | undefined {
  const since = frame.errors.filter((e) => e.ts >= o.startTs - 1_000);
  const mine = since.filter((e) => e.position_id === o.positionId);
  if (mine.length) return mine[mine.length - 1].message;
  const general = since.filter((e) => e.position_id == null && e.instrument === o.index);
  return general.length ? `Latest ${o.index} engine error: ${general[general.length - 1].message}` : undefined;
}

function done(o: TrackedOrder, tone: ToastTone, title: string, detail?: string): Toast {
  return { key: o.key, tone, title, detail };
}

/** One frame's worth of progress: orders still open, and toasts to show or replace. */
export function advanceOrders(orders: TrackedOrder[], frame: ExecuteFrame): { orders: TrackedOrder[]; toasts: Toast[] } {
  const still: TrackedOrder[] = [];
  const toasts: Toast[] = [];
  for (const o of orders) {
    const p: ExecutePosition | undefined = frame.positions.find((x) => x.id === o.positionId);
    const elapsed = frame.ts - o.startTs;
    const err = () => errorFor(frame, o) ?? 'See the engine log below for the reason.';
    let result: Toast | null = null;

    switch (o.kind) {
      case 'buy':
        if (p && !p.placing && p.qty > 0) {
          result = done(o, 'ok', `Bought ${p.qty} ${o.label}`, `avg ₹${p.avg.toFixed(2)}`);
        } else if (!p && (o.seen || elapsed > SETTLE_MS)) {
          result = done(o, 'err', `${o.label} not bought`, err());
        }
        break;
      case 'add':
        if (!p) {
          result = done(o, 'err', `${o.label} closed before the add`);
        } else if (p.qty > o.baseQty) {
          result = done(o, 'ok', `Added ${p.qty - o.baseQty} ${o.label}`, `now ${p.qty} @ avg ₹${p.avg.toFixed(2)}`);
        } else if (!p.adding && elapsed > SETTLE_MS) {
          result = done(o, 'err', `Add to ${o.label} not placed`, err());
        }
        break;
      case 'exit':
        if (!p) {
          result = done(o, 'ok', `Exited ${o.label}`, o.lastPnl != null ? `≈ ${fmtPnl(o.lastPnl)} at the last price` : undefined);
        } else if (!p.exiting && elapsed > SETTLE_MS) {
          result = done(o, 'err', `${o.label} still open`, err());
        }
        break;
      case 'sell':
        if (!p) {
          result = done(o, 'ok', `Exited ${o.label}`);
        } else if (p.qty < o.baseQty) {
          result = done(o, 'ok', `Sold ${o.baseQty - p.qty} ${o.label}`, `${p.qty} left`);
        } else if (!p.exiting && elapsed > SETTLE_MS) {
          result = done(o, 'err', `Sell of ${o.label} not done`, err());
        }
        break;
    }
    if (!result && elapsed > GIVE_UP_MS) {
      result = done(o, 'err', `${o.label}: no confirmation yet`, 'Check the engine log and your Kotak order book.');
    }
    if (result) {
      toasts.push(result);
    } else {
      still.push({ ...o, seen: o.seen || Boolean(p), lastPnl: p?.pnl ?? o.lastPnl });
    }
  }
  return { orders: still, toasts };
}
