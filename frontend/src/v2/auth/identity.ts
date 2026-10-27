import { createContext, useContext } from 'react';
import type { Role, SessionUser } from '../lib/types';

/**
 * Who is using the page: the owner (the passkey, v1's), the admin or a user
 * (a phone sign-in), or nobody. Provided by `IdentityProvider`.
 */
export interface Identity {
  role: Role;
  /** The phone sign-in, when there is one. */
  user: SessionUser | null;
  /** The passkey is held: owner-only actions go through without a prompt. */
  hasPasskey: boolean;
  /** Owner data (the engine's positions, logs, settings) may be read. */
  canReadOwner: boolean;
  signInOpen: boolean;
  signInReason: string | null;
  openSignIn: (reason?: string) => void;
  closeSignIn: () => void;
  /** Drop the phone sign-in. The passkey, if held, stays. */
  signOut: () => void;
}

export const IdentityContext = createContext<Identity | null>(null);

export function useIdentity(): Identity {
  const value = useContext(IdentityContext);
  if (!value) throw new Error('useIdentity must be used within an IdentityProvider');
  return value;
}
