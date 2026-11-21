import { getAdminUserHl, getFlags } from '../lib/api';
import { usePoll } from '../lib/usePoll';
import { perpPrice, signedUsd, sizeText, usd } from '../trade/perp/model';
import { Chip } from '../ui/Chip';

const TONE = (n: number) => (n > 0 ? 'text-profit' : n < 0 ? 'text-loss' : 'text-fg');

/**
 * A user's Hyperliquid LIVE desk on their P&L report, for the admin to see,
 * read-only (the owner's pick): Paper or LIVE, the account their API wallet
 * trades for and, while LIVE, what it holds and its day. Nothing here trades.
 */
export function UserHlLive({ serverBase, userId }: { serverBase: string; userId: number }) {
  const flags = usePoll(() => getFlags(serverBase), 60_000, serverBase);
  const hl = usePoll(() => getAdminUserHl(serverBase, userId), 10_000, `${serverBase}|${userId}`);
  const d = hl.data;
  // Only while Hyperliquid is switched on.
  if (flags.data?.hyperliquid !== true) return null;
  if (!d) return hl.error ? <div className="type-caption text-fg-subtle">Hyperliquid: {hl.error}</div> : null;
  const { live, account, today, error } = d;
  const cred = live.credentials[live.network];
  const isLive = live.mode === 'live';
  return (
    <div className="flex flex-col gap-1.5 type-caption text-fg-muted">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-fg">Hyperliquid</span>
        {isLive ? <Chip kind="live">LIVE · {live.network}</Chip> : <Chip kind="neutral">Paper</Chip>}
        {!live.allowed && <Chip kind="outline">{isLive ? 'LIVE switched off: close-only' : 'LIVE not allowed'}</Chip>}
        {live.kill && <Chip kind="loss">Kill switch on</Chip>}
        {cred && <span className="tnum text-fg-subtle">account {cred.account}</span>}
      </div>
      {live.kill_note && <div className="text-fg-subtle">{live.kill_note}</div>}
      {error && <div className="text-loss">{error}</div>}
      {isLive && today && (
        <div className="tnum">
          Today <span className={TONE(today.net)}>{signedUsd(today.net)}</span> · {today.trades} trade{today.trades === 1 ? '' : 's'} · equity {usd(today.equity)}
        </div>
      )}
      {isLive && account && (
        <div className="flex flex-col gap-0.5 tnum">
          {account.positions.length === 0 ? (
            <span className="text-fg-subtle">No position open{account.orders.length + account.lines.length > 0 ? ` · ${account.orders.length + account.lines.length} order(s) resting` : ''}</span>
          ) : (
            account.positions.map((p) => (
              <span key={p.id}>
                {p.label} {p.side === 'long' ? 'long' : 'short'} {sizeText(p.market, p.size)} at {perpPrice(p.entry)} · {p.leverage}x
                {p.mark != null && ` · mark ${perpPrice(p.mark)}`}
                {p.upnl != null && (
                  <>
                    {' · '}
                    <span className={TONE(p.upnl)}>{signedUsd(p.upnl)}</span>
                  </>
                )}
                {p.levels?.stop != null ? ` · stop ${perpPrice(p.levels.stop)}` : ' · no stop'}
              </span>
            ))
          )}
        </div>
      )}
    </div>
  );
}
