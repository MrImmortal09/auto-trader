// The System screen's numbers: durations, ages, counts, sizes and shares.

const COUNT = new Intl.NumberFormat('en-IN');

/** "8.4 ms", "142 ms", "1.2 s". */
export function ms(v: number | null | undefined): string {
  if (v == null) return '—';
  if (v >= 1000) return `${(v / 1000).toFixed(1)} s`;
  return v >= 10 ? `${Math.round(v)} ms` : `${v.toFixed(1)} ms`;
}

/** "0.3 s", "12 s", "4 min". */
export function ago(secs: number | null | undefined): string {
  if (secs == null) return '—';
  if (secs < 10) return `${secs.toFixed(1)} s`;
  if (secs < 120) return `${Math.round(secs)} s`;
  return `${Math.round(secs / 60)} min`;
}

/** "1,284", or a dash before there's a number. */
export function count(v: number | null | undefined): string {
  return v == null ? '—' : COUNT.format(v);
}

/** "180 MB", "1.0 GB". */
export function mb(v: number | null | undefined): string {
  if (v == null) return '—';
  return v >= 1024 ? `${(v / 1024).toFixed(1)} GB` : `${Math.round(v)} MB`;
}

/** "3.1%", "42%". */
export function pct(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return '—';
  return `${v < 10 ? v.toFixed(1) : Math.round(v)}%`;
}

/** "212", "0.6": a rate per second or minute. */
export function rate(v: number | null | undefined): string {
  if (v == null) return '—';
  return v >= 100 ? COUNT.format(Math.round(v)) : v.toFixed(1);
}

/** "4 days 2 h", "3 h 12 min", "12 min". */
export function uptime(secs: number | null | undefined): string {
  if (secs == null) return '—';
  const d = Math.floor(secs / 86_400);
  const h = Math.floor((secs % 86_400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d} ${d === 1 ? 'day' : 'days'} ${h} h`;
  if (h > 0) return `${h} h ${m} min`;
  return `${Math.max(1, m)} min`;
}

/** An axis tick: whole numbers plain, the rest to one place. */
export function tick(v: number): string {
  return Number.isInteger(v) ? COUNT.format(v) : v.toFixed(1);
}

const OUTCOMES: Record<string, string> = {
  accepted: 'Accepted',
  edit_applied: 'Edit applied',
  duplicate_revised: 'Duplicate revised',
  duplicate_skipped: 'Duplicate skipped',
  sl_updated: 'Stop moved',
  sl_unmatched: 'Stop move, no position',
  sl_lower_refused: 'Lower stop refused',
  exit_applied: 'Exit applied',
  exit_unmatched: 'Exit, no position',
  rejected_no_target: 'Refused · no target',
  rejected_expired: 'Refused · expired',
  rejected_kill_switch: 'Refused · kill switch',
  rejected_zero_lots: 'Refused · zero lots',
  rejected_no_scrip_master: 'Refused · no Scrip Master',
  rejected_unresolved: 'Refused · contract not found',
  rejected_at_target: 'Refused · already at target',
  rejected_unsupported: 'Refused · not supported',
};

/** What the engine did with a signal, in words. */
export function outcomeWords(key: string): string {
  if (OUTCOMES[key]) return OUTCOMES[key];
  const words = key.replace(/_/g, ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}
