// v2's calls to the backend. Reads come from the new /api/v2/* views; every
// action goes to the same endpoint v1 uses, unchanged.

import { apiFetch, apiFetchWith } from '../../lib/api';
import { isTokenValid, getToken } from '../../lib/auth';
import { clearUserToken, getUserToken, validUserToken } from '../../lib/session';
import type { ReconcileApplyItem, ReconcileFinding } from '../../types';
import type { ChartLevel } from '../../types';
import type { TradingConfig } from '../../types';
import type {
  AdminUser,
  ChainAnswer,
  ExpiriesAnswer,
  DeskAnswer,
  FeedAnswer,
  Flags,
  GroupId,
  HistoryAnswer,
  IndicatorAnswer,
  KotakState,
  MarketIndex,
  MarketLive,
  MarksAnswer,
  MeAnswer,
  ModeCheck,
  OptionChainAnswer,
  RangeId,
  SessionUser,
  StatusAnswer,
  SystemAnswer,
  TelegramState,
  TrendAnswer,
} from './types';

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

/**
 * Which sign-in a call speaks for: `owner` is the passkey token (v1's, with
 * its 401 handling), `user` the phone sign-in's token, and `auto` the
 * passkey when it's held, else the phone token — the right one for every
 * read, since the passkey may do everything a phone token may.
 */
export type Auth = 'auto' | 'owner' | 'user';

/** The token a call of `auth` sends: `null` means none is held. */
export function tokenFor(auth: Auth): { token: string | null; kind: 'owner' | 'user' | 'none' } {
  if (auth !== 'user' && isTokenValid()) return { token: getToken(), kind: 'owner' };
  if (auth !== 'owner') {
    const user = validUserToken();
    if (user) return { token: user, kind: 'user' };
  }
  return { token: null, kind: 'none' };
}

/** The token the live streams put in their URL — the same choice as `auto`. */
export const streamToken = () => tokenFor('auto').token;

/** A phone token the server has refused: it's no longer good, so drop it
 * (unless a newer one was stored meanwhile) and tell the identity provider. */
function userTokenRefused(token: string | null, isMutation: boolean) {
  if (!token || getUserToken() !== token) return;
  clearUserToken();
  window.dispatchEvent(new CustomEvent('at_user_unauthorized', { detail: { isMutation } }));
}

async function getJson<T>(serverBase: string, path: string, auth: Auth = 'auto'): Promise<T> {
  const { token, kind } = tokenFor(auth);
  let res: Response;
  try {
    res = kind === 'owner' ? await apiFetch(serverBase, path) : await apiFetchWith(token, serverBase, path);
  } catch {
    throw new ApiError('Could not reach the server', 0);
  }
  if (res.status === 401 && kind === 'user') userTokenRefused(token, false);
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
export const getExpiries = (base: string, index: string) => getJson<ExpiriesAnswer>(base, `/api/v2/expiries?index=${encodeURIComponent(index)}`);
/** One expiry's strikes; the nearest when `expiry` is left out. */
export const getChain = (base: string, index: string, expiry?: string | null) =>
  getJson<ChainAnswer>(base, `/api/v2/chain?index=${encodeURIComponent(index)}${expiry ? `&expiry=${encodeURIComponent(expiry)}` : ''}`);
export const getMarks = (base: string) => getJson<MarksAnswer>(base, '/api/v2/marks');
export const getSystem = (base: string) => getJson<SystemAnswer>(base, '/api/v2/system');
export const getModeCheck = (base: string) => getJson<ModeCheck>(base, '/api/v2/mode-check');
export const getVersion = (base: string) => getJson<{ version: string }>(base, '/api/version');

export const getIndicator = (base: string) => getJson<IndicatorAnswer>(base, '/api/market/indicator');
export const getMarketLive = (base: string) => getJson<MarketLive>(base, '/api/market/live');
export const getOptionChain = (base: string, index: MarketIndex) =>
  getJson<OptionChainAnswer>(base, `/api/market/option-chain?underlying=${index}`);
/** `window` strikes each side of ATM (0 = all), checks every `interval` minutes. */
export const getTrend = (base: string, index: MarketIndex, interval: number, window: number) =>
  getJson<TrendAnswer>(base, `/api/market/intraday-trend?underlying=${index}&interval=${interval}&window=${window}`);

/** The whole trading config, as `POST /api/settings` takes it back. */
export const getSettings = (base: string) => getJson<TradingConfig>(base, '/api/settings');
export const getKotak = (base: string) => getJson<KotakState>(base, '/api/auth/kotak');
export const getTelegram = (base: string) => getJson<TelegramState>(base, '/api/auth/telegram/status');
export const getWalletBalance = (base: string) => getJson<{ balance: number }>(base, '/api/wallet/balance');

/** How an action went. `status` 202 means LIVE accepted it and the engine is
 * placing the order; 0 means the server couldn't be reached. */
export interface Sent {
  ok: boolean;
  status: number;
  error?: string;
  body?: unknown;
}

async function send(base: string, path: string, method: string, body?: unknown, auth: Auth = 'auto'): Promise<Sent> {
  const { token, kind } = tokenFor(auth);
  const init: RequestInit = {
    method,
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  };
  try {
    const res = kind === 'owner' ? await apiFetch(base, path, init) : await apiFetchWith(token, base, path, init);
    if (res.status === 401 && kind === 'user') userTokenRefused(token, true);
    if (!res.ok) return { ok: false, status: res.status, error: await errorOf(res) };
    return { ok: true, status: res.status, body: await res.json().catch(() => null) };
  } catch {
    return { ok: false, status: 0, error: 'Could not reach the server' };
  }
}

// ── Phone sign-in and the allow-list ─────────────────────────────────── //

/** Who the caller is, by the token `auto` would send. */
export const getMe = (base: string) => getJson<MeAnswer>(base, '/api/auth/me');

/** How a code request went: `retryAfter` is when "Send again" may be offered;
 * `lockedForSecs` comes with a 429. */
export interface OtpRequested {
  ok: boolean;
  status: number;
  error?: string;
  retryAfter?: number;
  lockedForSecs?: number;
}

export async function requestOtp(base: string, phone: string): Promise<OtpRequested> {
  try {
    const res = await apiFetchWith(null, base, '/api/auth/otp/request', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ phone }),
    });
    if (res.ok) {
      const data = (await res.json().catch(() => ({}))) as { retry_after?: number };
      return { ok: true, status: res.status, retryAfter: typeof data.retry_after === 'number' ? data.retry_after : 30 };
    }
    if (res.status === 429) {
      const secs = parseInt(res.headers.get('Retry-After') || '600', 10);
      return { ok: false, status: 429, error: 'Too many attempts.', lockedForSecs: Number.isFinite(secs) ? secs : 600 };
    }
    return { ok: false, status: res.status, error: await errorOf(res) };
  } catch {
    return { ok: false, status: 0, error: 'Could not reach the server' };
  }
}

export async function verifyOtp(base: string, phone: string, code: string): Promise<{ token: string; user: SessionUser } | { error: string; status: number; lockedForSecs?: number }> {
  try {
    const res = await apiFetchWith(null, base, '/api/auth/otp/verify', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ phone, code }),
    });
    if (res.ok) return (await res.json()) as { token: string; user: SessionUser };
    if (res.status === 429) {
      const secs = parseInt(res.headers.get('Retry-After') || '600', 10);
      return { error: 'Too many attempts.', status: 429, lockedForSecs: Number.isFinite(secs) ? secs : 600 };
    }
    return { error: await errorOf(res), status: res.status };
  } catch {
    return { error: 'Could not reach the server', status: 0 };
  }
}

export const listUsers = (base: string) => getJson<AdminUser[]>(base, '/api/admin/users');
export const createUser = (base: string, phone: string, name: string | null) => send(base, '/api/admin/users', 'POST', { phone, name });
export const patchUser = (base: string, id: number, patch: { name?: string; enabled?: boolean }) => send(base, `/api/admin/users/${id}`, 'PATCH', patch);
export const deleteUser = (base: string, id: number) => send(base, `/api/admin/users/${id}`, 'DELETE');

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

/** Replace the whole trading config — v1's save. Send back every field,
 * including those v2 doesn't show. */
export const saveSettings = (base: string, cfg: TradingConfig) => send(base, '/api/settings', 'POST', cfg);
/** Turn one switch on or off; a switch left out stays as it is. */
/** The paper wallet's balance — v1's virtual balance. */
export const setWalletBalance = (base: string, balance: number) => send(base, '/api/wallet/balance', 'POST', { balance });
export const putFlags = (base: string, change: Partial<Flags>) => send(base, '/api/v2/flags', 'PUT', change);
/** Log in to Kotak with the server's saved credentials — v1's Auto Connect. */
export const kotakAutoLogin = (base: string) => send(base, '/api/auth/kotak/auto-login', 'POST');
export const kotakDisconnect = (base: string) => send(base, '/api/auth/kotak/disconnect', 'DELETE');

export const engageKillSwitch = (base: string) => send(base, '/api/kill-switch', 'POST');
export const resumeTrading = (base: string) => send(base, '/api/kill-switch/reset', 'POST');

export async function reconcilePreview(base: string): Promise<{ findings: ReconcileFinding[] } | { error: string }> {
  try {
    // An owner read: the passkey when held, else the admin's phone token.
    const { token, kind } = tokenFor('auto');
    const path = '/api/positions/reconcile/preview';
    const res = kind === 'owner' ? await apiFetch(base, path) : await apiFetchWith(token, base, path);
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
