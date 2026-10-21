import { useAuth } from '../../context/AuthContext';
import type { Sent } from '../lib/api';
import { useToast } from '../ui/toast';

/**
 * Run an action that changes something: without write access it opens the
 * unlock dialog instead (and returns `null`); a refusal from the server
 * shows as an error toast titled `failTitle`.
 */
export function useGuard() {
  const { hasWriteAccess, openUnlockModal } = useAuth();
  const toast = useToast();
  return async function guarded(reason: string, act: () => Promise<Sent>, failTitle: string): Promise<Sent | null> {
    if (!hasWriteAccess) {
      openUnlockModal(reason);
      return null;
    }
    const sent = await act();
    // A 401 already reopened the unlock dialog (see lib/api's apiFetch).
    if (!sent.ok && sent.status !== 401) {
      toast({ tone: 'loss', title: failTitle, body: sent.error ?? 'The server refused it.' });
    }
    return sent;
  };
}
