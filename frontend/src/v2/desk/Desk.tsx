import { useState } from 'react';
import { LogIn } from 'lucide-react';
import { useIdentity } from '../auth/identity';
import type { RangeId, Role, Stopped } from '../lib/types';
import { Button } from '../ui/Button';
import { Segment } from '../ui/Segment';
import { DeskPerps } from './DeskPerps';
import { DeskRange } from './DeskRange';
import { DeskToday } from './DeskToday';

const RANGES: { value: RangeId; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '7d', label: '7D' },
  { value: '30d', label: '30D' },
  { value: 'all', label: 'All' },
];

type Venue = 'fno' | 'hl';

/** Desk: Today, or the range switch's 7D, 30D and All — the owner's book
 * for the admin, a user's own paper portfolio for a user. With the
 * `hyperliquid` switch on, an F&O | Hyperliquid switch beside it: F&O is
 * where the Desk always opens (the owner's pick); Hyperliquid shows the
 * perps' today. */
export function Desk({ serverBase, desktop, stopped, role, perpsOn = false }: { serverBase: string; desktop: boolean; stopped: Stopped | null; role: Role; perpsOn?: boolean }) {
  const [range, setRange] = useState<RangeId>('today');
  const [venue, setVenue] = useState<Venue>('fno');
  if (role === 'anon') return <DeskGuest />;
  const paper = role === 'user';
  const size = desktop ? 'sm' : 'lg';
  const venueSwitch = perpsOn ? (
    <Segment
      label="Market"
      size={size}
      stretch={!desktop}
      value={venue}
      onChange={setVenue}
      options={[
        { value: 'fno', label: 'F&O' },
        { value: 'hl', label: 'Hyperliquid' },
      ]}
    />
  ) : null;
  if (perpsOn && venue === 'hl') return <DeskPerps serverBase={serverBase} desktop={desktop} header={venueSwitch} />;
  const rangeSegment = <Segment label="Range" options={RANGES} value={range} onChange={setRange} size={size} stretch={!desktop} />;
  const rangeSwitch = venueSwitch ? (
    <div className={desktop ? 'flex items-center gap-3' : 'flex flex-col gap-2'}>
      {venueSwitch}
      {rangeSegment}
    </div>
  ) : (
    rangeSegment
  );
  return range === 'today' ? (
    <DeskToday serverBase={serverBase} desktop={desktop} rangeSwitch={rangeSwitch} stopped={stopped} paper={paper} />
  ) : (
    <DeskRange serverBase={serverBase} range={range} desktop={desktop} rangeSwitch={rangeSwitch} paper={paper} />
  );
}

/** What a visitor sees in place of a desk. */
function DeskGuest() {
  const { openSignIn } = useIdentity();
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 py-16 text-center">
      <h2 className="type-title text-fg">Your paper desk</h2>
      <p className="max-w-[360px] type-body text-fg-muted">Sign in to see your positions, today's P&L and your trade history. Charts and the ladder are open to everyone.</p>
      <Button size="lg" kind="accent" icon={<LogIn size={16} />} onClick={() => openSignIn('Sign in to see your paper desk.')}>
        Sign in
      </Button>
    </div>
  );
}
