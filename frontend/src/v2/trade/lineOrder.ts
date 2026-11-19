// A line order being placed on its index's chart: the buy line, then the
// stop, then targets until every lot has one. Nothing goes to the server
// until the last target is placed, as one order (the owner's rule,
// 2026-11-18): it is never armed without its stop.

import type { EntryLine, ExecutePosition } from '../../types';
import type { LineBuy, LinePick } from '../lib/api';
import { level } from '../lib/format';

export interface LineDraft {
  /** The chart it is placed on: its index's own. */
  slot: number;
  /** The contract's trading symbol, and how it reads ("NIFTY 26000 CE (25 Nov)"). */
  sym: string;
  label: string;
  index: string;
  strike: number;
  side: 'CE' | 'PE';
  /** `YYYY-MM-DD`. */
  expiry: string;
  lots: number;
  lotSize: number;
  entry: LinePick | null;
  stop: number | null;
  targets: { price: number; lots: number }[];
  /** Lots the next target sells (its picker); the top is all those left. */
  pick: number;
  /** Sent, waiting for the server's answer. */
  sending: boolean;
}

export type DraftStep = 'entry' | 'stop' | 'target';

/** What the next tap places. */
export function draftStep(d: LineDraft): DraftStep {
  if (!d.entry) return 'entry';
  return d.stop == null ? 'stop' : 'target';
}

/** Lots no target sells yet. */
export function lotsLeft(d: Pick<LineDraft, 'lots' | 'targets'>): number {
  return d.lots - d.targets.reduce((n, t) => n + t.lots, 0);
}

/** How long a buy line stays armed: minutes, or `null` till close. */
export const VALIDITY_CHOICES: { minutes: number | null; label: string }[] = [
  { minutes: 5, label: '5 min' },
  { minutes: 15, label: '15 min' },
  { minutes: 30, label: '30 min' },
  { minutes: 60, label: '1 h' },
  { minutes: null, label: 'Till close' },
];
export const DEFAULT_VALIDITY = 30;

/** A saved validity this build offers, else the default. */
export function validityOf(raw: unknown): number | null {
  if (raw === null) return null;
  return VALIDITY_CHOICES.some((c) => c.minutes === raw) ? (raw as number) : DEFAULT_VALIDITY;
}

/** "till 11:45" from `now` (ms) for `minutes`, or "till close". */
export function validityWords(minutes: number | null, now: number): string {
  if (minutes == null) return 'till close';
  const t = new Date(now + minutes * 60_000 + 5.5 * 3_600_000);
  const hh = String(t.getUTCHours()).padStart(2, '0');
  const mm = String(t.getUTCMinutes()).padStart(2, '0');
  return `till ${hh}:${mm}`;
}

/** "till 11:45" for an armed line, or "till close". */
export function lineUntil(line: EntryLine): string {
  return line.valid_until ? `till ${line.valid_until.slice(11, 16)}` : 'till close';
}

/** Where a stop or target goes against the buy line: a call's stop below
 * and its targets above (it gains as the index rises), a put's the other
 * way round. */
export function sideOf(kind: 'stop' | 'target', side: string | null): 'below' | 'above' {
  return (kind === 'stop') === (side === 'CE') ? 'below' : 'above';
}

/** Why `at` can't be a `kind` of a `side` whose buy line is at `line`, or null. */
export function wrongSide(kind: 'stop' | 'target', side: string | null, line: number, at: number): string | null {
  const want = sideOf(kind, side);
  if (want === 'below' ? at < line : at > line) return null;
  return `A ${side === 'PE' ? 'put' : 'call'}'s ${kind} goes ${want} its buy line (${level(line)})`;
}

/** Why the buy line can't go at `at` with the stop and targets already placed, or null. */
export function entryMisfit(side: string | null, stop: number | null, targets: { price: number }[], at: number): string | null {
  if (stop != null && wrongSide('stop', side, at, stop)) return `The stop at ${level(stop)} has to stay ${sideOf('stop', side)} the buy line`;
  const off = targets.find((t) => wrongSide('target', side, at, t.price));
  return off ? `The target at ${level(off.price)} has to stay ${sideOf('target', side)} the buy line` : null;
}

/** "≤ 25,980.00" or "≥ 26,020.00": which way the index reaches the line. */
export function crossWords(line: LinePick): string {
  return `${line.rising ? '≥' : '≤'} ${level(line.price)}`;
}

/** The order to send, once the buy line and stop are placed and every lot has a target. */
export function orderOf(d: LineDraft, validMinutes: number | null): LineBuy | null {
  if (!d.entry || d.stop == null || d.targets.length === 0 || lotsLeft(d) !== 0) return null;
  return { trading_symbol: d.sym, lots: d.lots, line: d.entry, valid_minutes: validMinutes, stop: d.stop, targets: d.targets };
}

/** What a level would make against the buy line, roughly: the option moves
 * by its delta (negative for a put) for each point the index moves. */
export function linePnl(at: number, line: number, delta: number, qty: number): number {
  return delta * (at - line) * qty;
}

/** A line order still waiting on its buy line (not crossed yet). */
export function armedLine(p: ExecutePosition): EntryLine | null {
  return p.placing && p.line && !p.line.triggered_at ? p.line : null;
}
