import { price, shortDate } from '../lib/format';
import type { InstrumentRow, SearchAnswer } from '../lib/types';

// What a search answer is made of, for the desktop popover and the phone
// sheet: its groups in reading order, a row's DOM id, a derivative's note.

/** The four groups of an answer, in reading order, each with its head. */
export function groupsOf(answer: SearchAnswer): { head: string; rows: InstrumentRow[] }[] {
  return [
    { head: 'Indices', rows: answer.indices },
    { head: 'Stocks', rows: answer.stocks },
    { head: 'Futures', rows: answer.futures },
    { head: 'Options', rows: answer.options },
  ].filter((g) => g.rows.length > 0);
}

/** The DOM id of a result row, for `aria-activedescendant`. */
export const optionId = (listId: string, rowId: string) => `${listId}-${rowId.replace(/[^A-Za-z0-9_-]/g, '_')}`;

/** "11 Nov · 25,600" — a derivative's expiry and strike; a future's expiry alone. */
export function derivativeNote(row: InstrumentRow): string | null {
  if (!row.expiry) return null;
  const parts = [shortDate(row.expiry)];
  if (row.strike != null) parts.push(price(row.strike).replace(/\.00$/, ''));
  return parts.join(' · ');
}

/** What a row is listed as: its symbol — the underlying's on a contract,
 * whose expiry, strike and kind the chips and `derivativeNote` carry; the
 * trading symbol (`RELIANCE26NOV1380CE`) is its tooltip. */
export function rowSymbol(row: InstrumentRow): string {
  return row.symbol;
}

/** An instrument named in one go, for a title or a toast: "RELIANCE",
 * "NIFTY 25600 CE", "RELIANCE NOV FUT". */
export function rowTitle(row: Pick<InstrumentRow, 'kind' | 'symbol' | 'underlying' | 'expiry' | 'strike'>): string {
  if (row.kind === 'EQ' || row.kind === 'INDEX') return row.symbol;
  const month = row.expiry ? shortDate(row.expiry).split(' ')[1].toUpperCase() : '';
  if (row.kind === 'FUT') return `${row.underlying} ${month} FUT`;
  return `${row.underlying} ${row.strike != null ? price(row.strike).replace(/\.00$/, '') : ''} ${row.kind}`;
}
