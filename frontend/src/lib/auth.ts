// ---------------------------------------------------------------------------
// The sign-in, as v1 sees it: the phone sign-in's token (`session.ts`), the
// one the main page (v2) keeps. There is no passkey any more: write access is
// the admin's sign-in — `ADMIN_PHONE` on the server, and nobody else.
// ---------------------------------------------------------------------------
import { clearUserToken, decodeClaims, getUserToken, onSessionChange, setUserToken, validUserToken } from './session';

// The retired passkey's token, wherever a browser still keeps it: it opens nothing now.
try {
  if (typeof window !== 'undefined') localStorage.removeItem('at_session_token');
} catch {
  // Blocked storage: nothing kept either.
}

/** The stored token, even once expired: a request with it gets the 401 that clears it. */
export const getToken = getUserToken;
export const setToken = setUserToken;
export const clearToken = clearUserToken;

/** A stored token that hasn't expired. The server still checks it on every request. */
export function isTokenValid(): boolean {
  return validUserToken() != null;
}

/** Signed in as the admin, by the token's claim; the server has the last word. */
export function hasWriteAccess(): boolean {
  return decodeClaims(validUserToken())?.role === 'admin';
}

/** Called with `hasWriteAccess()` whenever the sign-in changes, here or in another tab. */
export function onAuthChange(listener: (hasWrite: boolean) => void): () => void {
  return onSessionChange(() => listener(hasWriteAccess()));
}
