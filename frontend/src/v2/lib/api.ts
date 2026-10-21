// v2's calls to the backend. Reads come from the new /api/v2/* views; every
// action goes to the same endpoint v1 uses, unchanged.

import { apiFetch } from '../../lib/api';
import type { ReconcileApplyItem, ReconcileFinding } from '../../types';
import type { ChartLevel } from '../../types';
import type { ChainAnswer, DeskAnswer, FeedAnswer, Flags, GroupId, HistoryAnswer, MarksAnswer, RangeId, StatusAnswer } from './types';

export class ApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

async function errorOf(res: Response): Promise<string> {
  const body = (await res.json().catch(() => null)) as { error?: unknown } | null;
  return typeof body?.error === 'string' ? body.error : `Request failed (${res.status})`;
}

async function getJson<T>(serverBase: string, path: string): Promise<T> {
  let res: Response;
  try {
    res = await apiFetch(serverBase, path);
  } catch {
    throw new ApiError('Could not reach the server', 0);
  }
  if (!res.ok) throw new ApiError(await errorOf(res), res.status);
  return (await res.json()) as T;
}

export const getDesk = (base: string) => getJson<DeskAnswer>(base, '/api/v2/desk');
export const getFeed = (base: string) => getJson<FeedAnswer>(base, '/api/v2/feed');
export const getStatus = (base: string, after: number | null) =>
  getJson<StatusAnswer>(base, after == null ? '/api/v2/status' : `/api/v2/status?after=${after}`);
export const getHistory = (base: string, range: RangeId, group: GroupId) =>
  getJson<HistoryAnswer>(base, `/api/v2/history?range=${range}&group=${group}`);
export const getFlags = (base: string) => getJson<Flags>(base, '/api/v2/flags');
export const getChain = (base: string, index: string) => getJson<ChainAnswer>(base, `/api/v2/chain?index=${encodeURIComponent(index)}`);
export const getMarks = (base: string) => getJson<MarksAnswer>(base, '/api/v2/marks');

/** How an action went. `status` 202 means LIVE accepted it and the engine is
 * placing the order; 0 means the server couldn't be reached. */
export interface Sent {
  ok: boolean;
  status: number;
  error?: string;
  body?: unknown;
}

async function send(base: string, path: string, method: string, body?: unknown): Promise<Sent> {
  try {
    const res = await apiFetch(base, path, {
      method,
      headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) return { ok: false, status: res.status, error: await errorOf(res) };
    return { ok: true, status: res.status, body: await res.json().catch(() => null) };
  } catch {
    return { ok: false, status: 0, error: 'Could not reach the server' };
  }
}

const position = (id: string) => `/api/positions/${encodeURIComponent(id)}`;

/** Sell `qty` of an open position at market — v1's partial sell. */
export const sellQty = (base: string, id: string, qty: number) => send(base, `${position(id)}/sell`, 'POST', { qty });
/** Sell all of an open position at market — v1's Close. */
export const exitPosition = (base: string, id: string) => send(base, `${position(id)}/close`, 'POST');
/** Drop a call still waiting for its entry — v1's Cancel. */
export const cancelPosition = (base: string, id: string) => send(base, position(id), 'DELETE');
/** Lots a waiting call buys, or `null` for the default — v1's quantity override. */
export const setLots = (base: string, id: string, lots: number | null) => send(base, position(id), 'PATCH', { override_qty: lots });

/** One tap on the ladder: a market buy of `lots`, or an add to the scalp
 * already held on that contract — v1's Execute buy. */
export const executeBuy = (base: string, tradingSymbol: string, lots: number) =>
  send(base, '/api/execute/buy', 'POST', { trading_symbol: tradingSymbol, lots });
/** Sell every scalp at market — v1's Exit all. */
export const executeExitAll = (base: string) => send(base, '/api/execute/exit-all', 'POST');
/** Set (`{price, trigger}`), clear (`null`) or keep (left out) a scalp's chart
 * stop and target — PR 4's endpoint, behind the `chart_stops` switch. */
export const setChartLevels = (base: string, id: string, levels: { stop?: ChartLevel | null; target?: ChartLevel | null }) =>
  send(base, `/api/v2/positions/${encodeURIComponent(id)}`, 'PATCH', levels);

export const engageKillSwitch = (base: string) => send(base, '/api/kill-switch', 'POST');
export const resumeTrading = (base: string) => send(base, '/api/kill-switch/reset', 'POST');

export async function reconcilePreview(base: string): Promise<{ findings: ReconcileFinding[] } | { error: string }> {
  try {
    const res = await apiFetch(base, '/api/positions/reconcile/preview');
    const data = await res.json().catch(() => null);
    if (!res.ok) return { error: (data as { error?: string } | null)?.error ?? 'Failed to fetch live data from Kotak' };
    return { findings: Array.isArray(data) ? (data as ReconcileFinding[]) : [] };
  } catch {
    return { error: 'Network error contacting the server' };
  }
}

export const reconcileApply = (base: string, items: ReconcileApplyItem[]) =>
  send(base, '/api/positions/reconcile/apply', 'POST', items);

/** `POST /api/auth/verify-passkey`: the token, or why not. */
export async function verifyPasskey(
  base: string,
  passkey: string,
): Promise<{ token: string } | { error: string; lockedForSecs?: number }> {
  try {
    const res = await apiFetch(base, '/api/auth/verify-passkey', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ passkey }),
    });
    if (res.ok) {
      const data = (await res.json()) as { token: string };
      return { token: data.token };
    }
    if (res.status === 429) {
      const secs = parseInt(res.headers.get('Retry-After') || '900', 10);
      return { error: 'Too many attempts.', lockedForSecs: secs };
    }
    if (res.status === 500) return { error: 'Server error — is AUTH_SECRET set on the backend?' };
    return { error: 'That passkey is not right.' };
  } catch {
    return { error: 'Could not reach the server.' };
  }
}
