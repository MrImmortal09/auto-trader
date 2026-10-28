import { useState } from 'react';
import { useIdentity } from '../auth/identity';
import { getSystem, updateServer } from '../lib/api';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { useToast } from '../ui/toast';

const POLL_MS = 3_000;
/** update.sh downloads the release, restarts the server and health-checks
 * it (rolling back if it fails) well within this. */
const GIVE_UP_MS = 4 * 60_000;
/** The sampler behind `uptime_secs` reads every 2 s. */
const UPTIME_SLACK_MS = 5_000;

const pause = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms));

/**
 * The admin's "Update server" (v1's, in Connections): runs update.sh on the
 * VM — the latest release from GitHub, a restart, a rollback if the new one
 * is unhealthy — then waits for a server process started after the tap.
 * The old server keeps answering while the release downloads, so an answer
 * alone says nothing; its uptime does.
 */
export function UpdateServer({ serverBase, size, onRestarted }: { serverBase: string; size: 'md' | 'lg'; onRestarted: () => void }) {
  const { role } = useIdentity();
  const guard = useGuard();
  const toast = useToast();
  const [asking, setAsking] = useState(false);
  const [phase, setPhase] = useState<'idle' | 'sending' | 'waiting'>('idle');
  if (role !== 'admin') return null;
  const busy = phase !== 'idle';

  function finish() {
    setPhase('idle');
    setAsking(false);
  }

  async function run() {
    setPhase('sending');
    const asked = Date.now();
    const sent = await guard('Updating the server needs the admin.', () => updateServer(serverBase), "Couldn't start the update");
    // No answer at all can mean the server went down before replying: wait
    // for it as if it had said yes. A refusal is a refusal (already toasted).
    if (!sent || (!sent.ok && sent.status !== 0)) {
      finish();
      return;
    }
    setPhase('waiting');
    while (Date.now() - asked < GIVE_UP_MS) {
      await pause(POLL_MS);
      try {
        const sys = await getSystem(serverBase);
        if (sys.uptime_secs != null && Date.now() - sys.uptime_secs * 1000 > asked - UPTIME_SLACK_MS) {
          toast({ tone: 'profit', title: 'Server updated', body: `Running v${sys.version}.` });
          onRestarted();
          finish();
          return;
        }
      } catch {
        // Down while it restarts.
      }
    }
    toast({ tone: 'loss', title: 'Update not confirmed', body: "The server hasn't restarted in 4 minutes. See /tmp/update.log on the VM." });
    finish();
  }

  return (
    <>
      <Button size={size} onClick={() => setAsking(true)} disabled={busy}>
        {busy ? 'Updating…' : 'Update'}
      </Button>
      <Modal open={asking} onClose={() => setAsking(false)} label="Update the server" width="lg:max-w-[440px]" locked={busy}>
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">Update the server?</h2>
          <p className="type-body text-fg-muted">
            It downloads the latest release, restarts on it and checks it's healthy, going back to this one if not. For about a minute nothing
            watches the market or the positions, and Kotak disconnects; the new server logs in to Kotak again as it starts.
          </p>
          {phase === 'waiting' && <p className="type-caption text-fg-muted">Waiting for the new server…</p>}
          <div className="mt-2 flex justify-end gap-2">
            <Button size="md" kind="quiet" onClick={() => setAsking(false)} disabled={busy} data-autofocus>
              Cancel
            </Button>
            <Button size="md" kind="alert" onClick={() => void run()} disabled={busy}>
              {phase === 'idle' ? 'Update server' : phase === 'sending' ? 'Starting…' : 'Updating…'}
            </Button>
          </div>
        </div>
      </Modal>
    </>
  );
}
