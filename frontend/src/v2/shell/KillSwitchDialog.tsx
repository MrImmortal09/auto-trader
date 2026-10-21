import { useState } from 'react';
import { CircleAlert } from 'lucide-react';
import { engageKillSwitch, getDesk } from '../lib/api';
import { plural, signedMoney, toneOf } from '../lib/format';
import { useIsDesktop } from '../lib/hooks';
import { usePoll } from '../lib/usePoll';
import type { Mode } from '../lib/types';
import { Button } from '../ui/Button';
import { HoldButton } from '../ui/HoldButton';
import { Modal } from '../ui/Modal';
import { useToast } from '../ui/toast';
import { useGuard } from './useGuard';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** "New calls are ignored for the rest of the day, the waiting call is
 * cancelled, and every open position is sold at market." */
function consequences(open: number, waiting: number): string {
  const parts = ['New calls are ignored for the rest of the day'];
  if (waiting === 1) parts.push('the waiting call is cancelled');
  if (waiting > 1) parts.push(`the ${waiting} waiting calls are cancelled`);
  if (open > 0) parts.push('every open position is sold at market');
  if (parts.length === 1) return `${parts[0]}.`;
  if (parts.length === 2) return `${parts[0]}, and ${parts[1]}.`;
  return `${parts.slice(0, -1).join(', ')}, and ${parts[parts.length - 1]}.`;
}

/**
 * "Stop all trading?" — lists exactly what will be sold, and fires only
 * after the button is held. Calls v1's `POST /api/kill-switch`, unchanged.
 * Mount it fresh each time it opens.
 */
export function KillSwitchDialog({ serverBase, mode, onClose, onDone }: {
  serverBase: string;
  mode: Mode | undefined;
  onClose: () => void;
  /** After the switch is on: refresh what shows it. */
  onDone: () => void;
}) {
  const desk = usePoll(() => getDesk(serverBase), 2000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  const desktop = useIsDesktop();
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const open = desk.data?.open ?? [];
  const waiting = desk.data?.waiting.length ?? 0;
  const openPnl = open.reduce((sum, p) => sum + (p.pnl ?? 0), 0);

  async function fire() {
    setSending(true);
    setError(null);
    const sent = await guard('The kill switch needs write access.', () => engageKillSwitch(serverBase), "Couldn't stop trading");
    setSending(false);
    if (!sent) return;
    if (!sent.ok) {
      setError(sent.error ?? "Couldn't stop trading.");
      return;
    }
    const body = sent.body as { liquidated_positions?: number; abandoned_entries?: number } | null;
    toast({
      tone: 'loss',
      title: 'Trading stopped',
      body: `${plural(body?.liquidated_positions ?? 0, 'position')} exited · ${plural(body?.abandoned_entries ?? 0, 'waiting call')} cancelled`,
    });
    onDone();
    onClose();
  }

  const hold = (
    <HoldButton size={desktop ? 'md' : 'lg'} disabled={sending || !desk.data} onFire={fire} className={desktop ? '' : 'w-full'}>
      {sending ? 'Stopping…' : 'Hold to stop and exit all'}
    </HoldButton>
  );
  // Focus lands on Cancel, never on the button that stops everything.
  const cancel = (
    <Button data-autofocus size={desktop ? 'md' : 'lg'} onClick={onClose} disabled={sending} className={desktop ? '' : 'w-full'}>
      Cancel
    </Button>
  );

  return (
    <Modal open onClose={onClose} label="Stop all trading?" locked={sending}>
      <div className="flex flex-col gap-4 overflow-y-auto p-6">
        <div className="flex items-center gap-3">
          <CircleAlert size={22} className="shrink-0 text-loss" />
          <h2 className="type-title text-fg">Stop all trading?</h2>
        </div>
        <p className="type-body text-fg-muted">{desk.data ? consequences(open.length, waiting) : 'Checking what is open…'}</p>

        {desk.data && (
          <div className="rounded-12 bg-inset px-4 py-1">
            {open.length === 0 ? (
              <div className="py-3 type-body text-fg-muted">Nothing is open.</div>
            ) : (
              <>
                {open.map((p) => (
                  <div key={p.id} className="flex items-center gap-2 py-2.5">
                    <span className="type-body-strong text-fg">{p.contract}</span>
                    <span className="type-caption text-fg-muted tnum">{p.qty} qty</span>
                    <span className={`ml-auto num-medium ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</span>
                  </div>
                ))}
                <div className="flex items-center border-t border-line py-2.5">
                  <span className="type-body text-fg-muted">Open P&amp;L</span>
                  <span className={`ml-auto num-medium ${TONE_TEXT[toneOf(openPnl)]}`}>{signedMoney(openPnl)}</span>
                </div>
              </>
            )}
          </div>
        )}

        <p className={`type-caption ${mode === 'LIVE' ? 'text-live' : 'text-fg-muted'}`}>
          {mode === 'LIVE'
            ? 'LIVE: every open position is sold on Kotak at market, with real money.'
            : 'Paper mode: only paper positions are affected.'}
        </p>
        {error && <p className="type-caption text-loss">{error}</p>}

        {desktop ? (
          <div className="flex justify-end gap-2">
            {cancel}
            {hold}
          </div>
        ) : (
          <div className="flex flex-col gap-3">
            {hold}
            {cancel}
          </div>
        )}
      </div>
    </Modal>
  );
}
