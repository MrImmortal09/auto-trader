import { useMemo, useState } from 'react';
import { Info, XCircle } from 'lucide-react';
import { apiFetch } from '../lib/api';
import { computeAvgBuyPerUnit, formatExitReason, getRealizedPnl } from '../lib/format';
import { useEffect } from 'react';

export function ReportsPage({ serverBase }: { serverBase: string }) {
  const [trades, setTrades] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedSignal, setSelectedSignal] = useState<any>(null); // For the modal

  useEffect(() => {
    apiFetch(serverBase, '/api/portfolio')
      .then(res => res.json())
      .then(data => {
        setTrades(data.trades || []);
        setLoading(false);
      })
      .catch(() => setLoading(false));
  }, [serverBase]);

  // Group by date (YYYY-MM-DD)
  const groupedByDate: Record<string, any[]> = {};
  trades.forEach(t => {
    const date = t.timestamp.split(' ')[0]; // "2026-07-24"
    if (!groupedByDate[date]) groupedByDate[date] = [];
    groupedByDate[date].push(t);
  });

  // Sort dates descending
  const dates = Object.keys(groupedByDate).sort((a, b) => b.localeCompare(a));

  const avgBuyPerUnit = useMemo(() => computeAvgBuyPerUnit(trades), [trades]);

  return (
    <div className="p-6">
      <h2 className="text-xl font-bold mb-6 text-on-surface">Daily Reports & PnL</h2>
      {loading ? (
        <p className="text-on-surface-variant font-medium">Loading reports...</p>
      ) : dates.length === 0 ? (
        <p className="text-on-surface-variant font-medium">No trades recorded yet.</p>
      ) : (
        <div className="flex flex-col gap-8">
          {dates.map(date => {
            const dayTrades = groupedByDate[date];
            const dailyPnl = dayTrades.reduce((acc, t) => acc + getRealizedPnl(t, avgBuyPerUnit), 0);

            // Group by signal_id within the day
            const bySignal: Record<string, any[]> = {};
            dayTrades.forEach(t => {
              const sid = t.signal_id || 'legacy';
              if (!bySignal[sid]) bySignal[sid] = [];
              bySignal[sid].push(t);
            });

            return (
              <div key={date} className="bg-surface-container-lowest rounded-lg border border-outline-variant overflow-hidden shadow-sm">
                <div className="bg-surface-container-low px-4 py-3 border-b border-outline-variant flex justify-between items-center">
                  <h3 className="font-semibold text-lg text-on-surface">{date}</h3>
                  <div className={`font-mono-code font-bold ${dailyPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                    {dailyPnl >= 0 ? '+' : ''}₹{dailyPnl.toFixed(2)}
                  </div>
                </div>
                <div className="divide-y divide-outline-variant">
                  {Object.entries(bySignal).map(([sid, sigTrades]) => {
                    const isLegacy = sid === 'legacy';
                    const groupPnl = sigTrades.reduce((acc, t) => acc + getRealizedPnl(t, avgBuyPerUnit), 0);
                    // Use the ticker from the first trade
                    const ticker = sigTrades[0].ticker;
                    const rawMsg = sigTrades[0].raw_message;

                    return (
                      <div key={sid} className="px-4 py-3 flex items-center justify-between hover:bg-surface-container/30 transition-colors">
                        <div>
                          <div className="font-bold text-sm text-on-surface flex items-center gap-1.5">
                            {ticker}
                            {sigTrades.some((st) => st.mode === 'LIVE') && (
                              <span className="px-1.5 py-0.5 rounded text-[10px] font-bold bg-error-container text-on-error-container border border-error">
                                LIVE
                              </span>
                            )}
                          </div>
                          <div className="text-xs text-on-surface-variant mt-1">
                            {isLegacy ? (
                              <span className="text-tertiary font-medium">Legacy Trades (No context)</span>
                            ) : (
                              <span className="font-mono-code">Signal ID: {sid.slice(0,8)}...</span>
                            )}
                          </div>
                        </div>

                        <div className="flex items-center gap-4">
                          <div className="flex flex-col items-end">
                            <span className="text-label-caps text-on-surface-variant uppercase tracking-wider">Realized PnL</span>
                            <span className={`font-mono-code font-semibold text-sm ${groupPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                              {groupPnl >= 0 ? '+' : ''}₹{groupPnl.toFixed(2)}
                            </span>
                          </div>

                          {!isLegacy && (
                            <button
                              onClick={() => setSelectedSignal({ trades: sigTrades, message: rawMsg })}
                              className="p-1.5 rounded-full bg-surface-container text-primary hover:bg-primary hover:text-on-primary transition-colors"
                              title="View Trade Info"
                            >
                              <Info size={16} />
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Info Modal */}
      {selectedSignal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-inverse-surface/60 p-4">
          <div className="bg-surface-container-lowest border border-outline-variant rounded-lg shadow-xl max-w-lg w-full overflow-hidden flex flex-col text-on-surface">
            <div className="px-4 py-3 border-b border-outline-variant flex justify-between items-center bg-surface-container-low">
              <h3 className="font-semibold">Trade Details</h3>
              <button onClick={() => setSelectedSignal(null)} className="text-on-surface-variant hover:text-on-surface">
                <XCircle size={20} />
              </button>
            </div>
            <div className="p-4 overflow-y-auto max-h-[70vh]">
              <h4 className="text-label-caps font-semibold text-on-surface-variant uppercase mb-2">Original Message</h4>
              <div className="bg-surface-container-low p-3 rounded font-mono-code text-sm text-on-surface whitespace-pre-wrap mb-6 border border-outline-variant">
                {selectedSignal.message || 'No message recorded.'}
              </div>

              <h4 className="text-label-caps font-semibold text-on-surface-variant uppercase mb-2">Executions</h4>
              <div className="flex flex-col gap-2">
                {selectedSignal.trades.map((t: any) => {
                  const isSell = t.action?.toUpperCase() === 'SELL';
                  const delta = getRealizedPnl(t, avgBuyPerUnit);
                  return (
                    <div key={t.id} className="bg-surface-container-low/50 p-2.5 rounded flex justify-between items-center border border-outline-variant/60">
                      <div className="flex flex-col gap-1">
                        <div className="flex items-center gap-2">
                          <span className={`text-xs font-bold ${t.action === 'BUY' ? 'text-secondary' : 'text-tertiary'}`}>
                            {t.action} {t.qty} @ ₹{t.executed_price}
                          </span>
                          {t.exit_reason && t.exit_reason !== 'ENTRY' && (
                            <span className="px-1.5 py-0.5 rounded text-[10px] bg-surface-container text-on-surface-variant border border-outline-variant">
                              {formatExitReason(t.exit_reason)}
                            </span>
                          )}
                          {t.mode === 'LIVE' && (
                            <span className="px-1.5 py-0.5 rounded text-[10px] font-bold bg-error-container text-on-error-container border border-error">
                              LIVE
                            </span>
                          )}
                        </div>
                        <span className="text-[10px] text-on-surface-variant font-mono-code">{t.timestamp}</span>
                      </div>
                      {isSell ? (
                        <div className={`font-mono-code font-semibold text-sm ${delta >= 0 ? 'text-secondary' : 'text-error'}`}>
                          {delta >= 0 ? '+' : ''}₹{delta.toFixed(2)}
                        </div>
                      ) : (
                        <div className="font-mono-code text-sm text-on-surface-variant">—</div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
