import { useState, type ReactNode } from 'react';
import { cancelPosition, exitPosition, getDesk, getFeed, getFlags, getMyDesk, paperClose, paperSellLots, sellQty, setLots, type Sent } from '../lib/api';
import { money, plural, signedMoney, toneOf } from '../lib/format';
import { useIsWide } from '../lib/hooks';
import { usePoll } from '../lib/usePoll';
import type { DeskAnswer, DeskPosition, Stopped } from '../lib/types';
import { useGuard } from '../shell/useGuard';
import { Segment } from '../ui/Segment';
import { Stat } from '../ui/Stat';
import { useToast } from '../ui/toast';
import { CallRow, OpenRow, WaitingRow } from './Rows';
import { SyncWithKotak } from './SyncWithKotak';
import { TelegramFeed } from './TelegramFeed';

function Section({ title, count, children }: { title: string; count: number; children: ReactNode }) {
  return (
    <section className="flex flex-col">
      <h2 className="mb-1 flex items-baseline gap-2 type-title text-fg">
        {title}
        <span className="type-caption text-fg-muted tnum">{count}</span>
      </h2>
      {children}
    </section>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="py-2 type-body text-fg-muted">{children}</p>;
}

/** `record` less `key`. */
function without<T>(record: Record<string, T>, key: string): Record<string, T> {
  const next = { ...record };
  delete next[key];
  return next;
}

function walletLabel(d: DeskAnswer): string {
  if (d.wallet.source === 'PAPER') return 'Paper wallet';
  return 'Wallet';
}

/** Desk · Today: the figures, open and waiting positions, the calls closed
 * today, and the live Telegram feed. With `paper`, a user's own portfolio:
 * no waiting calls, Telegram feed or Kotak sync, and sells fill at once. */
export function DeskToday({ serverBase, desktop, rangeSwitch, stopped, paper = false }: {
  serverBase: string;
  desktop: boolean;
  rangeSwitch: ReactNode;
  /** What the kill switch did, while it's on. */
  stopped: Stopped | null;
  paper?: boolean;
}) {
  const wide = useIsWide() && !paper;
  const desk = usePoll(() => (paper ? getMyDesk(serverBase) : getDesk(serverBase)), 2000, `${serverBase}|${paper ? 'paper' : 'owner'}`);
  const feed = usePoll(() => getFeed(serverBase), 4000, serverBase, !paper);
  // Whether a scalp's stop and target can be set: the owner's switch; paper always.
  const flags = usePoll(() => getFlags(serverBase), 30_000, serverBase, !paper);
  const levelsOn = paper || (flags.data?.chart_stops ?? true);
  const scope = paper ? 'user' : 'admin';
  const guard = useGuard();
  const toast = useToast();
  // Rows with an action in flight: their buttons wait for the answer and the
  // fresh positions after it, so a second tap can't land on stale state.
  const [sending, setSending] = useState<Record<string, true>>({});
  // Lots just set on a waiting call, shown until the server has them.
  const [lotsShown, setLotsShown] = useState<Record<string, number>>({});
  const [pane, setPane] = useState<'positions' | 'feed'>('positions');

  async function act(id: string, reason: string, failTitle: string, run: () => Promise<Sent>): Promise<Sent | null> {
    setSending((s) => ({ ...s, [id]: true }));
    try {
      const sent = await guard(reason, run, failTitle, scope);
      await desk.refresh();
      return sent;
    } finally {
      setSending((s) => without(s, id));
    }
  }

  // A paper sell fills at once; its toast, with the P&L, comes from the status bar.
  async function lessLot(p: DeskPosition) {
    const sell = () => (paper ? paperSellLots(serverBase, p.id, 1) : sellQty(serverBase, p.id, p.lot_size));
    const sent = await act(p.id, 'Selling needs write access.', `Couldn't sell ${p.contract}`, sell);
    if (sent?.ok && sent.status === 202) toast({ tone: 'neutral', title: `Selling 1 lot of ${p.contract}`, body: 'Sent to Kotak at market — the fill shows here when it is done.' });
  }

  async function exit(p: DeskPosition) {
    const close = () => (paper ? paperClose(serverBase, p.id) : exitPosition(serverBase, p.id));
    const sent = await act(p.id, 'Exiting needs write access.', `Couldn't exit ${p.contract}`, close);
    if (sent?.ok && sent.status === 202) toast({ tone: 'neutral', title: `Exiting ${p.contract}`, body: 'Sent to Kotak at market — the fill shows here when it is done.' });
  }

  async function cancel(p: DeskPosition) {
    const sent = await act(p.id, 'Cancelling needs write access.', `Couldn't cancel ${p.contract}`, () => cancelPosition(serverBase, p.id));
    if (sent?.ok) toast({ tone: 'neutral', title: `Cancelled ${p.contract}`, body: 'The call will not be bought.' });
  }

  async function lots(p: DeskPosition, next: number) {
    setLotsShown((s) => ({ ...s, [p.id]: next }));
    await act(p.id, 'Changing lots needs write access.', `Couldn't change the lots on ${p.contract}`, () => setLots(serverBase, p.id, next));
    setLotsShown((s) => without(s, p.id));
  }

  const d = desk.data;
  const k = d?.kpis;

  const kpis = d && k && (
    desktop ? (
      <div className="flex flex-wrap items-end gap-x-10 gap-y-4 border-b border-line pb-5">
        <Stat label="Net today" value={signedMoney(k.net)} size="hero" tone={toneOf(k.net)} />
        <Stat label="Booked" value={signedMoney(k.booked)} tone={toneOf(k.booked)} />
        <Stat label="Open" value={signedMoney(k.open)} tone={toneOf(k.open)} />
        <Stat label={paper ? 'Trades today' : 'Calls today'} value={k.calls} />
        <Stat
          label={walletLabel(d)}
          value={d.wallet.balance == null ? '—' : money(d.wallet.balance)}
          sub={d.wallet.source === 'LIVE_UNAVAILABLE' ? 'Kotak balance not read yet' : undefined}
        />
      </div>
    ) : (
      <div className="flex flex-col gap-3">
        <Stat label="Net today" value={signedMoney(k.net)} size="hero" tone={toneOf(k.net)} />
        <div className="grid grid-cols-3 gap-3">
          <Stat label="Booked" value={signedMoney(k.booked)} size="medium" tone={toneOf(k.booked)} />
          <Stat label="Open" value={signedMoney(k.open)} size="medium" tone={toneOf(k.open)} />
          <Stat label={paper ? 'Trades today' : 'Calls today'} value={k.calls} size="medium" />
        </div>
      </div>
    )
  );

  const list = desktop ? 'flex flex-col' : 'flex flex-col gap-3';
  const positions = d && (
    <div className="flex flex-col gap-6">
      <Section title="Open" count={d.open.length}>
        {d.open.length === 0 ? (
          <Empty>
            Nothing open.
            {stopped?.positions_exited ? ` The kill switch sold ${plural(stopped.positions_exited, 'position')} at market${stopped.at ? ` at ${stopped.at}` : ''}.` : ''}
          </Empty>
        ) : (
          <div className={list}>
            {d.open.map((p) => (
              <OpenRow key={p.id} p={p} desktop={desktop} sending={!!sending[p.id]} levelsOn={levelsOn} onLessLot={() => void lessLot(p)} onExit={() => void exit(p)} />
            ))}
          </div>
        )}
      </Section>
      {!paper && <Section title="Waiting" count={d.waiting.length}>
        {d.waiting.length === 0 ? (
          <Empty>
            {stopped?.entries_cancelled
              ? `The kill switch cancelled ${stopped.entries_cancelled === 1 ? 'the waiting call' : `${stopped.entries_cancelled} waiting calls`}.`
              : 'No call is waiting.'}
          </Empty>
        ) : (
          <div className={list}>
            {d.waiting.map((p) => (
              <WaitingRow
                key={p.id}
                p={p}
                desktop={desktop}
                lots={lotsShown[p.id] ?? p.lots}
                sending={!!sending[p.id]}
                onLots={(n) => void lots(p, n)}
                onCancel={() => void cancel(p)}
              />
            ))}
          </div>
        )}
      </Section>}
      <Section title="Closed today" count={d.closed.length}>
        {d.closed.length === 0 ? (
          <Empty>Nothing closed yet today.</Empty>
        ) : (
          <div className="flex flex-col">
            {d.closed.map((c) => (
              <CallRow key={c.key} call={c} desktop={desktop} />
            ))}
          </div>
        )}
      </Section>
    </div>
  );

  const loading = !d && (
    <p className={`type-body ${desk.error ? 'text-loss' : 'text-fg-muted'}`}>{desk.error ? `Couldn't load the desk: ${desk.error}` : 'Loading…'}</p>
  );
  const feedPane = <TelegramFeed feed={feed.data} error={feed.error} showTitle={desktop} />;
  const paneSwitch = (
    <Segment
      label="Show"
      size={desktop ? 'sm' : 'lg'}
      stretch={!desktop}
      value={pane}
      onChange={setPane}
      options={[
        { value: 'positions', label: 'Positions' },
        { value: 'feed', label: 'Telegram calls' },
      ]}
    />
  );

  if (desktop) {
    return (
      <div className="flex min-h-full">
        <div className="flex min-w-0 flex-1 flex-col gap-5 px-6 py-5">
          <div className="flex items-center gap-4">
            {rangeSwitch}
            <span className="flex-1" />
            {!paper && <SyncWithKotak serverBase={serverBase} onSynced={() => void desk.refresh()} />}
          </div>
          {kpis}
          {loading}
          {!wide && !paper && paneSwitch}
          {wide || paper || pane === 'positions' ? positions : feedPane}
        </div>
        {wide && <aside className="w-[320px] shrink-0 border-l border-line px-4 py-5">{feedPane}</aside>}
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-5 px-4 py-4">
      {rangeSwitch}
      {kpis}
      {loading}
      {!paper && paneSwitch}
      {paper || pane === 'positions' ? positions : feedPane}
    </div>
  );
}
