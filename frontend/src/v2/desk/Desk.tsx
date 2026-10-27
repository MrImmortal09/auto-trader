import { useState } from 'react';
import { LogIn } from 'lucide-react';
import { useIdentity } from '../auth/identity';
import type { RangeId, Role, Stopped } from '../lib/types';
import { Button } from '../ui/Button';
import { Segment } from '../ui/Segment';
import { DeskRange } from './DeskRange';
import { DeskToday } from './DeskToday';

const RANGES: { value: RangeId; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '7d', label: '7D' },
  { value: '30d', label: '30D' },
  { value: 'all', label: 'All' },
];

/** Desk: Today, or the range switch's 7D, 30D and All — the owner's book,
 * for the owner and the admin. A visitor is asked to sign in; a user's own
 * paper desk comes with the next release. */
export function Desk({ serverBase, desktop, stopped, role }: { serverBase: string; desktop: boolean; stopped: Stopped | null; role: Role }) {
  const [range, setRange] = useState<RangeId>('today');
  if (role !== 'owner' && role !== 'admin') return <DeskGuest role={role} />;
  const rangeSwitch = <Segment label="Range" options={RANGES} value={range} onChange={setRange} size={desktop ? 'sm' : 'lg'} stretch={!desktop} />;
  return range === 'today' ? (
    <DeskToday serverBase={serverBase} desktop={desktop} rangeSwitch={rangeSwitch} stopped={stopped} />
  ) : (
    <DeskRange serverBase={serverBase} range={range} desktop={desktop} rangeSwitch={rangeSwitch} />
  );
}

/** What a visitor or a user sees in place of the owner's desk. */
function DeskGuest({ role }: { role: Role }) {
  const { openSignIn } = useIdentity();
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 py-16 text-center">
      <h2 className="type-title text-fg">Your paper desk</h2>
      {role === 'anon' ? (
        <>
          <p className="max-w-[360px] type-body text-fg-muted">Sign in to see your positions, today's P&L and your trade history. Charts and the ladder are open to everyone.</p>
          <Button size="lg" kind="profit" icon={<LogIn size={16} />} onClick={() => openSignIn('Sign in to see your paper desk.')}>
            Sign in
          </Button>
        </>
      ) : (
        <p className="max-w-[360px] type-body text-fg-muted">Paper trading on your own portfolio opens with the next update. Charts, the ladder and the Market page are ready now.</p>
      )}
    </div>
  );
}
