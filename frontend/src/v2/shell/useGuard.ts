import { useIdentity } from '../auth/identity';
import type { Sent } from '../lib/api';
import { useToast } from '../ui/toast';

/**
 * Who an action is for. `admin`: the admin's sign-in (real money, the
 * engine, the broker, settings, the allow-list). `user`: a user's sign-in
 * (their own paper portfolio).
 */
export type GuardScope = 'admin' | 'user';

/**
 * Run an action that changes something. Without the sign-in it needs, the
 * sign-in opens (for nobody) or a toast says why not (for the wrong one), and
 * `null` comes back without anything sent. A refusal from the server shows as
 * an error toast titled `failTitle`.
 */
export function useGuard() {
  const { role, openSignIn } = useIdentity();
  const toast = useToast();
  return async function guarded(reason: string, act: () => Promise<Sent>, failTitle: string, scope: GuardScope = 'admin'): Promise<Sent | null> {
    if (role === 'anon') {
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
    // A 401 already dropped the sign-in, which brings back the sign-in page (see lib/api).
    if (!sent.ok && sent.status !== 401) {
      toast({ tone: 'loss', title: failTitle, body: sent.error ?? 'The server refused it.' });
    }
    return sent;
  };
}
