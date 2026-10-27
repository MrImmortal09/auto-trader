import { useState } from 'react';
import { LogIn, LogOut } from 'lucide-react';
import { useIdentity } from '../auth/identity';
import { displayName, formatPhone } from '../auth/phone';
import { getFlags, getVersion } from '../lib/api';
import { usePoll } from '../lib/usePoll';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
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
          {role === 'user' && <Row title="Your portfolio" caption="Paper trading on your own portfolio — with its wallet, positions and history — opens with the next update." />}
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
