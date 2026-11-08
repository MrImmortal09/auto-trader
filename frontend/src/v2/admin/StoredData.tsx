import { getStoredData } from '../lib/api';
import { dateRange, indexName, plural, shortDate } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { ArchivedSymbol, StoredTable } from '../lib/types';
import { Row, Section } from '../settings/parts';

/** "23,250" — Indian grouping. */
const count = (n: number) => n.toLocaleString('en-IN');

/** "20.9 MB". */
function size(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1e3))} KB`;
}

/** "10 Aug – 6 Nov", with the years once the span crosses one. */
function daySpan(from: string, to: string): string {
  const [a, b] = [from.slice(0, 10), to.slice(0, 10)];
  if (a.slice(0, 4) === b.slice(0, 4)) return dateRange(a, b);
  return `${shortDate(a)} ${a.slice(0, 4)} – ${shortDate(b)} ${b.slice(0, 4)}`;
}

function TableRow({ t }: { t: StoredTable }) {
  const held = t.rows === 0 || !t.first || !t.last ? 'Empty' : `${count(t.rows)} ${t.rows === 1 ? 'row' : 'rows'} · ${daySpan(t.first, t.last)}`;
  return (
    <Row
      title={t.label}
      caption={
        <>
          <div>{t.what}</div>
          <div className="tnum">{held}</div>
        </>
      }
    >
      <span className="shrink-0 type-caption text-fg-muted">{t.kept}</span>
    </Row>
  );
}

/** "Index · 10 Aug – 6 Nov · 62 days · 23,250 candles", or a future's line. */
function archiveLine(s: ArchivedSymbol): string {
  const what = s.expiry ? `Future, expires ${shortDate(s.expiry)}` : 'Index';
  const traded = s.expiry ? `, ${count(s.with_volume)} traded` : '';
  return `${what} · ${daySpan(s.first_day, s.last_day)} · ${plural(s.days, 'day')} · ${count(s.candles)} candles${traded}`;
}

/**
 * What the server's database keeps and for how long, then the candle
 * archive's coverage, index by index. Read-only.
 */
export function StoredData({ serverBase }: { serverBase: string }) {
  // Counting the archive reads all of it: once a minute is plenty.
  const data = usePoll(() => getStoredData(serverBase), 60_000, serverBase);
  const d = data.data;
  const pending = <Row title={data.error ? `Couldn't read the database: ${data.error}` : 'Loading…'} />;

  const groups = new Map<string, ArchivedSymbol[]>();
  for (const s of d?.archive ?? []) groups.set(s.index, [...(groups.get(s.index) ?? []), s]);

  return (
    <>
      <Section
        title="Stored data"
        lead={`What the server's database keeps${d ? ` (${size(d.db_bytes)})` : ''}, and for how long. Clearing the database from v1's log panel empties the owner's trades and the logs.`}
      >
        {d == null ? pending : d.tables.map((t) => <TableRow key={t.table} t={t} />)}
      </Section>
      <Section
        title="Candle archive"
        lead="1-minute candles of every index and its listed futures, saved each day after the close (15:50 IST) and kept forever. A future's volume is what an index's VWAP is built from, and Upstox stops serving a future once it expires."
      >
        {d == null ? (
          pending
        ) : !d.archiving ? (
          <Row title="Not archiving" caption="UPSTOX_ANALYTICS_TOKEN isn't set on the server, so no candles are saved." />
        ) : groups.size === 0 ? (
          <Row title="Nothing archived yet" caption="The first sweep runs at 15:50 IST, or when the server starts outside 09:00–15:50 on a weekday." />
        ) : (
          [...groups].map(([index, list]) => (
            <Row
              key={index}
              title={indexName(index)}
              caption={
                <div className="flex flex-col gap-0.5 tnum">
                  {list.map((s) => (
                    <div key={s.symbol}>{archiveLine(s)}</div>
                  ))}
                </div>
              }
            />
          ))
        )}
      </Section>
    </>
  );
}
