import type { ReactNode } from 'react';
import { getPerpToday } from '../lib/api';
import { plural, toneOf } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { PerpToday } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { perpPrice, roundPrice, sideWord, signedUsd, sizeText, unreadNote, usd } from '../trade/perp/model';
import { Stat } from '../ui/Stat';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

function Section({ title, count, children }: { title: string; count: number; children: ReactNode }) {
  return (
    <section className="flex flex-col">
      <h2 className="mb-1 flex items-baseline gap-2 type-title text-fg">
        {title}
        <span className="type-caption text-fg-subtle tnum">{count}</span>
      </h2>
      {children}
    </section>
  );
}

/** "Paper wallet", "LIVE · mainnet". */
function walletLabel(t: PerpToday): string {
  return t.mode === 'live' ? `Hyperliquid · LIVE ${t.network ?? ''}`.trim() : 'Perps wallet (paper)';
}

/**
 * Desk · Hyperliquid: the perps' today, from IST midnight, the way the F&O
 * desk shows its own — net, booked and open P&L, every charge (trading fees
 * and funding), the trades, the wallet; then what is open and what closed.
 * The owner's Hyperliquid account while LIVE, else the paper desk.
 */
export function DeskPerps({ serverBase, desktop, header }: { serverBase: string; desktop: boolean; header: ReactNode }) {
  const today = usePoll(() => getPerpToday(serverBase), 2_000, serverBase);
  const t = today.data;
  const flash = useTickFlash(t?.net);
  const charges = t ? t.fees + t.funding : 0;
  const breakdown = t && `${signedUsd(t.realized)} closed − ${usd(t.fees)} fees ${t.funding >= 0 ? '−' : '+'} ${usd(Math.abs(t.funding))} funding`;

  const kpis = t && (
    <div className={desktop ? 'flex flex-wrap items-end gap-x-8 gap-y-4 border-b border-line pb-5' : 'flex flex-col gap-3'}>
      <Stat label="Net today" value={signedUsd(t.net)} size="hero" tone={toneOf(t.net)} flash={flash} />
      <div className={desktop ? 'contents' : 'grid grid-cols-3 gap-3'}>
        <Stat label="Booked" value={signedUsd(t.booked)} size={desktop ? undefined : 'medium'} tone={toneOf(t.booked)} sub={desktop ? (breakdown ?? undefined) : undefined} />
        <Stat label="Open" value={signedUsd(t.open)} size={desktop ? undefined : 'medium'} tone={toneOf(t.open)} />
        <Stat label="Charges" value={usd(charges)} size={desktop ? undefined : 'medium'} sub={`fees ${usd(t.fees)} · funding ${t.funding >= 0 ? `${usd(t.funding)} paid` : `${usd(-t.funding)} received`}`} />
        <Stat label="Trades today" value={t.trades} size={desktop ? undefined : 'medium'} />
        <Stat label={walletLabel(t)} value={usd(t.equity)} size={desktop ? undefined : 'medium'} sub={`free ${usd(t.free)}`} />
      </div>
      {unreadNote(t) && <p className="w-full type-caption text-loss">{unreadNote(t)}</p>}
    </div>
  );

  const open = t && (
    <Section title="Open" count={t.positions.length}>
      {t.positions.length === 0 ? (
        <p className="py-2 type-body text-fg-subtle">No perp position open.</p>
      ) : (
        <div className="flex flex-col">
          {t.positions.map((p) => (
            <div key={p.id} className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5 border-b border-line py-2.5">
              <span className="flex items-baseline gap-2">
                <span className="type-body-strong text-fg">{p.label}</span>
                <span className={`type-caption ${p.side === 'long' ? 'text-profit' : 'text-loss'}`}>
                  {sideWord(p.side)} {sizeText(p.market, p.size)} · {p.leverage}x
                </span>
              </span>
              <span className="type-caption text-fg-muted tnum">
                entry {perpPrice(p.entry)} · mark {p.mark == null ? '—' : perpPrice(p.mark)} · liq {p.liq == null ? '—' : perpPrice(roundPrice(p.market, p.liq))}
                {p.levels?.stop != null ? ` · stop ${perpPrice(p.levels.stop)}` : ' · no stop'}
              </span>
              <span className={`num-medium ${TONE_TEXT[toneOf(p.upnl)]}`}>{p.upnl == null ? '—' : signedUsd(p.upnl)}</span>
              {p.stop_note && <span className="w-full type-caption text-loss">{p.stop_note}</span>}
            </div>
          ))}
        </div>
      )}
    </Section>
  );

  const closed = t && (
    <Section title="Closed today" count={t.closed.length}>
      {t.closed.length === 0 ? (
        <p className="py-2 type-body text-fg-subtle">Nothing closed yet today.</p>
      ) : (
        <div className="flex flex-col">
          {t.closed.map((c, i) => (
            <div key={`${c.time}-${c.market}-${i}`} className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5 border-b border-line py-2.5">
              <span className="flex items-baseline gap-2">
                <span className="type-caption text-fg-subtle tnum">{c.time}</span>
                <span className="type-body-strong text-fg">{c.label}</span>
                <span className="type-caption text-fg-muted">
                  {sideWord(c.side)} closed · {sizeText(c.market, c.size)} at {perpPrice(c.price)}
                </span>
              </span>
              <span className="type-caption text-fg-muted tnum">
                P&L {signedUsd(c.realized)} · fee {usd(c.fee)}
                {c.funding !== 0 ? ` · funding ${signedUsd(-c.funding)}` : ''}
              </span>
              <span className={`num-medium ${TONE_TEXT[toneOf(c.net)]}`}>{signedUsd(c.net)}</span>
            </div>
          ))}
        </div>
      )}
    </Section>
  );

  const loading = !t && <p className={`type-body ${today.error ? 'text-loss' : 'text-fg-subtle'}`}>{today.error ? `Couldn't load the perps desk: ${today.error}` : 'Loading…'}</p>;

  return (
    <div className={desktop ? 'flex flex-col gap-5 px-6 py-5' : 'flex flex-col gap-5 px-4 py-4'}>
      {header}
      {kpis}
      {loading}
      {t && <p className="type-caption text-fg-subtle">Today from 00:00 IST{t.mode === 'paper' ? ' · paper funding is counted when its position closes' : ''} · {plural(t.trades, 'order')} filled</p>}
      {open}
      {closed}
    </div>
  );
}
