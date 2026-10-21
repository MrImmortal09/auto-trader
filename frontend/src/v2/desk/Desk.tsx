import { useState } from 'react';
import type { RangeId, Stopped } from '../lib/types';
import { Segment } from '../ui/Segment';
import { DeskRange } from './DeskRange';
import { DeskToday } from './DeskToday';

const RANGES: { value: RangeId; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '7d', label: '7D' },
  { value: '30d', label: '30D' },
  { value: 'all', label: 'All' },
];

/** Desk: Today, or the range switch's 7D, 30D and All. */
export function Desk({ serverBase, desktop, stopped }: { serverBase: string; desktop: boolean; stopped: Stopped | null }) {
  const [range, setRange] = useState<RangeId>('today');
  const rangeSwitch = <Segment label="Range" options={RANGES} value={range} onChange={setRange} size={desktop ? 'sm' : 'lg'} stretch={!desktop} />;
  return range === 'today' ? (
    <DeskToday serverBase={serverBase} desktop={desktop} rangeSwitch={rangeSwitch} stopped={stopped} />
  ) : (
    <DeskRange serverBase={serverBase} range={range} desktop={desktop} rangeSwitch={rangeSwitch} />
  );
}
