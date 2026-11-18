import { useEffect, useRef, useState } from 'react';
import { Eye } from 'lucide-react';
import { lookupPublicTrades } from '../lib/api';
import { dateRange, dayLabel, hhmm, money, pct, plural, price, signedMoney, toneOf } from '../lib/format';
import { useServerBase } from '../lib/hooks';
import { callContract, reasonLabel } from '../lib/labels';
import type { PublicCall, PublicTrades, RangeId } from '../lib/types';
import { Mark } from '../shell/Mark';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Segment } from '../ui/Segment';
import { Stat } from '../ui/Stat';
import { useIdentity } from './identity';
import { formatPhone, normalizePhone } from './phone';
import { VIEW_PATH } from './viewPath';

const RANGES: { value: RangeId; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '7d', label: '7 days' },
  { value: '30d', label: '30 days' },
  { value: 'all', label: 'All' },
];

const TONE = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** The average price of a call's buys or sells. */
function avg(call: PublicCall, action: 'BUY' | 'SELL'): number | null {
  const legs = call.legs.filter((l) => l.action.toUpperCase() === action);
  const qty = legs.reduce((n, l) => n + l.qty, 0);
  return qty > 0 ? legs.reduce((n, l) => n + l.qty * l.price, 0) / qty : null;
}

/**
 * The view-only page: type a number the admin has made public and see its
 * closed trades and P&L. No sign-in and no code; nothing here can trade, and
 * nothing still open is shown.
 */
export function ViewerPage() {
  const serverBase = useServerBase();
  const { role } = useIdentity();
  const [linked] = useState(() => normalizePhone(new URLSearchParams(window.location.search).get('n') ?? ''));
  const [typed, setTyped] = useState(linked ?? '');
  const [phone, setPhone] = useState<string | null>(linked);
  const [range, setRange] = useState<RangeId>('30d');
  const [data, setData] = useState<PublicTrades | null>(null);
  const [busy, setBusy] = useState(linked != null);
  const [error, setError] = useState<string | null>(null);
  const asked = useRef(0);

  /** Ask for `p`'s trades; only the latest ask is shown. */
  function fetchTrades(p: string, r: RangeId) {
    const mine = ++asked.current;
    void lookupPublicTrades(serverBase, p, r).then((answer) => {
      if (mine !== asked.current) return;
      setBusy(false);
      if ('error' in answer) {
        setData(null);
        setError(answer.error);
      } else {
        setData(answer);
      }
    });
  }

  function load(p: string, r: RangeId) {
    setBusy(true);
    setError(null);
    fetchTrades(p, r);
  }

  // A shared link (`/view?n=…`) opens on its number's trades.
  const opened = useRef(false);
  useEffect(() => {
    if (opened.current || !linked) return;
    opened.current = true;
    fetchTrades(linked, '30d');
  });

  function submit() {
    const normalized = normalizePhone(typed);
    if (!normalized) {
      setError('Enter a 10-digit Indian mobile number.');
      return;
    }
    window.history.replaceState(null, '', `${VIEW_PATH}?n=${normalized}`);
    // Another number's trades never sit under this one's name while it loads.
    if (normalized !== phone) setData(null);
    setPhone(normalized);
    load(normalized, range);
  }

  function pickRange(r: RangeId) {
    setRange(r);
    if (phone) load(phone, r);
  }

  const who = data && phone ? data.name || `+91 ${formatPhone(phone)}` : null;

  return (
    <div className="flex min-h-dvh flex-col items-center bg-canvas px-4 py-8 text-fg">
      <div className="flex w-full max-w-[720px] flex-col gap-5">
        <div className="flex items-center gap-3">
          <Mark />
          <div className="min-w-0 flex-1">
            <h1 className="type-title text-fg">Auto Trader</h1>
            <p className="type-caption text-fg-subtle">Closed trades of a public number.</p>
          </div>
          <Chip kind="accent" icon={<Eye size={12} />}>
            View only
          </Chip>
        </div>

        <form
          className="flex flex-col gap-3 rounded-16 border border-line bg-panel p-4 shadow-lg sm:flex-row sm:items-end"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <label className="flex flex-1 flex-col gap-1.5">
            <span className="type-label text-fg-muted">Mobile number</span>
            <span className="relative flex items-center">
              <span className="pointer-events-none absolute left-3 type-body text-fg-subtle">+91</span>
              <input
                type="tel"
                inputMode="numeric"
                aria-label="Mobile number"
                autoFocus={!phone}
                maxLength={14}
                value={typed}
                onChange={(e) => {
                  setTyped(e.target.value.replace(/[^\d\s+-]/g, ''));
                  setError(null);
                }}
                className="h-12 w-full rounded-8 border border-line bg-inset pl-12 pr-3 text-fg num-medium tracking-[0.08em] transition-colors duration-120 focus:border-accent"
              />
            </span>
          </label>
          <Button size="lg" kind="accent" type="submit" disabled={busy || typed.replace(/\D/g, '').length < 10}>
            {busy ? 'Loading…' : 'View trades'}
          </Button>
        </form>

        {error && <p className="type-body text-loss">{error}</p>}

        {data && who && (
          <>
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="type-title min-w-0 truncate text-fg">{who}</h2>
              {data.mode === 'LIVE' ? <Chip kind="live">LIVE</Chip> : <Chip kind="neutral">Paper</Chip>}
              <span className="flex-1" />
              <Segment options={RANGES} value={range} onChange={pickRange} label="Range" disabled={busy} />
            </div>

            <div className="grid grid-cols-2 gap-4 rounded-16 border border-line bg-panel p-4 sm:grid-cols-4">
              <Stat
                label="Net P&L"
                value={signedMoney(data.kpis.net)}
                tone={toneOf(data.kpis.net)}
                sub={data.from ? dateRange(data.from, data.to) : 'All time'}
              />
              <Stat label="Trades" value={data.kpis.calls} sub={`${data.kpis.wins} won · ${data.kpis.losses} lost`} />
              <Stat label="Win rate" value={data.kpis.win_rate == null ? '–' : pct(data.kpis.win_rate)} />
              <Stat label="Charges" value={money(data.kpis.charges)} sub="included in P&L" />
            </div>

            <div className="overflow-hidden rounded-16 border border-line bg-panel">
              {data.calls.length === 0 ? (
                <p className="p-4 type-body text-fg-subtle">No closed trades in this range.</p>
              ) : (
                data.calls.map((c, i) => <TradeRow key={`${c.closed_at}-${i}`} call={c} />)
              )}
              {data.more_calls > 0 && (
                <p className="border-t border-line p-3 type-caption text-fg-subtle">
                  {plural(data.more_calls, 'older trade')} not listed; the totals count them.
                </p>
              )}
            </div>
          </>
        )}

        <p className="type-caption text-fg-subtle">
          Only trades already closed are shown, for numbers the admin has made public.{' '}
          <a href="/" className="text-accent underline-offset-2 transition-colors duration-120 hover:text-accent-strong hover:underline">
            {role === 'anon' ? 'Sign in' : 'Open the app'}
          </a>
        </p>
      </div>
    </div>
  );
}

function TradeRow({ call }: { call: PublicCall }) {
  const buy = avg(call, 'BUY');
  const sell = avg(call, 'SELL');
  const closed = call.closed_at ?? call.opened_at;
  const reason = reasonLabel(call.exit_reason);
  return (
    <div className="flex items-center gap-3 border-t border-line px-4 py-3 first:border-t-0">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="type-body-strong truncate text-fg">{callContract(call)}</span>
          {reason && <Chip kind="outline">{reason}</Chip>}
        </div>
        <div className="type-caption text-fg-subtle tnum">
          {dayLabel(closed)} · {hhmm(call.opened_at)}–{hhmm(closed)} · qty {call.qty}
          {buy != null && sell != null && ` · ${price(buy)} → ${price(sell)}`}
        </div>
      </div>
      <span className={`num-medium whitespace-nowrap ${TONE[toneOf(call.pnl)]}`}>{call.pnl == null ? '–' : signedMoney(call.pnl)}</span>
    </div>
  );
}
