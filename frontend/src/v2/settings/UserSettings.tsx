import { useState } from 'react';
import { LogIn, LogOut } from 'lucide-react';
import { useIdentity } from '../auth/identity';
import { displayName, formatPhone } from '../auth/phone';
import { getFlags, getMyDesk, getVersion, resetPortfolio } from '../lib/api';
import { money, plural } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Modal } from '../ui/Modal';
import { useToast } from '../ui/toast';
import { ServerDialog } from './dialogs';
import { TradeNotes } from './OtherSections';
import { Row, Section, StateDot } from './parts';

/**
 * Settings for a user or a visitor: their sign-in, the server this page
 * talks to, and how the Trade screen's orders go. The owner's settings
 * (connections, lots, exits, the mode) are the owner's.
 */
export function UserSettings({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const { user, role, openSignIn, signOut } = useIdentity();
  const version = usePoll(() => getVersion(serverBase), 60_000, serverBase);
  const flags = usePoll(() => getFlags(serverBase), 30_000, serverBase);
  const [serverOpen, setServerOpen] = useState(false);
  const large = !desktop;

  const account = user ? (
    <Row
      title={
        <span className="flex items-center gap-2">
          {displayName(user)}
          <Chip kind={user.role === 'admin' ? 'live' : 'neutral'}>{user.role === 'admin' ? 'Admin' : 'Paper trader'}</Chip>
        </span>
      }
      caption={`+91 ${formatPhone(user.phone)} · signed in with a WhatsApp code`}
    >
      <Button size={large ? 'lg' : 'md'} icon={<LogOut size={14} />} onClick={signOut}>
        Sign out
      </Button>
    </Row>
  ) : (
    <Row title="Not signed in" caption="Sign in with your mobile number to trade on paper with your own portfolio. A code comes to your WhatsApp.">
      <Button size={large ? 'lg' : 'md'} kind="profit" icon={<LogIn size={14} />} onClick={() => openSignIn()}>
        Sign in
      </Button>
    </Row>
  );

  return (
    <div className={desktop ? 'px-10 pt-6 pb-24' : 'p-4'}>
      <div className={`flex flex-col gap-8 ${desktop ? 'max-w-[760px]' : ''}`}>
        <Section title="Account">
          {account}
          {role === 'user' && <Portfolio serverBase={serverBase} large={large} />}
        </Section>
        <Section title="Server">
          <Row title="Server" caption={`${serverBase || window.location.origin}${version.data ? ` · backend v${version.data.version}` : ''}`}>
            <StateDot level={version.error ? 'down' : version.data ? 'ok' : 'off'}>{version.error ? "Can't reach" : 'Connected'}</StateDot>
            <Button size={large ? 'lg' : 'sm'} onClick={() => setServerOpen(true)}>
              Change
            </Button>
          </Row>
        </Section>
        <TradeNotes title="Trade" flags={flags.data} />
      </div>
      {serverOpen && <ServerDialog current={serverBase} onClose={() => setServerOpen(false)} />}
    </div>
  );
}

/** A user's paper portfolio: the wallet, and starting over. */
function Portfolio({ serverBase, large }: { serverBase: string; large: boolean }) {
  const desk = usePoll(() => getMyDesk(serverBase), 10_000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  const [asking, setAsking] = useState(false);
  const [resetting, setResetting] = useState(false);
  const open = desk.data?.open.length ?? 0;
  const balance = desk.data?.wallet.balance;

  async function reset() {
    setResetting(true);
    const sent = await guard('Sign in to reset your portfolio.', () => resetPortfolio(serverBase), "Couldn't reset the portfolio", 'user');
    setResetting(false);
    setAsking(false);
    if (sent?.ok) toast({ tone: 'neutral', title: 'Portfolio reset', body: 'Back to ₹10,00,000, with an empty history.' });
    await desk.refresh();
  }

  return (
    <>
      <Row
        title="Your portfolio"
        caption={`Paper wallet ${balance == null ? '—' : money(balance)}${open ? ` · ${plural(open, 'position')} open` : ''}. Starting over puts it back to ₹10,00,000 and clears the history from view.`}
      >
        <Button size={large ? 'lg' : 'md'} onClick={() => setAsking(true)} disabled={!desk.data || open > 0} title={open > 0 ? 'Close every position first' : undefined}>
          Reset
        </Button>
      </Row>
      <Modal open={asking} onClose={() => setAsking(false)} label="Reset your portfolio" width="lg:max-w-[420px]" locked={resetting}>
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">Start over?</h2>
          <p className="type-body text-fg-muted">Your paper wallet goes back to ₹10,00,000 and your trade history is cleared from the Desk. This can't be undone.</p>
          <div className="mt-2 flex justify-end gap-2">
            <Button size="md" kind="quiet" onClick={() => setAsking(false)} disabled={resetting} data-autofocus>
              Cancel
            </Button>
            <Button size="md" kind="alert" onClick={() => void reset()} disabled={resetting}>
              {resetting ? 'Resetting…' : 'Reset portfolio'}
            </Button>
          </div>
        </div>
      </Modal>
    </>
  );
}
