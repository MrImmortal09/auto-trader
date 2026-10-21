// Formatting for v2: Indian digit grouping, a sign on every P&L, IST times.

const TWO = new Intl.NumberFormat('en-IN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const UP_TO_TWO = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 2 });
const WHOLE = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 0 });

/** The typographic minus the designs use. */
export const MINUS = '−';

/** −0 and float dust below half a paisa read as 0. */
function clean(n: number): number {
  return Math.abs(n) < 0.005 ? 0 : n;
}

/** "+₹1,430.00", "−₹318.00", "₹0.00" — every P&L carries its sign. */
export function signedMoney(n: number, digits: 0 | 2 = 2): string {
  const v = clean(n);
  const body = `₹${(digits === 0 ? WHOLE : TWO).format(Math.abs(v))}`;
  return v > 0 ? `+${body}` : v < 0 ? `${MINUS}${body}` : body;
}

/** "₹2,48,560.00" — an amount that isn't a P&L. */
export function money(n: number): string {
  const v = clean(n);
  return `${v < 0 ? MINUS : ''}₹${TWO.format(Math.abs(v))}`;
}

/** An option premium: "150.00". */
export function price(n: number): string {
  return TWO.format(n);
}

/** An index level or strike: "25,060", "25,112.4". */
export function level(n: number): string {
  return UP_TO_TWO.format(n);
}

/** "+7.33%". */
export function signedPct(n: number): string {
  const v = clean(n);
  return `${v > 0 ? '+' : v < 0 ? MINUS : ''}${Math.abs(v).toFixed(2)}%`;
}

/** "58%". */
export function pct(n: number): string {
  return `${Math.round(n)}%`;
}

/** Chart axis money: "₹0", "₹40k", "₹1.2L", with a minus below zero. */
export function axisMoney(n: number): string {
  const v = clean(n);
  const a = Math.abs(v);
  const body = a >= 1e5 ? `${+(a / 1e5).toFixed(1)}L` : a >= 1e3 ? `${+(a / 1e3).toFixed(1)}k` : `${Math.round(a)}`;
  return `${v < 0 ? MINUS : ''}₹${body}`;
}

/** `HH:MM` of an IST `YYYY-MM-DD HH:MM:SS` timestamp. */
export function hhmm(ts: string | null | undefined): string {
  return ts ? ts.slice(11, 16) : '';
}

/** `HH:MM:SS` of an IST timestamp. */
export function hhmmss(ts: string | null | undefined): string {
  return ts ? ts.slice(11, 19) : '';
}

// Dates are IST calendar days ("YYYY-MM-DD"); read them as UTC so the
// browser's own timezone can never shift the day.
function asDate(day: string): Date {
  return new Date(`${day.slice(0, 10)}T00:00:00Z`);
}

// Spelled out here: browsers' "short" month for September is now "Sept".
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

/** "Wed 21 Oct". */
export function dayLabel(day: string): string {
  const d = asDate(day);
  return `${WEEKDAYS[d.getUTCDay()]} ${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]}`;
}

/** "21 Oct". */
export function shortDate(day: string): string {
  const d = asDate(day);
  return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]}`;
}

/** "22 Sep – 21 Oct", "1–8 Oct", or one day alone. */
export function dateRange(from: string, to: string): string {
  const a = asDate(from);
  const b = asDate(to);
  if (from.slice(0, 10) === to.slice(0, 10)) return shortDate(to);
  if (a.getUTCMonth() === b.getUTCMonth() && a.getUTCFullYear() === b.getUTCFullYear()) return `${a.getUTCDate()}–${shortDate(to)}`;
  return `${shortDate(from)} – ${shortDate(to)}`;
}

/** Today in IST, `YYYY-MM-DD`. */
export function todayIst(): string {
  return new Intl.DateTimeFormat('en-CA', { timeZone: 'Asia/Kolkata' }).format(new Date());
}

/** The current IST time, `HH:MM:SS`. */
export function nowIstTime(): string {
  return new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }).format(new Date());
}

const INDEX_NAMES: Record<string, string> = {
  NIFTY: 'Nifty',
  BANKNIFTY: 'Bank Nifty',
  FINNIFTY: 'Fin Nifty',
  MIDCPNIFTY: 'Midcap Nifty',
  SENSEX: 'Sensex',
  BANKEX: 'Bankex',
};

/** "Nifty", "Bank Nifty" — an index the way a sentence names it. */
export function indexName(index: string): string {
  return INDEX_NAMES[index.toUpperCase()] ?? index;
}

/** "1 call", "3 calls". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** `+`, `−` or nothing — which way a number points, for colour. */
export function toneOf(n: number | null | undefined): 'profit' | 'loss' | 'neutral' {
  if (n == null) return 'neutral';
  const v = clean(n);
  return v > 0 ? 'profit' : v < 0 ? 'loss' : 'neutral';
}
