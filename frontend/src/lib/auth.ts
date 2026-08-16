// ---------------------------------------------------------------------------
// JWT-like Token Management
// ---------------------------------------------------------------------------

const TOKEN_KEY = 'at_session_token';

export function getToken(): string | null {
  if (typeof window === 'undefined') return null;
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  if (typeof window === 'undefined') return;
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken(): void {
  if (typeof window === 'undefined') return;
  localStorage.removeItem(TOKEN_KEY);
}

// Quick client-side check if the token exists and hasn't expired.
// (The server still validates this cryptographically on every request)
export function isTokenValid(): boolean {
  const token = getToken();
  if (!token) return false;

  try {
    const parts = token.split('.');
    if (parts.length !== 3) return false;

    // Decode the base64url payload
    const payloadBase64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
    const payloadJson = atob(payloadBase64);
    const payload = JSON.parse(payloadJson);

    // exp is in seconds
    const now = Math.floor(Date.now() / 1000);
    return now < payload.exp;
  } catch {
    return false;
  }
}
