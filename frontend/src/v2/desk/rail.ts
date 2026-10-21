import { level, price } from '../lib/format';
import type { ChartLevel, DeskPosition } from '../lib/types';

// What a Figma "Position rail" shows for a Desk position — its marks, the
// price marker and the fill. Drawn by PositionRail.tsx.

export type MarkTone = 'loss' | 'profit' | 'muted';

export interface RailMark {
  at: number;
  label: string;
  tone: MarkTone;
  /** Lower shows first when labels would overlap. */
  priority: number;
  tick?: boolean;
}

export interface RailSpec {
  marks: RailMark[];
  marker: { at: number; label: string; hollow?: boolean } | null;
  fill: { from: number; to: number; tone: 'profit' | 'loss' } | null;
  zone: { from: number; to: number } | null;
  dashed: boolean;
  /** No rail to draw: this line instead. */
  empty?: string;
}

function chartText(kind: 'Stop' | 'Target', l: ChartLevel, index: string, side: string | null): string {
  if (l.trigger === 'premium') return `${kind} ₹${level(l.price)}`;
  // A call's stop and a put's target fire as the index falls.
  const falls = (kind === 'Stop') === (side === 'CE');
  return `${kind} ${index} ${falls ? '≤' : '≥'} ${level(l.price)}`;
}

/** The rail for a Desk position. */
export function railFor(p: DeskPosition): RailSpec {
  const none = (empty: string): RailSpec => ({ marks: [], marker: null, fill: null, zone: null, dashed: true, empty });

  if (p.source.kind === 'scalp') {
    if (p.state === 'waiting') return none('Buying at market');
    const { stop, target } = p.chart;
    if (!stop && !target) return none('No stop or target yet · add them from the chart');
    if (stop && target && stop.trigger !== target.trigger) {
      return none(`${chartText('Stop', stop, p.index, p.side)} · ${chartText('Target', target, p.index, p.side)}`);
    }
    const marks: RailMark[] = [];
    if (stop) marks.push({ at: stop.price, label: `Stop ${level(stop.price)}`, tone: 'loss', priority: 0 });
    if (target) marks.push({ at: target.price, label: `Target ${level(target.price)}`, tone: 'profit', priority: 0 });
    if ((stop ?? target)!.trigger === 'index') {
      // An index level's rail: the stop and target, and where the index is.
      const marker = p.index_ltp != null ? { at: p.index_ltp, label: price(p.index_ltp) } : null;
      return { marks, marker, fill: null, zone: null, dashed: false };
    }
    marks.push({ at: p.avg, label: level(p.avg), tone: 'muted', priority: 2 });
    return {
      marks,
      marker: p.ltp != null ? { at: p.ltp, label: price(p.ltp) } : null,
      fill: p.ltp != null ? { from: p.avg, to: p.ltp, tone: p.ltp >= p.avg ? 'profit' : 'loss' } : null,
      zone: null,
      dashed: false,
    };
  }

  const marks: RailMark[] = [];
  // A stop of 0 is a call with no stop ("SL :- 0"): nothing to draw.
  if (p.stop > 0) {
    const trailed = p.state !== 'waiting' && p.stop > p.signal_stop + 1e-9;
    marks.push({ at: p.stop, label: `SL ${level(p.stop)}${trailed ? ' · trailed' : ''}`, tone: 'loss', priority: 0 });
  }
  p.targets.forEach((t, i) => marks.push({ at: t, label: `T${i + 1} ${level(t)}`, tone: 'profit', priority: 1 }));
  const next = p.next_target;
  if (next != null && !p.targets.some((t) => Math.abs(t - next) < 1e-9)) {
    marks.push({ at: next, label: `Next ${level(next)}`, tone: 'profit', priority: 2 });
  }

  if (p.state === 'waiting') {
    const range = p.zone ? { from: p.zone[0], to: p.zone[1] } : p.entry_max != null ? { from: p.entry, to: p.entry_max } : null;
    const zone = range && range.to > range.from ? range : null;
    if (zone) {
      marks.push({ at: zone.from, label: '', tone: 'muted', priority: 9 });
      marks.push({ at: (zone.from + zone.to) / 2, label: `Buy ${level(zone.from)}–${level(zone.to)}`, tone: 'muted', priority: 1, tick: false });
    } else {
      marks.push({ at: p.entry, label: `Buy ${level(p.entry)}`, tone: 'muted', priority: 1 });
    }
    return {
      marks,
      marker: p.ltp != null ? { at: p.ltp, label: price(p.ltp), hollow: true } : null,
      fill: null,
      zone,
      dashed: true,
    };
  }

  if (Math.abs(p.avg - p.stop) > 1e-9) marks.push({ at: p.avg, label: level(p.avg), tone: 'muted', priority: 3 });
  return {
    marks,
    marker: p.ltp != null ? { at: p.ltp, label: price(p.ltp) } : null,
    fill: p.ltp != null ? { from: p.avg, to: p.ltp, tone: p.ltp >= p.avg ? 'profit' : 'loss' } : null,
    zone: null,
    dashed: false,
  };
}
