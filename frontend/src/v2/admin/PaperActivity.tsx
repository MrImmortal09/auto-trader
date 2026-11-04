import { useState, type ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';
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
const SELECT = 'h-8 appearance-none rounded-6 border border-line bg-panel pl-2 pr-7 type-body text-fg outline-none transition-colors duration-120 focus:border-accent';

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

/** One line in place of a card's rows: "Loading…", nothing to show, or (in loss) why it failed. */
export function Note({ tone = 'subtle', children }: { tone?: 'subtle' | 'loss'; children: ReactNode }) {
  return <p className={`border-t border-line px-4 py-3.5 type-body first:border-t-0 ${tone === 'loss' ? 'text-loss' : 'text-fg-subtle'}`}>{children}</p>;
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
    <div className="text-fg-subtle tnum">
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
        <Note tone={positions.error ? 'loss' : 'subtle'}>{positions.error ? `Couldn't load the positions: ${positions.error}` : 'Loading…'}</Note>
      ) : list.length === 0 ? (
        <Note>Nobody holds a paper position</Note>
      ) : (
        list.map((p) => (
          <Row key={p.id} title={`${p.contract} · ${nameOf(users, p.user_id)}`} caption={<span className="text-fg-subtle tnum">{`${p.qty} qty · avg ${price(p.avg)} · since ${when(p.opened_at)} · expires ${shortDate(p.expiry)}`}</span>}>
            <div className="shrink-0 text-right">
              <div className={`num-medium ${TONE_TEXT[toneOf(p.pnl)]}`}>{p.pnl == null ? '—' : signedMoney(p.pnl)}</div>
              <div className="type-caption text-fg-subtle tnum">{p.ltp == null ? 'No price yet' : price(p.ltp)}</div>
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
      <span className="relative flex items-center">
        <select aria-label="User" value={user ?? ''} onChange={(e) => setUser(e.target.value ? Number(e.target.value) : null)} className={`${SELECT} ${desktop ? '' : 'w-full'}`}>
          <option value="">All users</option>
          {traders.map((u) => (
            <option key={u.id} value={u.id}>
              {nameOf(users, u.id)}
            </option>
          ))}
        </select>
        <ChevronDown size={14} aria-hidden className="pointer-events-none absolute right-2 text-fg-muted" />
      </span>
      <Segment label="Range" size="sm" value={range} onChange={setRange} options={RANGES} stretch={!desktop} />
      {d && (
        <span className={`type-caption text-fg-subtle tnum ${desktop ? 'ml-auto' : ''}`}>
          {plural(d.count, 'trade')} · sells booked <span className={TONE_TEXT[toneOf(d.realized)]}>{signedMoney(d.realized)}</span>
        </span>
      )}
    </div>
  );

  const rows =
    d == null ? (
      <Note tone={trades.error ? 'loss' : 'subtle'}>{trades.error ? `Couldn't load the trades: ${trades.error}` : 'Loading…'}</Note>
    ) : d.trades.length === 0 ? (
      <Note>{range === 'today' ? 'No paper trades today' : 'No paper trades in this range'}</Note>
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
            caption={<span className="text-fg-subtle tnum">{[when(t.t, true), `${t.qty} @ ${price(t.price)}`, `charges ${money(t.charges)}`, reason || null].filter(Boolean).join(' · ')}</span>}
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
      {d && d.more > 0 && <p className="border-t border-line px-4 py-3 type-caption text-fg-subtle tnum">{`${plural(d.more, 'older trade')} not shown · Pick one user or a shorter range to see them.`}</p>}
    </Section>
  );
}
