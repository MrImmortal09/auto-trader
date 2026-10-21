import { useState } from 'react';
import { Check } from 'lucide-react';
import { getModeCheck } from '../lib/api';
import { money } from '../lib/format';
import { useIsDesktop } from '../lib/hooks';
import { usePoll } from '../lib/usePoll';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Modal } from '../ui/Modal';
import { consequences, liveBlockedReason, lotsLine } from './mode';

const WORD = 'LIVE';

/**
 * "Switch to LIVE?": the facts that decide what real money is at stake —
 * Kotak's funds, the lots each call buys, the kill switch — and what the
 * switch does to waiting calls. Typing LIVE unlocks it, unless paper
 * positions are still open or Kotak isn't logged in.
 * Mount it fresh each time it opens.
 */
export function LiveDialog({ serverBase, onConfirm, onClose }: {
  serverBase: string;
  /** Save the mode; resolves `true` once saved. */
  onConfirm: () => Promise<boolean>;
  onClose: () => void;
}) {
  const desktop = useIsDesktop();
  const check = usePoll(() => getModeCheck(serverBase), 3000, serverBase);
  const [typed, setTyped] = useState('');
  const [sending, setSending] = useState(false);
  const c = check.data;
  const matched = typed === WORD;
  const blocked = c ? liveBlockedReason(c) : null;
  const ready = c != null && blocked == null;

  async function confirm() {
    setSending(true);
    const ok = await onConfirm();
    setSending(false);
    if (ok) onClose();
  }

  const rows: [string, string][] = c
    ? [
        ['Kotak funds available', c.funds ? money(c.funds.net) : 'Not read yet'],
        ['Lots per call', lotsLine(c.lots)],
        ['Kill switch', c.kill_switch ? 'On' : 'Off'],
      ]
    : [];

  return (
    <Modal open onClose={onClose} label="Switch to LIVE?" locked={sending}>
      <div className="flex flex-col gap-4 overflow-y-auto p-6">
        <div className="flex items-center gap-2.5">
          <h2 className="type-title text-fg">Switch to LIVE?</h2>
          <Chip kind="live">LIVE</Chip>
        </div>
        <div className="flex flex-col gap-2 type-body text-fg-muted">
          <p>From the next order, trades go to Kotak with real money.</p>
          {c && consequences(c).map((line) => <p key={line}>{line}</p>)}
        </div>
        {c ? (
          <div className="rounded-12 bg-inset px-4 py-1">
            {rows.map(([k, v]) => (
              <div key={k} className="flex items-start gap-4 py-2.5">
                <span className="shrink-0 type-body-strong text-fg">{k}</span>
                <span className="ml-auto text-right num-medium text-fg">{v}</span>
              </div>
            ))}
          </div>
        ) : (
          <p className="type-body text-fg-muted">{check.error ?? 'Checking Kotak and what is open…'}</p>
        )}
        {blocked && <p className="type-caption text-loss">{blocked}</p>}
        <div className="flex flex-col gap-1.5">
          <label htmlFor="live-confirm" className="type-label text-fg-muted">
            Type {WORD} to confirm
          </label>
          <div className="flex h-11 items-center gap-2 rounded-6 border border-line bg-inset px-3 focus-within:border-line-strong">
            <input
              id="live-confirm"
              data-autofocus
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              autoComplete="off"
              autoCapitalize="characters"
              spellCheck={false}
              className="min-w-0 flex-1 bg-transparent type-body text-fg outline-none"
            />
            {matched && <Check size={16} className="shrink-0 text-fg" aria-label="Matches" />}
          </div>
          <span className="type-caption text-fg-muted">Capital letters, exactly as shown</span>
        </div>
        <div className={desktop ? 'flex justify-end gap-2' : 'flex flex-col-reverse gap-3'}>
          <Button size={desktop ? 'md' : 'lg'} onClick={onClose} disabled={sending} className={desktop ? '' : 'w-full'}>
            Cancel
          </Button>
          <Button kind="live" size={desktop ? 'md' : 'lg'} onClick={() => void confirm()} disabled={!matched || !ready || sending} className={desktop ? '' : 'w-full'}>
            {sending ? 'Switching…' : 'Switch to LIVE'}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
