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

/** The strike a contract query names: the number before the side at its end
 * (`NIFTY 25600 CE`, `nifty 25,600ce`, `M&M NOV 2800 PUT`). */
function typedStrike(query: string): number | null {
  const m = query.trim().toUpperCase().match(/(\d[\d,]*(?:\.\d+)?)\s*(?:CE|PE|CALLS?|PUTS?)?$/);
  const strike = m ? Number(m[1].replace(/,/g, '')) : NaN;
  return Number.isFinite(strike) && strike > 0 ? strike : null;
}

/**
 * The row Enter opens when none is highlighted, for an answer to what was
 * typed: what the query names. A contract query's contract at the typed
 * strike (its nearest expiry: the options come by expiry), a future
 * query's first future; a plain query's first row. A contract or future
 * query that matched none has no default: its other rows are the
 * underlying (an index opens the Trade screen) and, for a strike that
 * isn't listed, the strikes around it — never what was typed, so Enter
 * opens nothing and ↓ picks a row.
 */
export function defaultRow(answer: SearchAnswer): InstrumentRow | undefined {
  if (answer.parsed === 'contract') {
    const strike = typedStrike(answer.query);
    return strike == null ? undefined : answer.options.find((r) => r.strike != null && Math.abs(r.strike - strike) < 1e-6);
  }
  if (answer.parsed === 'future') return answer.futures[0];
  return groupsOf(answer)[0]?.rows[0];
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
