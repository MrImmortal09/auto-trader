import { useState, type ReactNode } from 'react';
import { isValidServerBase, normalizeServerBase, persistServerBase } from '../../lib/api';
import { getModeCheck, kotakDisconnect } from '../lib/api';
import { plural } from '../lib/format';
import { useIsDesktop } from '../lib/hooks';
import { usePoll } from '../lib/usePoll';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { useToast } from '../ui/toast';

function Actions({ children }: { children: ReactNode }) {
  const desktop = useIsDesktop();
  return <div className={desktop ? 'flex justify-end gap-2' : 'flex flex-col-reverse gap-3 [&>button]:w-full'}>{children}</div>;
}

/** "Change server": the backend this app — v1 and v2 alike — talks to. */
export function ServerDialog({ current, onClose }: { current: string; onClose: () => void }) {
  const desktop = useIsDesktop();
  const [value, setValue] = useState(current);
  const valid = isValidServerBase(value);
  const size = desktop ? 'md' : 'lg';
  return (
    <Modal open onClose={onClose} label="Change server" width="lg:max-w-[460px]">
      <div className="flex flex-col gap-4 p-6">
        <h2 className="type-title text-fg">Change server</h2>
        <p className="type-body text-fg-muted">The backend this app talks to, in the old design too. Leave it blank to use this site.</p>
        <div className="flex flex-col gap-1.5">
          <label htmlFor="server-base" className="type-label text-fg-muted">
            Server URL
          </label>
          <input
            id="server-base"
            data-autofocus
            value={value}
            onChange={(e) => setValue(e.target.value)}
            placeholder="https://…"
            autoComplete="off"
            spellCheck={false}
            className={`h-11 rounded-6 border bg-inset px-3 type-body text-fg outline-none focus:border-line-strong ${valid ? 'border-line' : 'border-loss'}`}
          />
          {!valid && <span className="type-caption text-loss">A full http:// or https:// address</span>}
        </div>
        <Actions>
          <Button size={size} onClick={onClose}>
            Cancel
          </Button>
          <Button
            size={size}
            kind="profit"
            disabled={!valid || normalizeServerBase(value) === normalizeServerBase(current)}
            onClick={() => {
              persistServerBase(value);
              onClose();
            }}
          >
            Save
          </Button>
        </Actions>
      </div>
    </Modal>
  );
}

/** "Disconnect Kotak?" — refused while LIVE positions are open: with no
 * session nothing could sell them. */
export function KotakDisconnectDialog({ serverBase, onClose, onDone }: { serverBase: string; onClose: () => void; onDone: () => void }) {
  const desktop = useIsDesktop();
  const check = usePoll(() => getModeCheck(serverBase), 3000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  const [sending, setSending] = useState(false);
  const c = check.data;
  const heldLive = c != null && c.mode === 'LIVE' && c.open > 0;
  const size = desktop ? 'md' : 'lg';

  async function disconnect() {
    setSending(true);
    const sent = await guard('Disconnecting Kotak needs write access.', () => kotakDisconnect(serverBase), "Couldn't disconnect Kotak");
    setSending(false);
    if (sent?.ok) {
      toast({ tone: 'neutral', title: 'Kotak disconnected' });
      onDone();
      onClose();
    }
  }

  return (
    <Modal open onClose={onClose} label="Disconnect Kotak?" locked={sending} width="lg:max-w-[460px]">
      <div className="flex flex-col gap-4 p-6">
        <h2 className="type-title text-fg">Disconnect Kotak?</h2>
        <p className="type-body text-fg-muted">
          Orders can't reach Kotak and the price feed stops until you log in again. The 09:05 auto-login logs in again on the next trading day.
        </p>
        {heldLive && (
          <p className="type-caption text-loss">
            {plural(c.open, 'LIVE position')} {c.open === 1 ? 'is' : 'are'} open. Exit {c.open === 1 ? 'it' : 'them'} first: with Kotak disconnected nothing could sell{' '}
            {c.open === 1 ? 'it' : 'them'}.
          </p>
        )}
        <Actions>
          <Button data-autofocus size={size} onClick={onClose} disabled={sending}>
            Cancel
          </Button>
          <Button size={size} kind="loss" onClick={() => void disconnect()} disabled={sending || !c || heldLive}>
            {sending ? 'Disconnecting…' : 'Disconnect'}
          </Button>
        </Actions>
      </div>
    </Modal>
  );
}
