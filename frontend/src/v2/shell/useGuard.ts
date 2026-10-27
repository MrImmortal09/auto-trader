import { useAuth } from '../../context/AuthContext';
import { useIdentity } from '../auth/identity';
import type { Sent } from '../lib/api';
import { useToast } from '../ui/toast';

/**
 * Who an action is for. `owner`: the passkey (real money, the engine, the
 * broker, settings). `admin`: the admin's phone sign-in or the passkey (the
 * allow-list). `user`: a user's phone sign-in (their own paper portfolio).
 */
export type GuardScope = 'owner' | 'admin' | 'user';

/**
 * Run an action that changes something. Without the sign-in it needs, the
 * right prompt opens instead — the passkey dialog for `owner`, the phone
 * sign-in for a visitor — and `null` comes back without anything sent. A
 * refusal from the server shows as an error toast titled `failTitle`.
 */
export function useGuard() {
  const { hasWriteAccess, openUnlockModal } = useAuth();
  const { role, openSignIn } = useIdentity();
  const toast = useToast();
  return async function guarded(reason: string, act: () => Promise<Sent>, failTitle: string, scope: GuardScope = 'owner'): Promise<Sent | null> {
    if (scope === 'owner') {
      if (!hasWriteAccess) {
        openUnlockModal(reason);
        return null;
      }
    } else if (role === 'anon') {
      openSignIn(reason);
      return null;
    } else if (scope === 'admin' && role === 'user') {
      toast({ tone: 'loss', title: failTitle, body: 'Only the admin can do this.' });
      return null;
    } else if (scope === 'user' && role !== 'user') {
      toast({ tone: 'loss', title: failTitle, body: "The admin trades on the owner's desk, not a paper portfolio." });
      return null;
    }
    const sent = await act();
    // A 401 already reopened the right prompt (see lib/api and v2/lib/api).
    if (!sent.ok && sent.status !== 401) {
      toast({ tone: 'loss', title: failTitle, body: sent.error ?? 'The server refused it.' });
    }
    return sent;
  };
}
