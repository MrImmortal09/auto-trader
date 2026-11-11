import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { clearUserToken, decodeClaims, onSessionChange, validUserToken } from '../../lib/session';
import { getMe } from '../lib/api';
import { useServerBase } from '../lib/hooks';
import type { Role, SessionUser } from '../lib/types';
import { IdentityContext, type Identity } from './identity';

/** How often the server is asked again whether the sign-in still stands. */
const RECHECK_MS = 5 * 60_000;
/** How soon a check that failed to reach the server is tried again. */
const RETRY_MS = 15_000;

/** The role a phone token claims, before the server has confirmed it. */
function claimedRole(token: string | null): Role {
  const claims = decodeClaims(token);
  if (!claims || !claims.sub.startsWith('u:')) return 'anon';
  return claims.role === 'admin' ? 'admin' : 'user';
}

/**
 * Reads who is signed in from the tokens at once, then confirms it with the
 * server, which also knows about a user disabled since. See `useIdentity`.
 */
export function IdentityProvider({ children }: { children: ReactNode }) {
  const serverBase = useServerBase();
  const [userToken, setUserTokenState] = useState<string | null>(validUserToken);
  // What the server last said about a token; ignored once the token changes.
  const [confirmed, setConfirmed] = useState<{ token: string; role: Role; user: SessionUser | null } | null>(null);
  // The last check that got no answer, so a screen waiting on `user` can say why.
  const [failed, setFailed] = useState<{ token: string; error: string } | null>(null);
  const [signIn, setSignIn] = useState<{ open: boolean; reason: string | null }>({ open: false, reason: null });

  // The token, as stored: set by the sign-in sheet, cleared by sign-out, by
  // a refusal (`at_unauthorized`, from `apiFetch`), or in another tab.
  useEffect(() => {
    const read = () => {
      const token = validUserToken();
      setUserTokenState(token);
      // A sign-in that completed elsewhere (another tab, say) closes the
      // request here too, so a later sign-out doesn't bring the sheet back.
      if (token) setSignIn({ open: false, reason: null });
    };
    const off = onSessionChange(read);
    const onRefused = (e: Event) => {
      read();
      if ((e as CustomEvent<{ isMutation?: boolean }>).detail?.isMutation) {
        setSignIn({ open: true, reason: 'Your sign-in has expired. Sign in again to continue.' });
      }
    };
    window.addEventListener('at_unauthorized', onRefused);
    return () => {
      off();
      window.removeEventListener('at_unauthorized', onRefused);
    };
  }, []);

  // Ask the server who the token is now, and again every few minutes: a
  // user the admin disabled is signed out on the next answer. A check that
  // never got through (the server unreachable as the page loaded) is tried
  // again sooner, so what waits on the confirmed sign-in isn't held for the
  // whole interval once the server is back.
  useEffect(() => {
    if (!userToken) return;
    let cancelled = false;
    let retry: number | null = null;
    const check = async () => {
      try {
        const me = await getMe(serverBase);
        if (cancelled) return;
        if (me.identity === 'admin' || me.identity === 'user') {
          setConfirmed({ token: userToken, role: me.identity, user: me.user });
          setFailed(null);
        } else {
          clearUserToken();
        }
      } catch (e) {
        // Unreachable server: keep what the token says, and say so.
        if (cancelled) return;
        setFailed({ token: userToken, error: e instanceof Error ? e.message : 'Could not reach the server' });
        // One retry pending at most: the interval's check failing while a
        // retry waits replaces it rather than start a second chain, so an
        // outage asks once every 15 s however long it lasts.
        if (retry != null) window.clearTimeout(retry);
        retry = window.setTimeout(() => {
          retry = null;
          void check();
        }, RETRY_MS);
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), RECHECK_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      if (retry != null) window.clearTimeout(retry);
    };
  }, [serverBase, userToken]);

  const openSignIn = useCallback((reason?: string) => setSignIn({ open: true, reason: reason ?? null }), []);
  const closeSignIn = useCallback(() => setSignIn({ open: false, reason: null }), []);
  const signOut = useCallback(() => clearUserToken(), []);

  const value = useMemo<Identity>(() => {
    const known = confirmed?.token === userToken ? confirmed : null;
    const role: Role = known ? known.role : claimedRole(userToken);
    return {
      role,
      user: known?.user ?? null,
      // Only while nothing confirmed this token: a recheck that fails later
      // changes nothing, the earlier answer stands.
      userError: !known && failed?.token === userToken ? failed.error : null,
      phoneRole: role === 'anon' ? null : role,
      canReadOwner: role === 'admin',
      // A sign-in that has happened closes the sheet, wherever it came from.
      signInOpen: signIn.open && !userToken,
      signInReason: signIn.reason,
      openSignIn,
      closeSignIn,
      signOut,
    };
  }, [confirmed, failed, userToken, signIn, openSignIn, closeSignIn, signOut]);

  return <IdentityContext.Provider value={value}>{children}</IdentityContext.Provider>;
}
