import type { TelemetrySample } from '../../types';
import { sumValues } from '../../lib/pulse';
import { price, signedMoney } from '../lib/format';
import type { Level } from '../lib/types';
import { HealthDot } from '../ui/Health';
import { Card, CountRow } from './parts';
import { ago, count, ms, outcomeWords, rate } from './units';

/** What the engine did with a signal, when it counts as applied. */
const APPLIED = ['accepted', 'edit_applied', 'duplicate_revised', 'sl_updated', 'exit_applied'];

/** Outcomes with how many, longest first, each with a bar. */
function OutcomeList({ rows, empty }: { rows: [string, number][]; empty: string }) {
  const shown = rows.filter(([, v]) => v > 0).sort((a, b) => b[1] - a[1]);
  if (shown.length === 0) return <p className="py-1.5 type-caption text-fg-muted">{empty}</p>;
  const top = Math.max(...shown.map(([, v]) => v));
  return (
    <ul className="flex flex-col gap-1.5">
      {shown.map(([k, v]) => (
        <li key={k}>
          <div className="flex items-baseline justify-between gap-3">
            <span className="type-body text-fg-muted">{outcomeWords(k)}</span>
            <span className="num-medium text-fg">{count(v)}</span>
          </div>
          <span aria-hidden className="mt-1 block h-1 rounded-full bg-chart-indicator" style={{ width: `${Math.max(2, (v / top) * 100)}%` }} />
        </li>
      ))}
    </ul>
  );
}

/** Signal flow since the server started: received, parsed, and what the
 * engine did with each. */
export function SignalFlow({ s, compact = false }: { s: TelemetrySample; compact?: boolean }) {
  const outcomes = Object.entries(s.signal_outcomes);
  return (
    <Card title="Signal flow">
      <div className={`grid gap-x-6 ${compact ? 'grid-cols-1' : 'grid-cols-2'}`}>
        <CountRow label="Received" hint={`${rate(s.signals_per_min)} a minute now`} value={count(s.signals_received_total)} />
        <CountRow label="Telegram messages" value={count(s.telegram_messages_total)} />
        <CountRow label="Not read as a call" hint="From the watched chats" value={count(s.telegram_parse.unparsed ?? 0)} />
        <CountRow label="Dropped, engine behind" value={count(s.signals_lagged_total)} alarm={s.signals_lagged_total > 0} />
      </div>
      <div className={`mt-3 grid gap-6 border-t border-line pt-3 ${compact ? 'grid-cols-1' : 'grid-cols-2'}`}>
        <div>
          <div className="mb-2 type-label text-fg">Applied</div>
          <OutcomeList rows={outcomes.filter(([k]) => APPLIED.includes(k))} empty="Nothing applied yet." />
        </div>
        <div>
          <div className="mb-2 type-label text-fg">Refused or unmatched</div>
          <OutcomeList rows={outcomes.filter(([k]) => !APPLIED.includes(k))} empty="Nothing refused." />
        </div>
      </div>
    </Card>
  );
}

/** Orders and Kotak since the server started. */
export function OrderFlow({ s, compact = false }: { s: TelemetrySample; compact?: boolean }) {
  const r = s.orders_by_result;
  return (
    <Card title="Orders and Kotak">
      <div className={`grid gap-x-6 ${compact ? 'grid-cols-1' : 'grid-cols-2'}`}>
        <CountRow label="Orders accepted" hint={`${count(s.orders_by_side.buy ?? 0)} buy · ${count(s.orders_by_side.sell ?? 0)} sell`} value={count(r.ok ?? 0)} />
        <CountRow label="Rejected" value={count(r.rejected ?? 0)} alarm={(r.rejected ?? 0) > 0} />
        <CountRow label="No verdict at send" hint="Settled from the order book" value={count(r.ambiguous ?? 0)} />
        <CountRow label="Session expired" value={count(r.session_expired ?? 0)} alarm={(r.session_expired ?? 0) > 0} />
        <CountRow label="Cancels" hint={`${count(s.order_cancels.error ?? 0)} failed`} value={count(sumValues(s.order_cancels))} />
        <CountRow label="Order-book poll failures" value={count(s.order_book_poll_failures_total)} alarm={s.order_book_poll_failures_total > 0} />
        <CountRow label="LIVE errors" hint={`${count(s.live_halts_total)} positions halted`} value={count(s.live_errors_total)} alarm={s.live_errors_total > 0} />
        <CountRow label="Kotak lock wait p95" hint="Last 30 s: the engine waits behind a broker call" value={ms(s.kotak_mutex_wait_p95_ms)} alarm={(s.kotak_mutex_wait_p95_ms ?? 0) >= 500} />
        <CountRow label="Kotak REST calls" hint={`${count(s.kotak_errors_total)} not 2xx`} value={count(s.kotak_requests_total)} />
        <CountRow label="Trades booked" hint="Paper and LIVE" value={count(sumValues(s.trades))} />
      </div>
    </Card>
  );
}

function StateRow({ name, level, state, detail }: { name: string; level: Level; state: string; detail?: string }) {
  return (
    <div className="flex items-start gap-3 border-t border-line py-2.5 first:border-t-0">
      <HealthDot level={level} className="mt-1.5" />
      <div className="min-w-0 flex-1">
        <div className="type-body-strong text-fg">{name}</div>
        {detail && <div className="type-caption text-fg-muted">{detail}</div>}
      </div>
      <span className="shrink-0 type-body text-fg">{state}</span>
    </div>
  );
}

/** Each connection as the sampler sees it, with what has gone wrong. */
export function ConnectionsCard({ s, marketOpen }: { s: TelemetrySample; marketOpen: boolean }) {
  const live = s.mode === 'live';
  const upstoxAge = s.upstox_last_success_age_secs;
  return (
    <Card title="Connections">
      <StateRow
        name="Kotak session"
        level={s.kotak_session_healthy ? 'ok' : s.kotak_connected || marketOpen ? 'down' : 'off'}
        state={s.kotak_session_healthy ? 'Healthy' : s.kotak_connected ? 'Session failing' : 'Not logged in'}
        detail={`Watchdog re-logins: ${count(s.kotak_relogins.ok ?? 0)} ok, ${count(s.kotak_relogins.error ?? 0)} failed`}
      />
      <StateRow
        name="Price feed"
        level={s.feed_connected ? 'ok' : marketOpen ? 'down' : 'off'}
        state={s.feed_connected ? 'Connected' : marketOpen ? 'Disconnected' : 'Closed'}
        detail={`Last tick ${ago(s.feed_last_tick_age_secs)} ago · ${count(s.feed_subscriptions)} subscriptions`}
      />
      <StateRow
        name="Telegram"
        level={s.telegram_connected ? 'ok' : 'down'}
        state={s.telegram_connected ? 'Listening' : 'Not running'}
        detail="The update loop itself, not the login"
      />
      <StateRow
        name="Upstox market data"
        level={!s.upstox_configured || !marketOpen ? 'off' : upstoxAge != null && upstoxAge <= 180 ? 'ok' : 'down'}
        state={!s.upstox_configured ? 'Not set up' : upstoxAge == null ? 'No data yet' : `${ago(upstoxAge)} ago`}
        detail={`${count(s.upstox_errors_total)} failed calls since the server started`}
      />
      <StateRow
        name="LIVE startup check"
        level={!live ? 'off' : s.live_startup_reconciled ? 'ok' : 'down'}
        state={!live ? 'Paper' : s.live_startup_reconciled ? 'Checked with Kotak' : 'Not trading yet'}
        detail="LIVE never trades before checking Kotak's positions and orders"
      />
      <StateRow name="Kill switch" level={s.kill_switch ? 'down' : 'ok'} state={s.kill_switch ? 'On · entries blocked' : 'Off'} />
    </Card>
  );
}

const STATE: Record<string, string> = { waiting: 'Waiting', active: 'Open', target1: 'Past T1', closed: 'Closed' };

/** Every position the engine tracks, as of the last sample. */
export function PositionsCard({ s, compact }: { s: TelemetrySample; compact: boolean }) {
  const rows = s.positions_table;
  const c = s.positions;
  return (
    <Card title="Tracked positions" note={c ? `${c.waiting} waiting · ${c.active} open · ${c.target1} past T1 · ${c.closed} closed` : undefined}>
      {rows == null ? (
        <p className="py-2 type-caption text-fg-muted">The engine was busy at the last sample; the table refreshes with the next.</p>
      ) : rows.length === 0 ? (
        <p className="py-2 type-caption text-fg-muted">Nothing tracked.</p>
      ) : (
        <div className="-mx-4 overflow-x-auto px-4">
          <table className={`w-full ${compact ? 'min-w-[560px]' : ''}`}>
            <thead>
              <tr className="border-b border-line type-caption text-fg-muted">
                <th className="py-2 text-left font-normal">Contract</th>
                <th className="py-2 text-left font-normal">State</th>
                <th className="py-2 text-right font-normal">Qty</th>
                <th className="py-2 text-right font-normal">Avg</th>
                <th className="py-2 text-right font-normal">Stop</th>
                <th className="py-2 text-right font-normal">LTP</th>
                <th className="py-2 text-right font-normal">P&amp;L</th>
                <th className="py-2 text-right font-normal">Price age</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((p) => (
                <tr key={p.id} className="border-b border-line last:border-0">
                  <td className="py-2 type-body text-fg">
                    {[p.instrument, p.strike, p.option_type].filter((x) => x != null && x !== '').join(' ')}
                    {p.halted && <span className="ml-2 type-label text-loss">Halted</span>}
                  </td>
                  <td className="py-2 type-body text-fg-muted">{STATE[p.state] ?? p.state}</td>
                  <td className="py-2 text-right num-medium text-fg">{p.qty}</td>
                  <td className="py-2 text-right num-medium text-fg">{price(p.avg_price > 0 ? p.avg_price : p.entry_price)}</td>
                  <td className="py-2 text-right num-medium text-fg">{p.current_sl > 0 ? price(p.current_sl) : '—'}</td>
                  <td className="py-2 text-right num-medium text-fg">{p.ltp == null ? '—' : price(p.ltp)}</td>
                  <td className={`py-2 text-right num-medium ${p.mtm == null ? 'text-fg' : p.mtm < 0 ? 'text-loss' : p.mtm > 0 ? 'text-profit' : 'text-fg'}`}>
                    {p.mtm == null ? '—' : signedMoney(p.mtm)}
                  </td>
                  <td className="py-2 text-right num-medium text-fg-muted">{p.ltp_age_secs == null ? '—' : ago(p.ltp_age_secs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}

/** Calls, failures and p95 by Kotak endpoint or dashboard route. */
export function EndpointCard({ title, rows, keyLabel, errorLabel }: { title: string; rows: TelemetrySample['kotak_endpoints']; keyLabel: string; errorLabel: string }) {
  return (
    <Card title={title} note="p95 over 30 s">
      {rows.length === 0 ? (
        <p className="py-2 type-caption text-fg-muted">No calls yet.</p>
      ) : (
        <table className="w-full table-fixed">
          <thead>
            <tr className="border-b border-line type-caption text-fg-muted">
              <th className="py-2 text-left font-normal">{keyLabel}</th>
              <th className="w-[70px] py-2 text-right font-normal">Calls</th>
              <th className="w-[70px] py-2 text-right font-normal">{errorLabel}</th>
              <th className="w-[80px] py-2 text-right font-normal">p95</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.endpoint} className="border-b border-line last:border-0">
                <td className="break-all py-2 pr-3 font-mono text-[12px] text-fg">{r.endpoint}</td>
                <td className="py-2 text-right num-medium text-fg">{count(r.total)}</td>
                <td className={`py-2 text-right num-medium ${r.errors > 0 ? 'text-loss' : 'text-fg'}`}>{count(r.errors)}</td>
                <td className="py-2 text-right num-medium text-fg">{ms(r.p95_ms)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  );
}
