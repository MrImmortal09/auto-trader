// ---------------------------------------------------------------------------
// The phone sign-in's token (a user's or the admin's), beside the passkey
// token in `auth.ts`. Both are the same JWT-like format; this one names a
// user (`sub: "u:<id>"`) and a role. v1 never reads it.
// ---------------------------------------------------------------------------

const USER_TOKEN_KEY = 'at_user_token';

export interface TokenClaims {
  sub: string;
  role: string | null;
  /** Unix seconds. */
  exp: number;
}

/** The payload of a token, or null when it can't be read. */
export function decodeClaims(token: string | null): TokenClaims | null {
  if (!token) return null;
  try {
    const parts = token.split('.');
    if (parts.length !== 3) return null;
    let b64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
    const pad = b64.length % 4;
    if (pad) b64 += '='.repeat(4 - pad);
    const payload = JSON.parse(atob(b64)) as { sub?: unknown; role?: unknown; exp?: unknown };
    if (typeof payload.sub !== 'string' || typeof payload.exp !== 'number') return null;
    return { sub: payload.sub, role: typeof payload.role === 'string' ? payload.role : null, exp: payload.exp };
  } catch {
    return null;
  }
}

export function getUserToken(): string | null {
  if (typeof window === 'undefined') return null;
  try {
    return localStorage.getItem(USER_TOKEN_KEY);
  } catch {
    return null;
  }
}

/** The stored phone token while it hasn't expired, else null. */
export function validUserToken(): string | null {
  const token = getUserToken();
  const claims = decodeClaims(token);
  if (!claims) return null;
  return Math.floor(Date.now() / 1000) < claims.exp ? token : null;
}

export function setUserToken(token: string): void {
  if (typeof window === 'undefined') return;
  try {
    localStorage.setItem(USER_TOKEN_KEY, token);
  } catch {
    // Private mode: signed in for this page only.
  }
  notifySessionChange();
}

export function clearUserToken(): void {
  if (typeof window === 'undefined') return;
  try {
    localStorage.removeItem(USER_TOKEN_KEY);
  } catch {
    // Nothing stored.
  }
  notifySessionChange();
}

type Listener = () => void;
const listeners = new Set<Listener>();

/** Called whenever the phone token is set, cleared, or changed in another tab. */
export function onSessionChange(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function notifySessionChange(): void {
  for (const l of listeners) {
    try {
      l();
    } catch (e) {
      console.error(e);
    }
  }
}

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (e) => {
    if (e.key === USER_TOKEN_KEY) notifySessionChange();
  });
}
