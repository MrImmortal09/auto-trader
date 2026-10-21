import { useEffect, useState } from 'react';
import { useAuth } from '../../context/AuthContext';
import { verifyPasskey } from '../lib/api';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';

// Not in Figma yet: the smallest passkey prompt in v2's style. The token it
// gets is the one v1 uses too, so unlocking either unlocks both.

export function UnlockDialog({ serverBase }: { serverBase: string }) {
  const { isUnlockModalOpen } = useAuth();
  // Mounted fresh each time, so it never opens on a stale code or error.
  return isUnlockModalOpen ? <UnlockForm serverBase={serverBase} /> : null;
}

function UnlockForm({ serverBase }: { serverBase: string }) {
  const { closeUnlockModal, unlockReason, unlock } = useAuth();
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [lockedUntil, setLockedUntil] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (lockedUntil == null) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [lockedUntil]);

  const lockedFor = lockedUntil == null ? 0 : Math.max(0, Math.ceil((lockedUntil - now) / 1000));
  const locked = lockedFor > 0;

  async function submit(value: string) {
    if (value.length !== 6 || busy || locked) return;
    setBusy(true);
    const answer = await verifyPasskey(serverBase, value);
    setBusy(false);
    if ('token' in answer) {
      unlock(answer.token);
      return;
    }
    setError(answer.error);
    setCode('');
    if (answer.lockedForSecs) {
      setNow(Date.now());
      setLockedUntil(Date.now() + answer.lockedForSecs * 1000);
    }
  }

  return (
    <Modal open onClose={closeUnlockModal} label="Unlock write access" width="lg:max-w-[400px]" locked={busy}>
      <form
        className="flex flex-col gap-4 p-6"
        onSubmit={(e) => {
          e.preventDefault();
          void submit(code);
        }}
      >
        <h2 className="type-title text-fg">Unlock write access</h2>
        <p className="type-body text-fg-muted">{unlockReason ?? 'Enter the 6-digit passkey to place orders and change settings.'}</p>
        <input
          type="password"
          inputMode="numeric"
          autoComplete="off"
          aria-label="Passkey"
          maxLength={6}
          value={code}
          disabled={busy || locked}
          onChange={(e) => {
            const next = e.target.value.replace(/\D/g, '').slice(0, 6);
            setCode(next);
            setError(null);
            if (next.length === 6) void submit(next);
          }}
          className="h-12 w-full rounded-8 border border-line bg-inset text-center num-large tracking-[0.6em] text-fg outline-none focus:border-line-strong disabled:opacity-50"
        />
        {locked ? (
          <p className="type-caption text-loss tnum">
            Too many attempts. Try again in {Math.floor(lockedFor / 60)}:{String(lockedFor % 60).padStart(2, '0')}.
          </p>
        ) : (
          error && <p className="type-caption text-loss">{error}</p>
        )}
        <div className="flex justify-end gap-2">
          <Button size="md" onClick={closeUnlockModal} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" kind="profit" type="submit" disabled={busy || locked || code.length !== 6}>
            {busy ? 'Checking…' : 'Unlock'}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
