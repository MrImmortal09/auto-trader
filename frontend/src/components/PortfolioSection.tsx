import { useState } from 'react';
import { IndianRupee, Info, TrendingUp, Wallet } from 'lucide-react';
import { usePortfolioSnapshot } from '../hooks/usePortfolioSnapshot';
import { fmt, formatExitReason, totalCharges } from '../lib/format';
import { Stat } from './Stat';

export function PortfolioSection({ serverBase }: { serverBase: string }) {
  const { portfolio, positions, realizedPnl: latestPnl, liveMtmPnl } = usePortfolioSnapshot(serverBase);
  const [openTradeInfo, setOpenTradeInfo] = useState<number | null>(null);

  if (!portfolio) {
    return (
      <div className="flex-1 flex items-center justify-center text-on-surface-variant text-sm font-medium">
        Loading portfolio…
      </div>
    );
  }

  return (
    <div className="p-4 space-y-4">
      {/* Balance strip */}
      <div className="flex gap-4 flex-wrap">
        <Stat icon={<Wallet size={16} className="text-blue-400" />} label="Virtual Balance">
          ₹{fmt(portfolio.balance)}
        </Stat>
        <Stat
          icon={
            <TrendingUp size={16} className={latestPnl >= 0 ? 'text-emerald-400' : 'text-red-400'} />
          }
          label="Realised P&L"
        >
          <span className={latestPnl >= 0 ? 'text-emerald-400' : 'text-red-400'}>
            {latestPnl >= 0 ? '+' : ''}₹{fmt(latestPnl)}
          </span>
        </Stat>
        <Stat
          icon={
            <TrendingUp size={16} className={liveMtmPnl >= 0 ? 'text-emerald-300' : 'text-red-300'} />
          }
          label="Live MTM P&L (LTP)"
        >
          <span className={liveMtmPnl >= 0 ? 'text-emerald-300' : 'text-red-300'}>
            {liveMtmPnl >= 0 ? '+' : ''}₹{fmt(liveMtmPnl)}
          </span>
        </Stat>
        <Stat icon={<IndianRupee size={16} className="text-yellow-400" />} label="Total Trades">
          {portfolio.trades.length}
        </Stat>
      </div>

      {/* Trade history table */}
      <div className="bg-surface-container-lowest rounded-lg border border-outline-variant overflow-hidden shadow-sm">
        <div className="px-4 py-2.5 border-b border-outline-variant text-label-caps font-semibold text-on-surface-variant uppercase tracking-wider bg-surface-container-low">
          Trade History
        </div>
        {portfolio.trades.length === 0 ? (
          <div className="px-4 py-6 text-center text-on-surface-variant/70 text-sm">
            No trades yet — signals will appear here once the engine executes.
          </div>
        ) : (
          <>
            <div className="hidden md:block w-full">
              <table className="w-full text-sm">
                <thead>
                  <tr className="bg-surface-container-low text-label-caps font-semibold text-on-surface-variant border-b border-outline-variant">
                    {['Time', 'Ticker', 'Side', 'Qty', 'Price', 'Gross', 'Charges', 'Net'].map(
                      (h) => <th key={h} className="px-3 py-2.5 text-left">{h}</th>,
                    )}
                  </tr>
                </thead>
                <tbody className="text-body-sm font-body-sm">
                  {portfolio.trades.map((t) => {
                    const charges = totalCharges(t);
                    const linkedPos = positions.find((p) => p.signal.instrument_name === t.ticker);
                    return (
                      <tr key={t.id} className="border-b border-outline-variant hover:bg-surface-container/30 transition-colors">
                        <td className="px-3 py-2.5 text-on-surface-variant text-xs whitespace-nowrap font-mono-code">
                          {t.timestamp.substring(0, 16).replace('T', ' ')}
                        </td>
                        <td className="px-3 py-2.5 font-bold text-on-surface">
                          <div className="inline-flex items-center gap-1.5">
                            <span>{t.ticker}</span>
                            {t.mode === 'LIVE' && (
                              <span className="px-1.5 py-0.5 rounded text-[10px] font-bold bg-error-container text-on-error-container border border-error">
                                LIVE
                              </span>
                            )}
                            <button
                              onClick={() => setOpenTradeInfo(openTradeInfo === t.id ? null : t.id)}
                              className="text-on-surface-variant hover:text-primary transition-colors"
                            >
                              <Info size={14} />
                            </button>
                          </div>
                          {openTradeInfo === t.id && (
                            <>
                              <div className="fixed inset-0 z-[190]" onClick={() => setOpenTradeInfo(null)} />
                              <div className="fixed right-6 top-24 z-[200] bg-inverse-surface border border-outline-variant p-3 rounded-lg shadow-2xl w-[min(92vw,680px)] max-h-[70vh] overflow-auto text-[11px] font-mono-code text-inverse-on-surface whitespace-pre-wrap break-words">
                                {JSON.stringify({
                                  trade: t,
                                  signal_targets_sl: linkedPos ? linkedPos.signal : null,
                                  live_position: linkedPos
                                    ? {
                                        state: linkedPos.state,
                                        current_sl: linkedPos.current_sl,
                                        ltp: linkedPos.ltp,
                                        executed_qty: linkedPos.executed_qty,
                                        avg_buy_price: linkedPos.avg_buy_price,
                                      }
                                    : null,
                                }, null, 2)}
                              </div>
                            </>
                          )}
                        </td>
                        <td className="px-3 py-2.5 flex items-center gap-1.5">
                          <span className={`px-2 py-0.5 rounded text-xs font-label-caps font-bold uppercase ${
                            t.action === 'BUY' ? 'bg-[#d1fae5] text-[#065f46]' : 'bg-[#ffe4e6] text-[#9f1239]'
                          }`}>{t.action}</span>
                          {t.exit_reason && t.exit_reason !== 'ENTRY' && (
                            <span className="px-1.5 py-0.5 rounded text-[10px] bg-surface-container text-on-surface-variant border border-outline-variant whitespace-nowrap">
                              {formatExitReason(t.exit_reason)}
                            </span>
                          )}
                        </td>
                        <td className="px-3 py-2.5 text-on-surface font-mono-code">{t.qty}</td>
                        <td className="px-3 py-2.5 text-on-surface font-mono-code">₹{fmt(t.executed_price)}</td>
                        <td className="px-3 py-2.5 text-on-surface font-mono-code">₹{fmt(t.gross_value)}</td>
                        <td className="px-3 py-2.5 text-on-surface-variant text-xs font-mono-code">₹{fmt(charges)}</td>
                        <td className="px-3 py-2.5 font-bold text-on-surface font-mono-code">₹{fmt(t.net_value)}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>

            {/* iPhone 17 Mobile Card View (Trade History) */}
            <div className="md:hidden p-3 space-y-2.5">
              {portfolio.trades.map((t) => {
                const charges = totalCharges(t);
                return (
                  <div key={t.id} className="bg-surface rounded-xl border border-outline-variant p-3 shadow-sm space-y-2">
                    <div className="flex items-center justify-between">
                      <div className="font-bold text-on-surface text-sm flex items-center gap-1.5">
                        <span>{t.ticker}</span>
                        {t.mode === 'LIVE' && (
                          <span className="px-1.5 py-0.5 rounded text-[10px] font-bold bg-error-container text-on-error-container border border-error">
                            LIVE
                          </span>
                        )}
                      </div>
                      <div className="flex items-center gap-1.5">
                        <span className={`px-2 py-0.5 rounded text-xs font-label-caps font-bold uppercase ${
                          t.action === 'BUY' ? 'bg-[#d1fae5] text-[#065f46]' : 'bg-[#ffe4e6] text-[#9f1239]'
                        }`}>{t.action}</span>
                        {t.exit_reason && t.exit_reason !== 'ENTRY' && (
                          <span className="px-1.5 py-0.5 rounded text-[10px] bg-surface-container text-on-surface-variant border border-outline-variant">
                            {formatExitReason(t.exit_reason)}
                          </span>
                        )}
                      </div>
                    </div>

                    <div className="text-[11px] text-on-surface-variant font-mono-code">
                      {t.timestamp.substring(0, 16).replace('T', ' ')}
                    </div>

                    <div className="grid grid-cols-3 gap-2 text-xs font-mono-code bg-surface-container-low p-2 rounded-lg border border-outline-variant/50">
                      <div>
                        <div className="text-on-surface-variant text-[10px] uppercase font-sans">Qty</div>
                        <div className="font-semibold text-on-surface">{t.qty}</div>
                      </div>
                      <div>
                        <div className="text-on-surface-variant text-[10px] uppercase font-sans">Price</div>
                        <div className="font-semibold text-on-surface">₹{fmt(t.executed_price)}</div>
                      </div>
                      <div>
                        <div className="text-on-surface-variant text-[10px] uppercase font-sans">Charges</div>
                        <div className="font-semibold text-on-surface-variant">₹{fmt(charges)}</div>
                      </div>
                    </div>

                    <div className="flex items-center justify-between pt-1 border-t border-outline-variant/30">
                      <span className="text-xs font-semibold text-on-surface-variant">Net Value:</span>
                      <span className="text-sm font-bold font-mono-code text-on-surface">₹{fmt(t.net_value)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
