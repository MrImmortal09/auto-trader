// The engine log in words: each JSON event the backend logs becomes a kind
// (Info, Order, Signal, Error) and a sentence. Anything not known here
// falls back to its own message, or its event name and fields.

import { price, signedMoney, todayIst } from '../lib/format';
import { reasonLabel } from '../lib/labels';

export type LogKind = 'info' | 'order' | 'signal' | 'error';

export interface LogLine {
  id: string;
  /** IST `YYYY-MM-DD HH:MM:SS`. */
  at: string;
  kind: LogKind;
  text: string;
  /** The event as the server logged it. */
  raw: string;
}

export const KIND_LABEL: Record<LogKind, string> = { info: 'Info', order: 'Order', signal: 'Signal', error: 'Error' };

type Event = Record<string, unknown>;

const str = (v: unknown): string => (typeof v === 'string' ? v : v == null ? '' : String(v));
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const amount = (v: unknown) => {
  const n = num(v);
  return n == null ? '?' : price(n);
};

/** "CALL_EXPIRED — not bought within 30 min" → "not bought within 30 min". */
function reasonText(reason: unknown): string {
  const r = str(reason);
  const dash = r.indexOf(' — ');
  return dash > 0 && /^[A-Z_]+$/.test(r.slice(0, dash)) ? r.slice(dash + 3) : r;
}

/** A sentence from the event's own `message`, else its name and fields. */
function fallback(e: Event): string {
  if (typeof e.message === 'string' && e.message) return e.message;
  const name = str(e.event).replace(/_/g, ' ').toLowerCase();
  const fields = Object.entries(e)
    .filter(([k, v]) => !['event', 'level', 'mode'].includes(k) && v != null && typeof v !== 'object')
    .map(([k, v]) => `${k.replace(/_/g, ' ')} ${v}`);
  return [name.charAt(0).toUpperCase() + name.slice(1), ...fields].join(' · ');
}

const ORDER_SENT: Record<string, string> = {
  LIVE_ENTRY_PLACED: 'Buy order sent',
  LIVE_TOPUP_PLACED: 'Add order sent',
  LIVE_EXIT_PLACED: 'Sell order sent',
};

/** What one logged event says, or `null` for what the log leaves out
 * (raw Telegram messages: the Desk feed shows those). */
export function describe(e: Event): { kind: LogKind; text: string } | null {
  const event = str(e.event);
  const level = str(e.level).toUpperCase();
  const inst = str(e.instrument);
  switch (event) {
    case 'TELEGRAM_MESSAGE':
    case 'TELEGRAM_MESSAGE_EDITED':
      return null;
    case 'ENTRY':
      return { kind: 'order', text: `Bought ${e.qty} ${inst} at ${amount(e.price)}${num(e.net_cost) != null ? ` · ₹${amount(e.net_cost)} with charges` : ''}` };
    case 'TOPUP':
    case 'MANUAL_ADD':
      return { kind: 'order', text: `Added ${e.qty} ${inst} at ${amount(e.price)}${num(e.holding) != null ? ` · holding ${e.holding}` : ''}` };
    case 'LIVE_ENTRY_PLACED':
    case 'LIVE_TOPUP_PLACED':
    case 'LIVE_EXIT_PLACED': {
      const kind = str(e.order_type) ? ` · ${str(e.order_type).toLowerCase()}${num(e.limit_price) != null ? ` at ${amount(e.limit_price)}` : ''}` : '';
      const why = event === 'LIVE_EXIT_PLACED' && e.reason ? ` · ${reasonLabel(str(e.reason))}` : '';
      return { kind: 'order', text: `${ORDER_SENT[event]} · ${e.qty} ${inst}${kind}${why} · order ${str(e.order_id)}` };
    }
    case 'MANUAL_BUY_REQUESTED':
      return { kind: 'order', text: `Scalp ${str(e.action).toLowerCase() || 'buy'} · ${e.qty} ${inst}${num(e.ltp) != null ? ` near ${amount(e.ltp)}` : ''}` };
    case 'MANUAL_EXIT_ALL_REQUESTED':
      return { kind: 'order', text: `Exit all scalps · ${e.exiting ?? 0} selling, ${e.cancelled ?? 0} cancelled` };
    case 'ORPHAN_ORDER_CANCELLED':
      return { kind: 'order', text: `Cancelled stray order ${str(e.order_id)} · ${inst}` };
    case 'EXIT_ORDER_UNFILLED':
      return { kind: 'error', text: `Sell order ${str(e.order_id)} didn't fill · ${inst} · ${reasonText(e.note || e.reason)}` };
    case 'SIGNAL_SKIPPED':
      return { kind: 'signal', text: str(e.reason) === 'not_index_option' ? `${inst} call skipped · not an index option` : fallback(e) };
    case 'SIGNAL_IGNORED':
      return { kind: 'signal', text: fallback(e) };
    case 'SIGNAL_UPDATED':
      return { kind: 'signal', text: `${inst} stop moved to ${amount(e.new_sl)} by the channel` };
    case 'SIGNAL_EXITED':
      return { kind: 'signal', text: `Channel exit at ${amount(e.exit_price)} · ${inst}` };
    case 'SL_LOWER_REFUSED':
      return { kind: 'signal', text: `${inst} lower stop refused · stays ${amount(e.current_sl)}, asked ${amount(e.requested_sl)}` };
    case 'DUPLICATE_SIGNAL_SKIPPED':
      return { kind: 'signal', text: `${inst} call skipped · the same call is already open or waiting` };
    case 'INDICATOR_WAIT':
      return { kind: 'signal', text: `${inst} call waits · ${reasonText(e.reason)}` };
    case 'INDICATOR_SIZING':
      return { kind: 'signal', text: `${inst} sized · ${str(e.note)}` };
    case 'ENTRY_ABANDONED':
      return { kind: 'signal', text: `${inst} call dropped · ${reasonText(e.reason)}` };
    case 'SL_TRAILED':
      return { kind: 'info', text: `${inst} stop trailed to ${amount(e.new_sl)}${num(e.next_target) != null ? ` · next target ${amount(e.next_target)}` : ''}` };
    case 'CONFIG_UPDATED':
      return { kind: 'info', text: `Settings saved · ${str(e.mode) === 'LIVE' ? 'LIVE' : 'Paper'}` };
    case 'LIVE_STARTUP_RECONCILED':
      return { kind: 'info', text: `LIVE checked against Kotak · ${e.positions ?? 0} positions` };
    case 'LIVE_ERROR':
    case 'ERROR':
      return { kind: 'error', text: `${inst && inst !== 'SYSTEM' && inst !== 'UPDATE' ? `${inst} · ` : ''}${str(e.message) || fallback(e)}` };
  }
  // Exits log their reason as the event, with the fill.
  if (num(e.price) != null && num(e.qty) != null && num(e.pnl) != null) {
    return { kind: 'order', text: `Sold ${e.qty} ${inst} at ${amount(e.price)} · ${reasonLabel(event)} · ${signedMoney(num(e.pnl) ?? 0)} net` };
  }
  const failed = level === 'ERROR' || /_(FAILED|ERROR|DEAD)$/.test(event);
  return { kind: failed ? 'error' : 'info', text: fallback(e) };
}

/** One row of the log, from its JSON and when it was logged. */
export function logLine(raw: string, at: string, id: string): LogLine | null {
  let e: Event;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object') return { id, at, kind: 'info', text: raw, raw };
    e = parsed as Event;
  } catch {
    return { id, at, kind: 'info', text: raw, raw };
  }
  const said = describe(e);
  return said && { id, at, raw, ...said };
}

/** The log as a text file: oldest first, each line with its event. */
export function downloadLog(lines: LogLine[]) {
  const body = [...lines]
    .reverse()
    .map((l) => `${l.at}  ${KIND_LABEL[l.kind].padEnd(6)}  ${l.text}  ${l.raw}`)
    .join('\n');
  const url = URL.createObjectURL(new Blob([`${body}\n`], { type: 'text/plain' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = `engine-log-${todayIst()}.txt`;
  a.click();
  URL.revokeObjectURL(url);
}
