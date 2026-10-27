// ---------------------------------------------------------------------------
import { getToken, clearToken } from './auth';
// API helpers — server base persistence and fetch wrapper
// ---------------------------------------------------------------------------

export const SERVER_BASE_STORAGE_KEY = 'server_base';
export const SERVER_BASE_COOKIE = 'server_base';
export const DEFAULT_SERVER_BASE = 'https://m1.axiosiiitl.dev';

export function readCookie(name: string) {
  if (typeof document === 'undefined') return '';
  const prefix = `${name}=`;
  const entry = document.cookie.split('; ').find((item) => item.startsWith(prefix));
  return entry ? decodeURIComponent(entry.slice(prefix.length)) : '';
}

export function normalizeServerBase(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return '';
  return trimmed.replace(/\/+$/, '');
}

export function isValidServerBase(value: string) {
  const normalized = normalizeServerBase(value);
  if (!normalized) return true;

  try {
    const parsed = new URL(normalized);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

export function getStoredServerBase() {
  if (typeof window === 'undefined') return '';
  const saved = window.localStorage.getItem(SERVER_BASE_STORAGE_KEY);
  if (saved !== null && saved !== undefined && saved !== '') {
    // If the user previously had the remote default saved while running on localhost,
    // default back to local backend ("") so it connects to the local dev server.
    if (
      (window.location.hostname === 'localhost' || window.location.hostname === '127.0.0.1') &&
      saved === DEFAULT_SERVER_BASE
    ) {
      return '';
    }
    return normalizeServerBase(saved);
  }
  const cookie = readCookie(SERVER_BASE_COOKIE);
  if (cookie) return normalizeServerBase(cookie);
  if (import.meta.env.VITE_API_BASE_URL) return normalizeServerBase(import.meta.env.VITE_API_BASE_URL);

  // When running locally on localhost/127.0.0.1, default to "" so Vite's proxy forwards to local port 8080
  if (window.location.hostname === 'localhost' || window.location.hostname === '127.0.0.1') {
    return '';
  }

  return normalizeServerBase(DEFAULT_SERVER_BASE);
}

export function persistServerBase(value: string) {
  const normalized = normalizeServerBase(value);

  if (typeof window !== 'undefined') {
    if (normalized) window.localStorage.setItem(SERVER_BASE_STORAGE_KEY, normalized);
    else window.localStorage.removeItem(SERVER_BASE_STORAGE_KEY);
    window.dispatchEvent(new CustomEvent('at_server_base_change', { detail: { serverBase: normalized } }));
  }

  if (typeof document !== 'undefined') {
    document.cookie = normalized
      ? `${SERVER_BASE_COOKIE}=${encodeURIComponent(normalized)}; path=/; max-age=31536000; SameSite=Lax`
      : `${SERVER_BASE_COOKIE}=; path=/; max-age=0; SameSite=Lax`;
  }

  return normalized;
}

export function apiUrl(serverBase: string, path: string) {
  const normalized = normalizeServerBase(serverBase);
  if (normalized && !isValidServerBase(normalized)) return path;
  return normalized ? `${normalized}${path}` : path;
}

export function apiFetch(serverBase: string, path: string, init?: RequestInit) {
  const token = getToken();
  const headers = new Headers(init?.headers);
  if (token) headers.set('Authorization', `Bearer ${token}`);
  
  return fetch(apiUrl(serverBase, path), { ...init, headers }).then(res => {
    if (res.status === 401) {
      // 1. Passkey verification endpoint handles its own 401 (invalid passkey) — never clear session
      // 2. A 401 to a request that carried the stored token means the token is
      //    no longer good (expired, or the server's secret changed), whatever
      //    the method: the owner's reads are gated too, so a background poll
      //    is as telling as an order. Without a token there is nothing to drop.
      const method = (init?.method || 'GET').toUpperCase();
      const isMutation = ['POST', 'PUT', 'PATCH', 'DELETE'].includes(method);
      const isPasskeyVerify = path.includes('/api/auth/verify-passkey');

      if (!isPasskeyVerify && token) {
        handleUnauthorized(isMutation, token);
      }
    }
    return res;
  });
}

export function handleUnauthorized(isMutation: boolean = false, rejectedToken?: string) {
  // If a specific token was rejected, only clear if it matches the current stored token
  // (prevents an older in-flight request from wiping a freshly saved token)
  if (rejectedToken && getToken() !== rejectedToken) {
    return;
  }
  clearToken();
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent('at_unauthorized', { detail: { isMutation } }));
  }
}
