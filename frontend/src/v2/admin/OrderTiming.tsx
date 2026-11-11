import { getOrderTimings } from '../lib/api';
import { plural, price } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { OrderTimingAnswer, OrderTimingRow } from '../lib/types';
import { Row, Section } from '../settings/parts';
import { Chip, type ChipKind } from '../ui/Chip';

const IST_TIME = new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });

/** "10:15:30.123" in IST. */
function clock(ms: number): string {
  return `${IST_TIME.format(new Date(ms))}.${String(Math.floor(ms) % 1000).padStart(3, '0')}`;
}

/** "182 ms", "1.4 s". */
function span(ms: number): string {
  return Math.abs(ms) < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/** The middle value; null for none. */
function median(values: number[]): number | null {
  if (!values.length) return null;
  const s = [...values].sort((a, b) => a - b);
  return s[Math.ceil(s.length / 2) - 1];
}

function statusChip(o: OrderTimingRow): { kind: ChipKind; label: string } | null {
  if (o.error) return { kind: 'loss', label: 'Refused' };
  const s = o.status ?? '';
  if (s === 'complete') return { kind: 'profit', label: 'Filled' };
  if (s === 'rejected') return { kind: 'loss', label: 'Rejected' };
  if (s === 'cancelled') return { kind: 'neutral', label: 'Cancelled' };
  if (s) return { kind: 'outline', label: s[0].toUpperCase() + s.slice(1) };
  return null;
}

/** "MKT", "L ₹120.00", "SL ₹95.00 / trigger ₹96.00". */
function orderKind(o: OrderTimingRow): string {
  const type = o.order_type || '—';
  const limit = o.price ? ` ${price(o.price)}` : '';
  const trigger = o.trigger_price ? ` / trigger ${price(o.trigger_price)}` : '';
  return `${type}${limit}${trigger}`;
}

/** The order's own lines: the send, Kotak's answer, and what the push stream said. */
function lines(o: OrderTimingRow, pushOn: boolean): string[] {
  const sent = o.sent_at_ms;
  const out: string[] = [];
  const answered = sent != null && o.replied_at_ms != null ? `Kotak answered in ${span(o.replied_at_ms - sent)}` : null;
  out.push([sent != null ? clock(sent) : null, orderKind(o), o.error ? `refused: ${o.error}` : answered].filter(Boolean).join(' · '));

  const after = (t: number | null) => (sent != null && t != null ? `+${span(t - sent)}` : t != null ? clock(t) : null);
  const stages = [
    o.accepted_at_ms != null ? `accepted ${after(o.accepted_at_ms)}` : null,
    o.first_fill_ms != null && o.first_fill_ms !== o.filled_at_ms ? `first fill ${after(o.first_fill_ms)}` : null,
    o.filled_at_ms != null ? `filled ${after(o.filled_at_ms)}${o.avg_price ? ` at ${price(o.avg_price)}` : ''}` : null,
    o.filled_qty != null && o.qty ? `${o.filled_qty}/${o.qty} filled` : null,
    o.reject_reason ? `reason: ${o.reject_reason}` : null,
    o.seen_at_ms != null ? `poll saw it ${after(o.seen_at_ms)}` : null,
    o.filled_at_ms != null && o.seen_at_ms != null
      ? `first: ${o.filled_at_ms <= o.seen_at_ms ? 'push' : 'poll'}, ${span(Math.abs(o.seen_at_ms - o.filled_at_ms))} ahead`
      : null,
  ].filter(Boolean);
  if (stages.length) out.push(stages.join(' · '));
  else if (!o.error && o.order_no) out.push(pushOn ? 'No update from Kotak yet' : 'Fill time needs the push stream');

  const theirs = [
    o.kotak_received_ns != null ? `Kotak ${clock(o.kotak_received_ns / 1e6)}` : null,
    o.exchange_epoch_s != null ? `exchange ${IST_TIME.format(new Date(o.exchange_epoch_s * 1000))}` : null,
    o.exchange_confirm ? `confirmed ${o.exchange_confirm}` : null,
    o.order_no ? `#${o.order_no}` : null,
  ].filter(Boolean);
  if (theirs.length) out.push(theirs.join(' · '));
  return out;
}

function pushRow(push: OrderTimingAnswer['push']) {
  if (!push.enabled) {
    return (
      <Row
        title="Push stream off"
        caption="Send and answer times are recorded for every order. Fill times need Kotak's order push stream: run `server order-feed-probe` on the VM during market hours, then set KOTAK_ORDER_STREAM=on and restart."
      />
    );
  }
  if (push.connected) {
    const last = push.last_event_ms != null ? `, last message ${span(Date.now() - push.last_event_ms)} ago` : '';
    return <Row title="Push stream connected" caption={`${push.style ?? '—'} login · ${plural(push.updates, 'update')} since the server started${last}`} />;
  }
  return <Row title="Push stream not connected" caption={push.error ?? 'Connecting…'} />;
}

/**
 * How long each LIVE order took today: sent → Kotak's answer, and (from
 * Kotak's push stream) accepted and filled, on the server's clock; Kotak's
 * and the exchange's own times under each. Admin only.
 */
export function OrderTiming({ serverBase }: { serverBase: string }) {
  const answer = usePoll(() => getOrderTimings(serverBase), 5_000, serverBase);
  const d = answer.data;
  const orders = d?.orders ?? [];
  const answeredIn = median(orders.flatMap((o) => (o.sent_at_ms != null && o.replied_at_ms != null && !o.error ? [o.replied_at_ms - o.sent_at_ms] : [])));
  const filledIn = median(orders.flatMap((o) => (o.sent_at_ms != null && o.filled_at_ms != null ? [o.filled_at_ms - o.sent_at_ms] : [])));
  const seenIn = median(orders.flatMap((o) => (o.sent_at_ms != null && o.seen_at_ms != null ? [o.seen_at_ms - o.sent_at_ms] : [])));

  return (
    <Section
      title="Order timing"
      lead="How long each LIVE order took today, on the server's clock: from sending it to Kotak's answer, and — from Kotak's push stream — to the exchange accepting and filling it. Kotak's and the exchange's own times are under each order."
    >
      {d == null ? (
        <Row title={answer.error ? `Couldn't load the order timings: ${answer.error}` : 'Loading…'} />
      ) : (
        <>
          {pushRow(d.push)}
          {orders.length === 0 ? (
            <Row title="No LIVE orders today" />
          ) : (
            <>
              <Row
                title={`${plural(orders.length, 'order')} today`}
                caption={
                  [
                    answeredIn != null ? `Kotak answered in ${span(answeredIn)} (median)` : null,
                    filledIn != null ? `filled ${span(filledIn)} after sending (median)` : null,
                    seenIn != null ? `the engine's poll saw it ${span(seenIn)} after sending (median)` : null,
                  ]
                    .filter(Boolean)
                    .join(' · ') || undefined
                }
              />
              {orders.map((o, i) => {
                const chip = statusChip(o);
                return (
                  <Row
                    key={o.order_no ?? `refused-${i}`}
                    title={
                      <span className="flex items-center gap-2">
                        <span className="truncate">{`${o.side === 'S' ? 'SELL' : 'BUY'} ${o.qty ?? '—'} ${o.trading_symbol ?? ''}`}</span>
                        {chip && <Chip kind={chip.kind}>{chip.label}</Chip>}
                      </span>
                    }
                    caption={
                      <div className="flex flex-col gap-0.5 tnum">
                        {lines(o, d.push.enabled).map((line) => (
                          <div key={line}>{line}</div>
                        ))}
                      </div>
                    }
                  />
                );
              })}
            </>
          )}
        </>
      )}
    </Section>
  );
}
