// Words for an index's listed expiries.

import { daysLeftLabel, expiryLabel } from '../../lib/execute';
import type { ExpiriesAnswer } from '../lib/types';

export type ExpiryRow = ExpiriesAnswer['expiries'][number];

/** The last listed expiry of its month is the monthly contract. */
export function isMonthly(all: { date: string }[], date: string): boolean {
  return !all.some((e) => e.date > date && e.date.slice(0, 7) === date.slice(0, 7));
}

/** "Tue 27 Oct · 6 days left", with "nearest" or "monthly" where it applies. */
export function expiryWords(e: ExpiryRow, all: ExpiryRow[]): { label: string; tag: string | null } {
  const tag = e.date === all[0]?.date ? 'nearest' : isMonthly(all, e.date) ? 'monthly' : null;
  return { label: `${expiryLabel(e.date)} · ${daysLeftLabel(e.days_to_expiry)}`, tag };
}

/** "27 Oct", or "25 Nov · M" for the monthly. */
export function expiryChip(e: ExpiryRow, all: ExpiryRow[]): string {
  const day = expiryLabel(e.date).replace(/^\S+\s/, '');
  return isMonthly(all, e.date) ? `${day} · M` : day;
}
