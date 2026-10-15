// ---------------------------------------------------------------------------
// System screen helpers — status levels, thresholds and number formatting.
// ---------------------------------------------------------------------------

/** good / warning / critical carry meaning; neutral = nothing to judge yet. */
export type Level = 'good' | 'warning' | 'critical' | 'neutral';

/** Reserved status colors — only ever used to mean state, always beside an
 * icon + label (never color alone). */
export const STATUS_COLOR: Record<Level, string> = {
  good: '#0ca30c',
  warning: '#fab219',
  critical: '#d03b3b',
  neutral: '#737686',
};

/** Chart series colors (validated as a pair: CVD ΔE 29.6, normal ΔE 41.3). */
export const SERIES = {
  primary: '#004ac6',
  secondary: '#eb6834',
} as const;

/** The engine's tick period — a slower tick body is an overrun. */
export const TICK_BUDGET_MS = 50;

/** `warnAt` / `criticalAt` thresholds, higher is worse. */
export function levelFor(value: number | null | undefined, warnAt: number, criticalAt: number): Level {
  if (value == null || Number.isNaN(value)) return 'neutral';
  if (value >= criticalAt) return 'critical';
  if (value >= warnAt) return 'warning';
  return 'good';
}

export function worst(...levels: Level[]): Level {
  if (levels.includes('critical')) return 'critical';
  if (levels.includes('warning')) return 'warning';
  if (levels.includes('good')) return 'good';
  return 'neutral';
}

/** Weekday 09:15–15:30 IST. Holidays aren't known here — this only decides
 * whether a silent price feed is suspicious or just the market being shut. */
export function isMarketHoursIST(now: Date = new Date()): boolean {
  const ist = new Date(now.toLocaleString('en-US', { timeZone: 'Asia/Kolkata' }));
  const day = ist.getDay();
  if (day === 0 || day === 6) return false;
  const mins = ist.getHours() * 60 + ist.getMinutes();
  return mins >= 9 * 60 + 15 && mins <= 15 * 60 + 30;
}

export function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms >= 1000) return `${(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)} s`;
  if (ms >= 10) return `${Math.round(ms)} ms`;
  return `${ms.toFixed(ms >= 1 ? 1 : 2)} ms`;
}

/** "0.2s", "45s", "3m 10s", "2h 5m" — for ages of heartbeats and ticks. */
export function fmtAge(secs: number | null | undefined): string {
  if (secs == null) return 'never';
  if (secs < 10) return `${secs.toFixed(1)}s`;
  if (secs < 60) return `${Math.round(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${Math.round(secs % 60)}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export function fmtRate(v: number, digits = 1): string {
  if (v === 0) return '0';
  return v >= 100 ? Math.round(v).toLocaleString('en-IN') : v.toFixed(digits);
}

/** 1,284 / 12.9K / 4.2M */
export function fmtCount(n: number): string {
  if (n < 10_000) return n.toLocaleString('en-IN');
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function fmtMb(mb: number): string {
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}

/** HH:MM:SS in IST for chart axes and tooltips. */
export function fmtClockIST(ts: number): string {
  return new Date(ts * 1000).toLocaleTimeString('en-IN', {
    timeZone: 'Asia/Kolkata',
    hour12: false,
  });
}

/** "rejected_kill_switch" → "Rejected · kill switch" */
export function outcomeLabel(key: string): string {
  const words = key.replace(/_/g, ' ');
  if (words.startsWith('rejected ')) return `Rejected · ${words.slice(9)}`;
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function sumValues(r: Record<string, number> | undefined): number {
  return r ? Object.values(r).reduce((a, b) => a + b, 0) : 0;
}
