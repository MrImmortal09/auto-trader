import { createContext, useContext } from 'react';
import type { Role, SessionUser } from '../lib/types';

/**
 * Who is using the page: the admin (the owner's own number) or a user — a
 * phone sign-in — or nobody. Provided by `IdentityProvider`.
 */
export interface Identity {
  role: Role;
  /** The phone sign-in, once the server has confirmed it. */
  user: SessionUser | null;
  /** Why `user` is still null though a sign-in is held: the check with the
   * server (`/api/auth/me`) failed, as "Could not reach the server". Null
   * while the check is out, once it answered, and without a sign-in. */
  userError: string | null;
  /** The sign-in's role — the server's word, else the token's claim — or
   * null without one: `role` without `anon`. */
  phoneRole: 'admin' | 'user' | null;
  /** The admin: the owner's data (the engine's positions, logs, settings)
   * may be read, and every order and setting changed. */
  canReadOwner: boolean;
  signInOpen: boolean;
  signInReason: string | null;
  openSignIn: (reason?: string) => void;
  closeSignIn: () => void;
  /** Drop the sign-in. */
  signOut: () => void;
}

export const IdentityContext = createContext<Identity | null>(null);

export function useIdentity(): Identity {
  const value = useContext(IdentityContext);
  if (!value) throw new Error('useIdentity must be used within an IdentityProvider');
  return value;
}
