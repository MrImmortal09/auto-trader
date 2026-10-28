import { useState } from 'react';
import { formatPhone } from '../auth/phone';
import { getAdminPositions, getAdminTrades } from '../lib/api';
import { hhmm, hhmmss, money, plural, price, shortDate, signedMoney, todayIst, toneOf } from '../lib/format';
import { reasonLabel } from '../lib/labels';
import { usePoll } from '../lib/usePoll';
import type { AdminPaper, AdminUser } from '../lib/types';
import { Row, Section } from '../settings/parts';
import { Chip } from '../ui/Chip';
import { Segment } from '../ui/Segment';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;
const SELECT = 'h-8 rounded-6 border border-line bg-inset px-2 type-body text-fg outline-none focus:border-line-strong';

type Range = 'today' | '7d' | 'all';
const RANGES: { value: Range; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '7d', label: '7D' },
  { value: 'all', label: 'All' },
];

/** The first IST day `range` covers; null for all of them. */
function fromDay(range: Range): string | null {
  if (range === 'all') return null;
  const today = todayIst();
  if (range === 'today') return today;
  const d = new Date(`${today}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() - 6);
  return d.toISOString().slice(0, 10);
}

/** "10:42", or "26 Oct 10:42" on another day. */
function when(ts: string, withSeconds = false): string {
  const time = withSeconds ? hhmmss(ts) : hhmm(ts);
  return ts.startsWith(todayIst()) ? time : `${shortDate(ts.slice(0, 10))} ${time}`;
}

/** A user's name, else their number; "User 7" once they are removed. */
function nameOf(users: AdminUser[], id: number): string {
  const u = users.find((x) => x.id === id);
  return u ? u.name?.trim() || formatPhone(u.phone) : `User ${id}`;
}

/** "Wallet ₹9,88,000.00 · 2 open · today +₹1,500.00 · 14 trades". */
export function PaperLine({ p }: { p: AdminPaper }) {
  const parts = [
    `Wallet ${money(p.balance)}`,
    p.open ? `${p.open} open` : null,
    `${plural(p.trades, 'trade')}${p.last_trade_at ? `, last ${when(p.last_trade_at)}` : ''}`,
    p.resets ? `reset ${plural(p.resets, 'time')}` : null,
  ].filter(Boolean);
  return (
    <div className="tnum">
      {parts.join(' · ')} · today <span className={TONE_TEXT[toneOf(p.day_pnl)]}>{signedMoney(p.day_pnl)}</span>
    </div>
  );
}

/** Every user's open paper positions, at the live price. */
export function PaperPositions({ serverBase, users }: { serverBase: string; users: AdminUser[] }) {
  const positions = usePoll(() => getAdminPositions(serverBase), 3_000, serverBase);
  const list = positions.data ?? [];
  return (
    <Section title="Open paper positions" lead="What every user holds now, at the live price. Read-only.">
      {positions.data == null ? (
        <Row title={positions.error ? `Couldn't load the positions: ${positions.error}` : 'Loading…'} />
      ) : list.length === 0 ? (
        <Row title="Nobody holds a paper position" />
      ) : (
        list.map((p) => (
          <Row key={p.id} title={`${p.contract} · ${nameOf(users, p.user_id)}`} caption={`${p.qty} qty · avg ${price(p.avg)} · since ${when(p.opened_at)} · expires ${shortDate(p.expiry)}`}>
            <div className="shrink-0 text-right">
              <div className={`num-medium ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</div>
              <div className="type-caption text-fg-muted tnum">{p.ltp == null ? 'No price yet' : price(p.ltp)}</div>
            </div>
          </Row>
        ))
      )}
    </Section>
  );
}

/** Every user's paper buys and sells, newest first, by user and range. */
export function PaperTrades({ serverBase, users, desktop }: { serverBase: string; users: AdminUser[]; desktop: boolean }) {
  const [user, setUser] = useState<number | null>(null);
  const [range, setRange] = useState<Range>('today');
  const from = fromDay(range);
  const trades = usePoll(() => getAdminTrades(serverBase, user, from), 10_000, `${serverBase}|${user ?? 'all'}|${from ?? 'all'}`);
  const d = trades.data;
  const traders = users.filter((u) => u.role === 'user');

  const controls = (
    <div className={`flex gap-2 px-4 py-3 ${desktop ? 'items-center' : 'flex-col'}`}>
      <select aria-label="User" value={user ?? ''} onChange={(e) => setUser(e.target.value ? Number(e.target.value) : null)} className={SELECT}>
        <option value="">All users</option>
        {traders.map((u) => (
          <option key={u.id} value={u.id}>
            {nameOf(users, u.id)}
          </option>
        ))}
      </select>
      <Segment label="Range" size="sm" value={range} onChange={setRange} options={RANGES} stretch={!desktop} />
      {d && (
        <span className={`type-caption text-fg-muted tnum ${desktop ? 'ml-auto' : ''}`}>
          {plural(d.count, 'trade')} · sells booked <span className={TONE_TEXT[toneOf(d.realized)]}>{signedMoney(d.realized)}</span>
        </span>
      )}
    </div>
  );

  const rows =
    d == null ? (
      <Row title={trades.error ? `Couldn't load the trades: ${trades.error}` : 'Loading…'} />
    ) : d.trades.length === 0 ? (
      <Row title={range === 'today' ? 'No paper trades today' : 'No paper trades in this range'} />
    ) : (
      d.trades.map((t) => {
        const buy = t.action === 'BUY';
        const reason = reasonLabel(t.reason);
        return (
          <Row
            key={t.id}
            title={
              <span className="flex min-w-0 items-center gap-2">
                <Chip kind={buy ? 'profit' : 'neutral'}>{buy ? 'Buy' : 'Sell'}</Chip>
                <span className="truncate">{t.contract}</span>
                <span className="truncate font-normal text-fg-muted">{nameOf(users, t.user_id)}</span>
                {t.before_reset && <Chip>Before reset</Chip>}
              </span>
            }
            caption={[when(t.t, true), `${t.qty} @ ${price(t.price)}`, `charges ${money(t.charges)}`, reason || null].filter(Boolean).join(' · ')}
          >
            <div className="shrink-0 text-right">
              {buy ? (
                <div className="num-medium text-fg-muted">{money(t.net)}</div>
              ) : (
                <div className={`num-medium ${TONE_TEXT[toneOf(t.pnl)]}`}>{t.pnl == null ? '—' : signedMoney(t.pnl)}</div>
              )}
            </div>
          </Row>
        );
      })
    );

  return (
    <Section title="Paper trades" lead="Every user's buys and sells, newest first, with each sell's P&L after charges. Trades from before a user started over are kept, marked. Read-only.">
      {controls}
      {rows}
      {d && d.more > 0 && <Row title={`${plural(d.more, 'older trade')} not shown`} caption="Pick one user or a shorter range to see them." />}
    </Section>
  );
}
