import { AlertOctagon, Power, TriangleAlert } from 'lucide-react';
import type { ExecuteFrame, Portfolio } from '../../types';
import { balanceAsOf } from '../../lib/format';
import { FEED_SLOW_MS, FEED_STALE_MS, fmtPnl, fmtRupees } from '../../lib/execute';
import { HoldButton } from './HoldButton';

type Tone = 'ok' | 'warn' | 'bad';

const DOT: Record<Tone, string> = { ok: 'bg-secondary', warn: 'bg-amber-500', bad: 'bg-error' };
const PILL: Record<Tone, string> = {
  ok: 'bg-secondary/10 text-secondary border-secondary/25',
  warn: 'bg-amber-50 text-amber-800 border-amber-300',
  bad: 'bg-error-container text-on-error-container border-error/30',
};

function Pill({ tone, children, title }: { tone: Tone; children: React.ReactNode; title?: string }) {
  return (
    <span title={title} className={`inline-flex items-center gap-1.5 px-2 py-1 rounded-full border text-[11px] font-bold whitespace-nowrap ${PILL[tone]}`}>
      <span className={`w-1.5 h-1.5 rounded-full ${DOT[tone]} ${tone === 'ok' ? 'animate-pulse' : ''}`} />
      {children}
    </span>
  );
}

function feedPill(frame: ExecuteFrame | null, streamStale: boolean) {
  if (!frame || streamStale) return <Pill tone="bad" title="The live stream from the server dropped — reconnecting">Reconnecting…</Pill>;
  const age = frame.feed.last_tick_age_ms;
  if (!frame.feed.connected || age == null) return <Pill tone="bad" title="Kotak price feed is not connected">Feed down</Pill>;
  const secs = (age / 1000).toFixed(1);
  const tone: Tone = age > FEED_STALE_MS ? 'bad' : age > FEED_SLOW_MS ? 'warn' : 'ok';
  return <Pill tone={tone} title="Time since the last price tick from Kotak">Feed {secs}s</Pill>;
}

/** Top strip: mode, feed and broker health, funds, today's scalp P&L, EXIT ALL. */
export function ScalpStatusBar({ frame, streamStale, portfolio, realized, open, manualOpen, onExitAll, exitAllBusy }: {
  frame: ExecuteFrame | null;
  streamStale: boolean;
  portfolio: Portfolio | null;
  realized: number;
  open: number;
  manualOpen: number;
  onExitAll: () => void;
  exitAllBusy: boolean;
}) {
  const mode = frame?.mode ?? '—';
  const total = realized + open;
  const funds =
    portfolio == null || portfolio.balance_source === 'LIVE_UNAVAILABLE' ? '—' : fmtRupees(portfolio.balance);
  const asOf = balanceAsOf(portfolio?.balance_updated_at);

  return (
    <div className="rounded-xl border border-outline-variant bg-surface-container-lowest shadow-sm px-3 sm:px-4 py-2.5 flex flex-wrap items-center gap-x-5 gap-y-2.5">
      <div className="flex flex-wrap items-center gap-1.5">
        <span
          className={`px-2 py-1 rounded-md text-[11px] font-extrabold tracking-wider ${
            mode === 'LIVE' ? 'bg-error text-on-error' : 'bg-secondary text-on-secondary'
          }`}
          title={mode === 'LIVE' ? 'Real orders on Kotak' : 'Simulated fills at the live price'}
        >
          {mode}
        </span>
        {feedPill(frame, streamStale)}
        {frame?.mode === 'LIVE' && (
          <Pill tone={frame.kotak_ready ? 'ok' : 'bad'} title="Kotak trading session">
            Kotak {frame.kotak_ready ? 'ready' : 'down'}
          </Pill>
        )}
      </div>

      <div className="flex items-center gap-5">
        <div className="leading-tight">
          <div className="text-[10px] font-bold uppercase tracking-wider text-on-surface-variant">Funds</div>
          <div className="font-mono-code text-sm font-bold tabular-nums text-on-surface">
            {funds}
            {asOf && <span className="hidden sm:inline ml-1.5 text-[10px] font-sans font-medium text-on-surface-variant">{asOf}</span>}
          </div>
        </div>
        <div className="leading-tight" title={`Realized ${fmtPnl(realized)} · open ${fmtPnl(open)}`}>
          <div className="text-[10px] font-bold uppercase tracking-wider text-on-surface-variant">Scalps today</div>
          <div className={`font-mono-code text-sm font-bold tabular-nums ${total >= 0 ? 'text-secondary' : 'text-error'}`}>
            {fmtPnl(total)}
            <span className="hidden sm:inline ml-1.5 text-[10px] font-sans font-medium text-on-surface-variant">
              {fmtPnl(realized)} booked · {fmtPnl(open)} open
            </span>
          </div>
        </div>
      </div>

      <HoldButton
        onFire={onExitAll}
        disabled={manualOpen === 0 || exitAllBusy}
        title={manualOpen === 0 ? 'No open scalps' : 'Press and hold to exit every manual scalp at market'}
        className="w-full sm:w-auto sm:ml-auto h-10 sm:h-9 px-4 rounded-lg bg-tertiary text-on-tertiary text-xs font-extrabold tracking-wider shadow-sm hover:bg-tertiary/90 disabled:bg-surface-container-high disabled:text-outline disabled:shadow-none transition-colors justify-center"
      >
        <Power size={14} />
        EXIT ALL SCALPS{manualOpen > 0 ? ` (${manualOpen})` : ''}
        <span className="hidden sm:inline text-[10px] font-semibold opacity-75">· hold</span>
      </HoldButton>
    </div>
  );
}

/** Slim banners for anything that stops a buy. Never fades the page. */
export function ScalpAlerts({ frame, streamStale }: { frame: ExecuteFrame | null; streamStale: boolean }) {
  const alerts: { tone: 'bad' | 'warn'; text: string }[] = [];
  if (frame?.kill_switch) alerts.push({ tone: 'bad', text: 'Kill switch engaged — buys are disabled.' });
  if (frame && frame.mode === 'LIVE' && !frame.kotak_ready) alerts.push({ tone: 'bad', text: 'Kotak is disconnected — reconnect it in Settings before trading.' });
  if (frame && !streamStale && !frame.market_open) alerts.push({ tone: 'warn', text: 'Market closed — manual buys run 09:15–15:30 IST.' });
  if (frame && !streamStale && frame.market_open) {
    const age = frame.feed.last_tick_age_ms;
    if (!frame.feed.connected || age == null || age > FEED_STALE_MS) {
      alerts.push({ tone: 'warn', text: 'No fresh prices from Kotak — buys are paused until the feed recovers.' });
    }
  }
  if (frame?.board_error) alerts.push({ tone: 'warn', text: frame.board_error });
  if (alerts.length === 0) return null;
  return (
    <div className="space-y-1.5">
      {alerts.map((a) => (
        <div
          key={a.text}
          className={`flex items-center gap-2 rounded-lg px-3 py-2 text-xs font-semibold border ${
            a.tone === 'bad' ? 'bg-error-container text-on-error-container border-error/30' : 'bg-amber-50 text-amber-900 border-amber-300'
          }`}
        >
          {a.tone === 'bad' ? <AlertOctagon size={14} className="shrink-0" /> : <TriangleAlert size={14} className="shrink-0" />}
          {a.text}
        </div>
      ))}
    </div>
  );
}
