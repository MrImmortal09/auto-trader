// ---------------------------------------------------------------------------
import { getToken, clearToken } from './auth';
// API helpers — server base persistence and fetch wrapper
// ---------------------------------------------------------------------------

export const SERVER_BASE_STORAGE_KEY = 'server_base';
export const SERVER_BASE_COOKIE = 'server_base';
export const DEFAULT_SERVER_BASE = 'https://at.axiosiiitl.dev';

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
  const saved = window.localStorage.getItem(SERVER_BASE_STORAGE_KEY) ?? '';
  const cookie = readCookie(SERVER_BASE_COOKIE);
  return normalizeServerBase(saved || cookie || (import.meta.env.VITE_API_BASE_URL ?? DEFAULT_SERVER_BASE));
}

export function persistServerBase(value: string) {
  const normalized = normalizeServerBase(value);

  if (typeof window !== 'undefined') {
    if (normalized) window.localStorage.setItem(SERVER_BASE_STORAGE_KEY, normalized);
    else window.localStorage.removeItem(SERVER_BASE_STORAGE_KEY);
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
      handleUnauthorized();
    }
    return res;
  });
}

export function handleUnauthorized() {
  clearToken();
  if (typeof window !== 'undefined') {
    window.location.reload();
  }
}
