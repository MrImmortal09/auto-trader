import { useState } from 'react';
import type { PerpAccount } from '../../lib/types';
import { toneOf } from '../../lib/format';
import { Button } from '../../ui/Button';
import { Chip } from '../../ui/Chip';
import { Segment } from '../../ui/Segment';
import { perpLabel, perpPrice, roundPrice, roundSize, sideWord, signedUsd, sizeText, usd } from './model';
import type { Perps } from './usePerps';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

type View = 'open' | 'waiting' | 'fills' | 'activity';

/**
 * The trading panel's Perps tab: the paper perps wallet, then the open
 * positions (each with its stop, targets and liquidation, Close half and
 * Close), what waits (resting limits and line orders, each with ×), the fills
 * since the last reset, and the activity. Drag a position's lines on its
 * chart to move its stop and targets.
 */
export function PerpTab({ account, perps, onOpen, compact = false }: {
  account: PerpAccount | null;
  perps: Perps;
  /** Show a perp's chart and ticket. */
  onOpen: (key: 'BTC' | 'ETH' | 'GOLD') => void;
  /** Phone: each position a card, 44 pt buttons. */
  compact?: boolean;
}) {
  const [view, setView] = useState<View>('open');
  const [busy, setBusy] = useState<string | null>(null);
  if (!account) return <p className="px-1 py-3 type-body text-fg-subtle">{perps.error ?? 'Loading the perps wallet…'}</p>;
  const run = async (id: string, act: () => Promise<boolean>) => {
    setBusy(id);
    await act();
    setBusy(null);
  };
  const waiting = account.orders.length + account.lines.length;
  const flat = account.positions.length === 0 && waiting === 0;
  // LIVE: the owner's Hyperliquid account, every action sent to Hyperliquid.
  const live = account.mode === 'live';

  const GRID = 'grid grid-cols-[minmax(140px,1.2fr)_120px_100px_100px_100px_110px_minmax(150px,1fr)_150px] items-center gap-2';
  const cards = (
    <div className="flex flex-col gap-3">
      {account.positions.map((p) => {
        const k = p.market;
        const half = roundSize(k, p.size / 2);
        const targets = (p.levels?.targets ?? []).map((t) => `${perpPrice(t.price)} (${t.size == null ? 'all left' : sizeText(k, t.size)})`).join(', ');
        return (
          <article key={p.id} className="flex flex-col gap-2 border-b border-line pb-3">
            <header className="flex items-start justify-between gap-3">
              <button type="button" onClick={() => onOpen(k)} className="min-w-0 text-left">
                <div className="type-body-strong text-fg">{p.label}</div>
                <div className="type-caption text-fg-subtle tnum">
                  {sideWord(p.side)} {sizeText(k, p.size)} · {p.leverage}x · margin {usd(p.margin)}
                </div>
              </button>
              <div className="shrink-0 text-right">
                <div className={`num-large ${TONE_TEXT[toneOf(p.upnl)]}`}>{p.upnl == null ? '—' : signedUsd(p.upnl)}</div>
                {p.roe_pct != null && <div className={`type-caption tnum ${TONE_TEXT[toneOf(p.roe_pct)]}`}>{p.roe_pct >= 0 ? '+' : '−'}{Math.abs(p.roe_pct).toFixed(2)}%</div>}
              </div>
            </header>
            <p className="type-caption text-fg-muted tnum">
              Entry {perpPrice(p.entry)} · mark {p.mark == null ? '—' : perpPrice(p.mark)} · <span className="text-loss">liq {p.liq == null ? '—' : perpPrice(roundPrice(k, p.liq))}</span>
            </p>
            <p className="type-caption tnum">
              <span className={p.levels?.stop != null ? 'text-loss' : 'text-fg-subtle'}>{p.levels?.stop != null ? `Stop ${perpPrice(p.levels.stop)}` : 'No stop'}</span>
              {targets && <span className="text-profit"> · targets {targets}</span>}
            </p>
            {p.stop_note && <p className="type-caption text-loss">{p.stop_note}</p>}
            <div className="grid grid-cols-4 gap-2">
              <Button size="lg" kind="ghost" disabled={live} onClick={() => (onOpen(k), perps.place(k, 'stop'))}>
                + Stop
              </Button>
              <Button size="lg" kind="ghost" disabled={live} onClick={() => (onOpen(k), perps.place(k, 'target'))}>
                + Target
              </Button>
              <Button pointerOnly size="lg" kind="loss-outline" disabled={live || busy != null || half <= 0} onClick={() => void run(p.id, () => perps.actions.close(k, half))}>
                ½
              </Button>
              <Button pointerOnly size="lg" kind="loss" disabled={live || busy != null} onClick={() => void run(p.id, () => perps.actions.close(k))}>
                Close
              </Button>
            </div>
          </article>
        );
      })}
    </div>
  );
  const open =
    account.positions.length === 0 ? (
      <p className="px-1 py-3 type-body text-fg-subtle">No perp open. Pick BTC, ETH or GOLD in the header to trade it.</p>
    ) : (
      <div className="flex flex-col">
        <div className={`${GRID} border-b border-line px-1 py-2 type-eyebrow text-fg-muted`}>
          <span>Perp</span>
          <span className="text-right">Size</span>
          <span className="text-right">Entry</span>
          <span className="text-right">Mark</span>
          <span className="text-right">Liq.</span>
          <span className="text-right">P&L</span>
          <span>Stop · targets</span>
          <span />
        </div>
        {account.positions.map((p) => {
          const k = p.market;
          const half = roundSize(k, p.size / 2);
          return (
            <div key={p.id} className={`${GRID} border-b border-line px-1 py-2.5`}>
              <button type="button" onClick={() => onOpen(k)} className="flex min-w-0 flex-col text-left" title="Show its chart">
                <span className="truncate type-body-strong text-fg">{p.label}</span>
                <span className="type-caption text-fg-subtle tnum">
                  {p.leverage}x isolated · margin {usd(p.margin)}
                </span>
              </button>
              <span className="flex items-center justify-end gap-2">
                <Chip kind={p.side === 'long' ? 'profit' : 'loss'}>{sideWord(p.side)}</Chip>
                <span className="num-medium text-fg">{Number(p.size.toFixed(5))}</span>
              </span>
              <span className="text-right num-medium text-fg">{perpPrice(p.entry)}</span>
              <span className="text-right num-medium text-fg">{p.mark == null ? '—' : perpPrice(p.mark)}</span>
              <span className="text-right num-medium text-loss">{p.liq == null ? '—' : perpPrice(roundPrice(k, p.liq))}</span>
              <span className="flex flex-col items-end">
                <span className={`num-medium ${TONE_TEXT[toneOf(p.upnl)]}`}>{p.upnl == null ? '—' : signedUsd(p.upnl)}</span>
                {p.roe_pct != null && <span className={`type-caption tnum ${TONE_TEXT[toneOf(p.roe_pct)]}`}>{p.roe_pct >= 0 ? '+' : '−'}{Math.abs(p.roe_pct).toFixed(2)}%</span>}
              </span>
              <span className="flex min-w-0 flex-col type-caption tnum">
                <span className={p.levels?.stop != null ? 'text-loss' : 'text-fg-subtle'}>
                  {p.levels?.stop != null ? `Stop ${perpPrice(p.levels.stop)}` : (
                    <button type="button" className="text-loss hover:underline disabled:opacity-50 disabled:no-underline" onClick={() => (onOpen(k), perps.place(k, 'stop'))}>
                      + Stop
                    </button>
                  )}
                </span>
                {p.stop_note && <span className="text-loss">{p.stop_note}</span>}
                {(p.levels?.targets ?? []).map((t) => (
                  <span key={t.id} className="text-profit">
                    Target {perpPrice(t.price)} · {t.size == null ? 'all left' : sizeText(k, t.size)}
                  </span>
                ))}
                <button type="button" className="self-start text-profit hover:underline disabled:opacity-50 disabled:no-underline" onClick={() => (onOpen(k), perps.place(k, 'target'))}>
                  + Target
                </button>
                {p.funding !== 0 && <span className="text-fg-subtle">Funding {p.funding > 0 ? 'paid' : 'received'} {usd(Math.abs(p.funding))}</span>}
              </span>
              <span className="flex justify-end gap-2">
                <Button pointerOnly kind="loss-outline" disabled={live || busy != null || half <= 0} onClick={() => void run(p.id, () => perps.actions.close(k, half))} title={`Close ${sizeText(k, half)} at the mid`}>
                  ½
                </Button>
                <Button pointerOnly kind="loss" disabled={busy != null} onClick={() => void run(p.id, () => perps.actions.close(k))} title={live ? 'Close it all at the market (IOC at the mid ± 0.5 %)' : 'Close it all at the mid'}>
                  Close
                </Button>
              </span>
            </div>
          );
        })}
      </div>
    );

  const waitingList =
    waiting === 0 ? (
      <p className="px-1 py-3 type-body text-fg-subtle">Nothing waiting. A limit that isn't reached yet, or a line order, shows here and on its chart.</p>
    ) : (
      <ul className="flex flex-col">
        {account.orders.map((o) => (
          <li key={o.id} className="flex items-center gap-3 border-b border-line px-1 py-2">
            <span className="min-w-0 flex-1 type-body text-fg">
              <span className="type-body-strong">{perpLabel(o.market)}</span> · limit {o.side === 'long' ? 'buy' : 'sell'} {sizeText(o.market, o.size)} at {perpPrice(o.price)}
              {o.reduce_only ? ' · reduce only' : o.leverage > 0 ? ` · ${o.leverage}x` : ''}
            </span>
            <Button pointerOnly kind="neutral" disabled={busy != null} onClick={() => void run(o.id, () => perps.actions.cancelOrder(o.id))}>
              Cancel
            </Button>
          </li>
        ))}
        {account.lines.map((l) => (
          <li key={l.id} className="flex items-center gap-3 border-b border-line px-1 py-2">
            <span className="min-w-0 flex-1 type-body text-fg">
              <span className="type-body-strong">{perpLabel(l.market)}</span> · line: {sideWord(l.side).toLowerCase()} {sizeText(l.market, l.size)}{l.leverage > 0 ? ` at ${l.leverage}x` : ''} when the mark {l.rising ? 'rises' : 'falls'} to {perpPrice(l.price)}
              {l.valid_until ? ` · till ${l.valid_until.slice(11, 16)}` : ' · until cancelled'}
            </span>
            <Button pointerOnly kind="neutral" disabled={busy != null} onClick={() => void run(l.id, () => perps.actions.cancelLine(l.id))}>
              Cancel
            </Button>
          </li>
        ))}
      </ul>
    );

  const fills =
    account.fills.length === 0 ? (
      <p className="px-1 py-3 type-body text-fg-subtle">No fills since the wallet started.</p>
    ) : (
      <ul className="flex flex-col">
        {account.fills.map((f) => {
          const net = f.realized - f.fee - f.funding;
          const closing = f.realized !== 0 || f.funding !== 0;
          return (
            <li key={f.id} className="flex items-center gap-3 border-b border-line px-1 py-2 type-caption tnum">
              <span className="w-16 shrink-0 text-fg-muted">{f.timestamp.slice(11, 19)}</span>
              <span className={`w-10 shrink-0 ${f.action === 'BUY' ? 'text-profit' : 'text-loss'}`}>{f.action}</span>
              <span className="min-w-0 flex-1 text-fg">
                {f.size} {perpLabel(f.market)} at {perpPrice(f.price)} · {f.reason.toLowerCase()}
              </span>
              <span className="text-fg-muted">fee {usd(f.fee)}</span>
              {closing && <span className={`w-24 text-right ${TONE_TEXT[toneOf(net)]}`}>{signedUsd(net)}</span>}
            </li>
          );
        })}
      </ul>
    );

  const activity =
    account.activity.length === 0 ? (
      <p className="px-1 py-3 type-body text-fg-subtle">Nothing yet. Fills, orders, levels and funding show here until the server next restarts.</p>
    ) : (
      <ul className="flex flex-col">
        {account.activity.map((a, i) => (
          <li key={`${a.t}:${i}`} className="flex gap-3 border-b border-line px-1 py-2 type-caption">
            <span className="w-16 shrink-0 text-fg-muted tnum">{a.t.slice(11, 19)}</span>
            <span className="min-w-0 flex-1 text-fg">{a.message}</span>
          </li>
        ))}
      </ul>
    );

  return (
    <div className="flex flex-col gap-2 py-2">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 px-1 type-caption tnum">
        <span className="type-label text-fg">{live ? `Perps · LIVE (${account.network})` : 'Perps · paper'}</span>
        <span className="text-fg-muted">
          Equity <span className="text-fg">{usd(account.equity)}</span>
        </span>
        <span className="text-fg-muted">
          Free <span className="text-fg">{usd(account.cash)}</span>
        </span>
        <span className="text-fg-muted">
          Open P&L <span className={TONE_TEXT[toneOf(account.upnl)]}>{signedUsd(account.upnl)}</span>
        </span>
        {!live && (
          <span className="text-fg-muted">
            Since {usd(account.start)} <span className={TONE_TEXT[toneOf(account.equity - account.start)]}>{signedUsd(account.equity - account.start)}</span>
          </span>
        )}
        {(account.venues ?? []).map((v) => (
          <span key={v.dex} className="text-fg-muted">
            {v.label} <span className="text-fg">{usd(v.account_value)}</span>
          </span>
        ))}
        <span className="flex-1" />
        <Segment
          label="Perps view"
          size={compact ? 'sm' : 'xs'}
          value={view}
          onChange={setView}
          options={[
            { value: 'open', label: `Open ${account.positions.length}` },
            { value: 'waiting', label: `Waiting ${waiting}` },
            { value: 'fills', label: 'Fills' },
            { value: 'activity', label: 'Activity' },
          ]}
        />
        {!live && (
          <Button kind="quiet" disabled={!flat || busy != null} title={flat ? 'Back to 10,000 USDC' : 'Close and cancel everything first'} onClick={() => void run('reset', perps.actions.reset)}>
            Reset
          </Button>
        )}
      </div>
      {live && perps.error && (
        <p className="px-1 type-caption text-loss">
          Couldn't refresh the LIVE account: {perps.error}. Shown as read{account.as_of ? ` at ${account.as_of.slice(11)} IST` : ' earlier'} — a stop, target or liquidation since then isn't on it.
        </p>
      )}
      {view === 'open' && (compact && account.positions.length > 0 ? cards : open)}
      {view === 'waiting' && waitingList}
      {view === 'fills' && fills}
      {view === 'activity' && activity}
    </div>
  );
}
