import { CheckCircle2, Info, Shield } from 'lucide-react';
import { usePortfolioSnapshot } from '../hooks/usePortfolioSnapshot';
import { fmt, formatExitReason, todayIsoIST } from '../lib/format';

export function TradeAnalyticsScreen({ serverBase }: { serverBase: string }) {
  const { portfolio, positions, realizedPnl, liveMtmPnl } = usePortfolioSnapshot(serverBase);
  const totalTradesToday = portfolio?.trades.filter((t) => t.timestamp.startsWith(todayIsoIST())).length ?? 0;
  const activePositions = positions.filter((p) => p.state === 'Active' || p.state === 'Target1Hit');
  const recentTrades = portfolio?.trades.slice(0, 8) ?? [];

  return (
    <div className="space-y-6">
      {/* Overview Stats */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <div className="flex items-center justify-between text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
            <span>Virtual Balance</span>
            <Info size={14} className="text-outline cursor-help" />
          </div>
          <span className="text-xl font-bold text-on-surface tabular-nums mt-1">₹{fmt(portfolio?.balance ?? 0)}</span>
        </div>
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <div className="flex items-center justify-between text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
            <span>Realised P&amp;L</span>
            <Info size={14} className="text-outline cursor-help" />
          </div>
          <span className={`text-xl font-bold tabular-nums mt-1 ${realizedPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
            {realizedPnl >= 0 ? '+' : ''}₹{fmt(realizedPnl)}
          </span>
        </div>
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <div className="flex items-center justify-between text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
            <span>Live MTM (LTP)</span>
            <Info size={14} className="text-outline cursor-help" />
          </div>
          <span className={`text-xl font-bold tabular-nums mt-1 ${liveMtmPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
            {liveMtmPnl >= 0 ? '+' : ''}₹{fmt(liveMtmPnl)}
          </span>
        </div>
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <div className="flex items-center justify-between text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
            <span>Total Trades Today</span>
            <Info size={14} className="text-outline cursor-help" />
          </div>
          <span className="text-xl font-bold text-on-surface tabular-nums mt-1">{totalTradesToday}</span>
        </div>
      </div>

      {/* Deep Dive Grid */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        <div className="lg:col-span-2 space-y-6">
          {activePositions.length === 0 ? (
            <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm p-8 text-center text-on-surface-variant text-sm">
              No active positions — risk parameters will appear here once a trade is live.
            </div>
          ) : activePositions.map((p) => {
            const hasLtp = p.ltp !== undefined && p.ltp !== null;
            const pnl = hasLtp ? (p.ltp! - p.avg_buy_price) * p.executed_qty : null;
            const pctChange = hasLtp && p.signal.entry_price ? ((p.ltp! - p.signal.entry_price) / p.signal.entry_price) * 100 : null;
            const risk = Math.abs(p.signal.entry_price - p.signal.stop_loss);
            const reward = p.signal.targets.length > 0 ? Math.abs(p.signal.targets[0] - p.signal.entry_price) : null;
            const rrRatio = reward !== null && risk > 0 ? reward / risk : null;
            return (
              <div key={p.id} className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm overflow-hidden">
                <div className="p-4 border-b border-outline-variant flex items-center justify-between bg-surface-container-low">
                  <div className="flex items-center gap-2">
                    <span className="font-bold text-on-surface text-sm">{p.signal.instrument_name}</span>
                    {p.ws_scrip_key && (
                      <span className="text-xs text-on-surface-variant bg-surface-container px-2 py-0.5 rounded font-mono">{p.ws_scrip_key}</span>
                    )}
                  </div>
                  {hasLtp && (
                    <div className="flex items-center gap-3">
                      <span className={`font-bold text-sm font-mono ${pctChange !== null && pctChange >= 0 ? 'text-secondary' : 'text-error'}`}>₹{fmt(p.ltp!)}</span>
                      {pctChange !== null && (
                        <span className={`text-xs font-semibold ${pctChange >= 0 ? 'text-secondary' : 'text-error'}`}>
                          {pctChange >= 0 ? '+' : ''}{pctChange.toFixed(2)}%
                        </span>
                      )}
                    </div>
                  )}
                </div>
                <div className="p-5">
                  <h3 className="text-xs font-semibold text-on-surface-variant uppercase tracking-wider mb-4 border-b border-outline-variant pb-2">Position Strategy &amp; Risk Parameters</h3>
                  <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
                    <div>
                      <span className="text-xs text-on-surface-variant block">Entry Price</span>
                      <span className="font-bold text-on-surface font-mono">₹{fmt(p.signal.entry_price)}</span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">Current LTP</span>
                      <span className="font-bold text-on-surface font-mono">{hasLtp ? `₹${fmt(p.ltp!)}` : '—'}</span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">Net Unrealised P&amp;L</span>
                      <span className={`font-bold font-mono ${pnl === null ? 'text-on-surface-variant' : pnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                        {pnl === null ? '—' : `${pnl >= 0 ? '+' : ''}₹${fmt(pnl)}`}
                      </span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">Quantity</span>
                      <span className="font-bold text-on-surface font-mono">{p.executed_qty}</span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">Risk / Reward Ratio</span>
                      <span className="font-bold text-primary font-mono">{rrRatio !== null ? `1 : ${rrRatio.toFixed(2)}` : '—'}</span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">State</span>
                      <span className="font-bold text-on-surface">{p.state}</span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">Stop Loss (SL)</span>
                      <span className="font-bold text-error font-mono">₹{fmt(p.current_sl)}</span>
                    </div>
                    <div>
                      <span className="text-xs text-on-surface-variant block">Targets</span>
                      <span className="font-bold text-secondary font-mono">{p.signal.targets.map((t) => `₹${fmt(t)}`).join(' / ')}</span>
                    </div>
                  </div>
                </div>
              </div>
            );
          })}
        </div>

        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-5 shadow-sm flex flex-col h-full max-h-[580px]">
          <h3 className="text-xs font-semibold text-on-surface-variant uppercase tracking-wider mb-4 border-b border-outline-variant pb-2">Recent Order Flow</h3>
          {recentTrades.length === 0 ? (
            <p className="text-xs text-on-surface-variant">No trades recorded yet.</p>
          ) : (
            <div className="flex-1 overflow-y-auto space-y-4 pr-1 text-sm">
              {recentTrades.map((t) => {
                const isBuy = t.action?.toUpperCase() === 'BUY';
                const reasonLabel = formatExitReason(t.exit_reason);
                return (
                  <div key={t.id} className="flex items-start gap-3">
                    {isBuy ? (
                      <CheckCircle2 size={16} className="text-secondary shrink-0 mt-0.5" />
                    ) : (
                      <Shield size={16} className="text-error shrink-0 mt-0.5" />
                    )}
                    <div>
                      <span className="font-bold text-on-surface block text-xs">{t.timestamp} — {t.action}</span>
                      <p className="text-xs text-on-surface-variant">
                        {t.ticker} {t.qty} qty @ ₹{fmt(t.executed_price)}{reasonLabel ? ` (${reasonLabel})` : ''}
                      </p>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
