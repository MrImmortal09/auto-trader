import { useEffect, useState } from 'react';
import { Info } from 'lucide-react';
import type { MonitoredPosition } from '../types';
import { apiFetch } from '../lib/api';
import { fmt } from '../lib/format';
import { QtyInput } from './QtyInput';

export function UpcomingTrades({ serverBase }: { serverBase: string }) {
  const [positions, setPositions] = useState<MonitoredPosition[]>([]);
  const [openTooltip, setOpenTooltip] = useState<string | null>(null);
  const [closingId, setClosingId] = useState<string | null>(null);

  useEffect(() => {
    function load() {
      apiFetch(serverBase, '/api/positions')
        .then(r => r.json())
        .then(setPositions)
        .catch(console.error);
    }
    load();
    const id = setInterval(load, 3000);
    return () => clearInterval(id);
  }, [serverBase]);

  async function cancelTrade(id: string) {
    try {
      const res = await apiFetch(serverBase, `/api/positions/${id}`, { method: 'DELETE' });
      if (!res.ok) {
        // In LIVE the server refuses to forget a position that is still open at
        // the broker — leave the row in place so it stays visible.
        const err = await res.json().catch(() => null);
        console.error(err?.error ?? 'Failed to cancel trade');
        return;
      }
      setPositions(prev => prev.filter(p => p.id !== id));
    } catch (e) {
      console.error(e);
    }
  }

  async function updateQty(id: string, qty: number | null) {
    try {
      await apiFetch(serverBase, `/api/positions/${id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ override_qty: qty }),
      });
      setPositions(prev => prev.map(p => p.id === id ? { ...p, override_qty: qty } : p));
    } catch (e) {
      console.error(e);
    }
  }

  async function closeOngoingTrade(id: string) {
    try {
      setClosingId(id);
      const res = await apiFetch(serverBase, `/api/positions/${id}/close`, { method: 'POST' });
      if (!res.ok) {
        const err = await res.json().catch(() => null);
        console.error(err?.error ?? 'Failed to close position');
        return;
      }
      // 202 means LIVE: the exit was handed to the engine and is not filled yet.
      // Keep the row until the poll shows the broker actually sold.
      if (res.status === 202) return;
      setPositions((prev) => prev.filter((p) => p.id !== id));
    } catch (e) {
      console.error(e);
    } finally {
      setClosingId(null);
    }
  }

  const waiting = positions.filter((p) => p.state === 'WaitingForEntry');
  const active = positions.filter((p) => p.state === 'Active' || p.state === 'Target1Hit');

  if (waiting.length === 0 && active.length === 0) return null;

  return (
    <div className="bg-surface-container-lowest border-b border-outline-variant shrink-0">
      {waiting.length > 0 && (
        <>
          <div className="px-4 py-2.5 border-b border-outline-variant text-label-caps font-semibold text-on-surface-variant uppercase tracking-wider bg-surface-container-low relative z-10">
            Upcoming Trades (Awaiting Entry)
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="bg-surface-container-low text-label-caps font-semibold text-on-surface-variant border-b border-outline-variant">
                  <th className="px-3 py-2.5 text-left sticky top-0 bg-surface-container-low">Instrument</th>
                  <th className="px-3 py-2.5 text-left sticky top-0 bg-surface-container-low">Action</th>
                  <th className="px-3 py-2.5 text-left sticky top-0 bg-surface-container-low">Trigger</th>
                  <th className="px-3 py-2.5 text-left sticky top-0 bg-surface-container-low">SL</th>
                  <th className="px-3 py-2.5 text-left sticky top-0 bg-surface-container-low">Qty (Override)</th>
                  <th className="px-3 py-2.5 text-right sticky top-0 bg-surface-container-low">Controls</th>
                </tr>
              </thead>
              <tbody className="text-body-sm font-body-sm">
                {waiting.map((p) => (
                  <tr key={p.id} className="border-b border-outline-variant hover:bg-surface-container/30 transition-colors">
                    <td className="px-3 py-2.5 font-bold text-on-surface relative">
                      <div className={`flex items-center gap-1.5 relative group/tooltip ${openTooltip === p.id ? 'z-[60]' : 'hover:z-[60]'}`}>
                        <div className="flex flex-col">
                          <span>{p.signal.instrument_name}</span>
                          {p.ltp !== undefined && p.ltp !== null && (
                            <span className="text-[10px] text-on-surface-variant font-normal">LTP: <span className="text-primary font-mono-code font-semibold">₹{fmt(p.ltp)}</span></span>
                          )}
                        </div>
                        <button
                          onClick={(e) => { e.stopPropagation(); setOpenTooltip(openTooltip === p.id ? null : p.id); }}
                          className="text-on-surface-variant hover:text-primary focus:outline-none cursor-pointer"
                        >
                          <Info size={14} />
                        </button>

                        {openTooltip === p.id && (
                          <div
                            className="fixed inset-0 z-40 cursor-default"
                            onClick={(e) => { e.stopPropagation(); setOpenTooltip(null); }}
                          />
                        )}

                        <div className={`absolute left-full ml-2 top-1/2 -translate-y-1/2 transition-opacity bg-inverse-surface border border-outline-variant p-3 rounded-lg shadow-2xl z-[100] min-w-max text-[11px] font-mono-code text-inverse-on-surface whitespace-pre ${
                          openTooltip === p.id
                            ? 'opacity-100 pointer-events-auto'
                            : 'opacity-0 group-hover/tooltip:opacity-100 pointer-events-none group-hover/tooltip:pointer-events-auto'
                        }`}>
                          <div className="text-secondary font-bold mb-1 border-b border-outline-variant/30 pb-1">Signal + Targets/SL</div>
                          {JSON.stringify(p.signal, null, 2)}

                          {p.resolved_order && (
                            <>
                              <div className="text-primary font-bold mt-2 mb-1 border-b border-outline-variant/30 pb-1">Resolved Order (Kotak API)</div>
                              {JSON.stringify(p.resolved_order, null, 2)}
                            </>
                          )}
                        </div>
                      </div>
                    </td>
                    <td className="px-3 py-2.5">
                      <span className={`px-2 py-0.5 rounded text-xs font-label-caps font-bold uppercase ${
                        p.signal.action === 'BUY' ? 'bg-[#d1fae5] text-[#065f46]' : 'bg-[#ffe4e6] text-[#9f1239]'
                      }`}>{p.signal.action}</span>
                    </td>
                    <td className="px-3 py-2.5 text-on-surface font-mono-code">
                      {p.signal.entry_condition} ₹{fmt(p.signal.entry_price)}
                    </td>
                    <td className="px-3 py-2.5 text-error font-mono-code font-semibold">₹{fmt(p.signal.stop_loss)}</td>
                    <td className="px-3 py-2.5">
                      <QtyInput initialQty={p.override_qty} id={p.id} defaultQty={p.resolved_order?.quantity} onUpdate={updateQty} />
                    </td>
                    <td className="px-3 py-2.5 text-right">
                      <button onClick={() => cancelTrade(p.id)} className="px-2.5 py-1 bg-error-container hover:bg-error text-on-error-container hover:text-on-error rounded text-xs transition-colors font-medium">
                        Cancel
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {active.length > 0 && (
        <>
          <div className="px-4 py-2.5 border-y border-outline-variant text-label-caps font-semibold text-on-surface-variant uppercase tracking-wider bg-surface-container-low">
            Active Positions (Live LTP + MTM)
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="bg-surface-container-low text-label-caps font-semibold text-on-surface-variant border-b border-outline-variant">
                  <th className="px-3 py-2.5 text-left">Instrument</th>
                  <th className="px-3 py-2.5 text-left">State</th>
                  <th className="px-3 py-2.5 text-left">Qty</th>
                  <th className="px-3 py-2.5 text-left">Entry LTP</th>
                  <th className="px-3 py-2.5 text-left">Current LTP</th>
                  <th className="px-3 py-2.5 text-left">SL</th>
                  <th className="px-3 py-2.5 text-left">Targets</th>
                  <th className="px-3 py-2.5 text-right">Unrealized P&L</th>
                  <th className="px-3 py-2.5 text-right">Controls</th>
                </tr>
              </thead>
              <tbody className="text-body-sm font-body-sm">
                {active.map((p) => {
                  const hasLtp = p.ltp !== undefined && p.ltp !== null;
                  const pnl = hasLtp ? (p.ltp! - p.avg_buy_price) * p.executed_qty : null;
                  return (
                    <tr key={p.id} className="border-b border-outline-variant hover:bg-surface-container/30 transition-colors">
                      <td className="px-3 py-2.5 font-bold text-on-surface relative">
                        <div className={`flex items-center gap-1.5 relative ${openTooltip === p.id ? 'z-[60]' : 'hover:z-[60]'}`}>
                          <div className="flex flex-col">
                            <span>{p.signal.instrument_name}</span>
                            {p.ws_scrip_key && <span className="text-[10px] text-on-surface-variant font-normal">{p.ws_scrip_key}</span>}
                          </div>
                          <button
                            onClick={(e) => { e.stopPropagation(); setOpenTooltip(openTooltip === p.id ? null : p.id); }}
                            className="text-on-surface-variant hover:text-primary focus:outline-none cursor-pointer"
                          >
                            <Info size={14} />
                          </button>

                          {openTooltip === p.id && (
                            <div
                              className="fixed inset-0 z-40 cursor-default"
                              onClick={(e) => { e.stopPropagation(); setOpenTooltip(null); }}
                            />
                          )}

                          {openTooltip === p.id && (
                            <div className="fixed inset-0 z-[110] flex items-start justify-center pt-24 px-4 pointer-events-none">
                              <div className="bg-inverse-surface border border-outline-variant p-3 rounded-lg shadow-2xl text-[11px] font-mono-code text-inverse-on-surface whitespace-pre max-w-[90vw] max-h-[70vh] overflow-auto pointer-events-auto">
                                <div className="text-secondary font-bold mb-1 border-b border-outline-variant/30 pb-1">Order Details + Signal</div>
                                {JSON.stringify({
                                  state: p.state,
                                  executed_qty: p.executed_qty,
                                  avg_buy_price: p.avg_buy_price,
                                  current_sl: p.current_sl,
                                  ltp: p.ltp,
                                  signal: p.signal,
                                  resolved_order: p.resolved_order,
                                }, null, 2)}
                              </div>
                            </div>
                          )}
                        </div>
                      </td>
                      <td className="px-3 py-2.5 text-surface-tint font-semibold">{p.state}</td>
                      <td className="px-3 py-2.5 text-on-surface font-mono-code">{p.executed_qty}</td>
                      <td className="px-3 py-2.5 text-on-surface font-mono-code">₹{fmt(p.avg_buy_price)}</td>
                      <td className="px-3 py-2.5 text-on-surface font-mono-code font-semibold">{hasLtp ? `₹${fmt(p.ltp!)}` : '—'}</td>
                      <td className="px-3 py-2.5 text-error font-mono-code font-semibold">₹{fmt(p.current_sl)}</td>
                      <td className="px-3 py-2.5 text-primary font-mono-code">{p.signal.targets.map((t) => `₹${fmt(t)}`).join(' / ')}</td>
                      <td className="px-3 py-2.5 text-right font-mono-code">
                        {pnl === null ? (
                          <span className="text-on-surface-variant">—</span>
                        ) : (
                          <span className={`font-bold ${pnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                            {pnl >= 0 ? '+' : ''}₹{fmt(pnl)}
                          </span>
                        )}
                      </td>
                      <td className="px-3 py-2.5 text-right">
                        <button
                          onClick={() => closeOngoingTrade(p.id)}
                          disabled={closingId === p.id}
                          className="px-2.5 py-1 bg-tertiary-container hover:bg-tertiary text-on-tertiary-container hover:text-on-tertiary rounded text-xs transition-colors font-medium disabled:opacity-50"
                        >
                          {closingId === p.id ? 'Closing…' : 'Close'}
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  );
}
