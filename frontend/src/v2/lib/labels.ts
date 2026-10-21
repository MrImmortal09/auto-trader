import { level, price, signedMoney } from './format';
import type { Call, Fill, FeedStatus, LegRow, Source, Tone } from './types';

/** A sell's reason in a few words: "T1 hit", "Stop hit", "Manual exit". */
export function reasonLabel(reason: string | null | undefined): string {
  if (!reason) return '';
  if (reason === 'TGT2_HIT') return 'T2 hit';
  if (reason.startsWith('TGT1')) return 'T1 hit';
  if (reason.startsWith('EXIT_AT')) return 'Telegram exit';
  switch (reason) {
    case 'SL_HIT':
      return 'Stop hit';
    case 'TRAIL_SL_HIT':
    case 'TRAILED_SL_HIT':
      return 'Trailed stop hit';
    case 'CHART_STOP':
      return 'Chart stop';
    case 'CHART_TARGET':
      return 'Chart target';
    case 'OPPOSITE_SIGNAL_EXIT':
      return 'Opposite call';
    case 'CLOSED_VIA_FRONTEND':
    case 'MANUAL_CLOSE':
    case 'MANUAL_EXIT_ALL':
      return 'Manual exit';
    case 'MANUAL_SELL':
      return 'Manual sell';
    case 'MANUAL_ADD':
      return 'Add';
    case 'KILL_SWITCH':
      return 'Kill switch';
    case 'EXPIRY_SQUAREOFF':
      return 'Expiry square-off';
    case 'ENTRY_CHANGED_ERROR':
      return 'Call edited';
    default: {
      const words = reason.replace(/_/g, ' ').toLowerCase();
      return words.charAt(0).toUpperCase() + words.slice(1);
    }
  }
}

/** A buy leg's reason: "Entry above 84" from the call's message, "Add". */
export function buyLabel(leg: LegRow, message: string | null, scalp: boolean): string {
  if (leg.reason === 'MANUAL_ADD') return 'Add';
  if (scalp) return 'Manual buy';
  const m = message?.match(/\b(ABOVE|BELOW)\s+([\d.]+)/i);
  return m ? `Entry ${m[1].toLowerCase()} ${m[2]}` : 'Entry';
}

/** A call's channel as a chip shows it: the Execute screen's scalps are "Scalp". */
export function sourceOfChannel(channel: string): Source {
  return channel === 'Scalps' ? { kind: 'scalp', title: 'Scalp' } : { kind: 'telegram', title: channel };
}

/** A toast for a fill. */
export function fillToast(f: Fill): { title: string; body: string; tone: Tone; key: string } {
  const at = f.t.slice(11, 19);
  if (f.action === 'BUY') {
    return { title: `Bought ${f.qty} ${f.contract}`, body: `Filled at ${price(f.price)} · ${at}`, tone: 'profit', key: `fill-${f.id}` };
  }
  const why = reasonLabel(f.reason);
  const pnl = f.pnl == null ? '' : ` · ${signedMoney(f.pnl)}`;
  const tone: Tone = f.pnl == null ? 'neutral' : f.pnl < 0 ? 'loss' : 'profit';
  return {
    title: `Sold ${f.qty} ${f.contract}${why ? ` · ${why}` : ''}`,
    body: `Filled at ${price(f.price)}${pnl} · ${at}`,
    tone,
    key: `fill-${f.id}`,
  };
}

/** What a feed message's chip says, and its colour. */
export function feedStatusChip(s: FeedStatus): { label: string; tone: Tone } {
  switch (s.kind) {
    case 'waiting':
      return { label: s.valid_till ? `Waiting · valid till ${s.valid_till}` : 'Waiting', tone: 'neutral' };
    case 'bought':
      return { label: `Bought ${s.qty} @ ${price(s.price)}`, tone: 'profit' };
    case 'closed': {
      const parts = ['Closed'];
      if (s.outcome) parts.push(s.outcome.label);
      if (s.pnl != null) parts.push(signedMoney(s.pnl, 0));
      return { label: parts.join(' · '), tone: s.pnl == null ? 'neutral' : s.pnl < 0 ? 'loss' : 'profit' };
    }
    case 'not_taken':
      return { label: 'Not taken', tone: 'neutral' };
    case 'skipped':
      return { label: 'Skipped · not an index option', tone: 'neutral' };
    case 'not_a_call':
      return { label: s.no_instrument ? 'Not a call · no instrument found' : 'Not a call', tone: 'neutral' };
    case 'stop_update':
      return { label: s.price == null ? 'Stop update' : `Stop update · ${level(s.price)}`, tone: 'neutral' };
    case 'exit':
      return { label: s.price == null ? 'Exit' : `Exit at ${level(s.price)}`, tone: 'neutral' };
  }
}

/** "NIFTY 25000 CE" for a call. */
export function callContract(c: Pick<Call, 'index' | 'strike' | 'side'>): string {
  return [c.index, c.strike != null ? String(c.strike) : null, c.side].filter(Boolean).join(' ');
}

/** A call's gross P&L, before charges: what its sells fetched less what its buys cost. */
export function grossOf(c: Pick<Call, 'legs'>): number {
  return c.legs.reduce((sum, l) => sum + (l.action.toUpperCase() === 'BUY' ? -1 : 1) * l.price * l.qty, 0);
}
