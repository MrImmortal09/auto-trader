import { useEffect, useMemo, useState } from 'react';
import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import type { PaperTrade, Portfolio } from '../types';
import { apiFetch } from '../lib/api';
import { computeAvgBuyPerUnit, fmt, fmtPct, getRealizedPnl } from '../lib/format';
import { usePortfolioSnapshot } from '../hooks/usePortfolioSnapshot';
import { ReportsPage } from '../components/ReportsPage';

const RANGE_DAYS: Record<'1D' | '1W' | '1M' | '3M' | 'ALL', number> = {
  '1D': 1, '1W': 7, '1M': 30, '3M': 90, 'ALL': Infinity,
};

export function PortfolioPerformanceScreen({ serverBase }: { serverBase: string }) {
  const [range, setRange] = useState<'1D' | '1W' | '1M' | '3M' | 'ALL'>('1M');
  const { positions } = usePortfolioSnapshot(serverBase);
  const [trades, setTrades] = useState<PaperTrade[]>([]);

  useEffect(() => {
    function load() {
      apiFetch(serverBase, '/api/portfolio')
        .then((r) => r.json())
        .then((data: Portfolio) => setTrades(data.trades ?? []))
        .catch(console.error);
    }
    load();
    const id = setInterval(load, 5_000);
    return () => clearInterval(id);
  }, [serverBase]);

  // Trades come back newest-first from the API; walk oldest-first to build
  // a cumulative realized equity curve.
  const chronological = useMemo(() => [...trades].reverse(), [trades]);
  const avgBuyPerUnit = useMemo(() => computeAvgBuyPerUnit(chronological), [chronological]);

  const equityCurve = useMemo(() => (
    chronological.reduce<{ timestamp: string; pnl: number }[]>((acc, t) => {
      const priorPnl = acc.length > 0 ? acc[acc.length - 1].pnl : 0;
      acc.push({ timestamp: t.timestamp, pnl: priorPnl + getRealizedPnl(t, avgBuyPerUnit) });
      return acc;
    }, [])
  ), [chronological, avgBuyPerUnit]);

  const totalRealizedPnl = equityCurve.length > 0 ? equityCurve[equityCurve.length - 1].pnl : 0;

  const closedLegPnls = chronological
    .map((t) => getRealizedPnl(t, avgBuyPerUnit))
    .filter((pnl) => pnl !== 0);
  const wins = closedLegPnls.filter((p) => p > 0);
  const losses = closedLegPnls.filter((p) => p < 0);
  const winRate = closedLegPnls.length > 0 ? (wins.length / closedLegPnls.length) * 100 : 0;
  const grossProfit = wins.reduce((a, b) => a + b, 0);
  const grossLoss = Math.abs(losses.reduce((a, b) => a + b, 0));
  const profitFactor = grossLoss > 0 ? grossProfit / grossLoss : null;

  const maxDrawdown = useMemo(() => {
    let peak = 0;
    let worst = 0;
    for (const point of equityCurve) {
      if (point.pnl > peak) peak = point.pnl;
      worst = Math.max(worst, peak - point.pnl);
    }
    return worst;
  }, [equityCurve]);

  const nowIST = new Date(new Date().toLocaleString('en-US', { timeZone: 'Asia/Kolkata' }));
  const rangeDays = RANGE_DAYS[range];
  const cutoff = rangeDays === Infinity ? -Infinity : nowIST.getTime() - rangeDays * 86_400_000;
  const chartData = equityCurve
    .filter((p) => new Date(p.timestamp.replace(' ', 'T')).getTime() >= cutoff)
    .map((p) => ({ ...p, label: p.timestamp.slice(5, 16) }));

  const perInstrument = useMemo(() => {
    const map: Record<string, { trades: number; wins: number; grossProfit: number; grossLoss: number; netPnl: number }> = {};
    chronological.forEach((t) => {
      const pnl = getRealizedPnl(t, avgBuyPerUnit);
      if (pnl === 0) return;
      if (!map[t.ticker]) map[t.ticker] = { trades: 0, wins: 0, grossProfit: 0, grossLoss: 0, netPnl: 0 };
      const m = map[t.ticker];
      m.trades += 1;
      m.netPnl += pnl;
      if (pnl > 0) { m.wins += 1; m.grossProfit += pnl; }
      else { m.grossLoss += Math.abs(pnl); }
    });
    return Object.entries(map)
      .map(([ticker, m]) => ({
        ticker,
        trades: m.trades,
        winRate: m.trades > 0 ? (m.wins / m.trades) * 100 : 0,
        netPnl: m.netPnl,
        profitFactor: m.grossLoss > 0 ? m.grossProfit / m.grossLoss : null,
        active: positions.some((p) => p.signal.instrument_name === ticker
          && (p.state === 'Active' || p.state === 'Target1Hit' || p.state === 'WaitingForEntry')),
      }))
      .sort((a, b) => b.netPnl - a.netPnl);
  }, [chronological, avgBuyPerUnit, positions]);

  return (
    <div className="space-y-8">
      {/* Top KPI Cards */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <span className="text-xs text-on-surface-variant font-semibold uppercase tracking-wider">Total Realized P&amp;L</span>
          <span className={`text-xl font-bold tabular-nums mt-1 ${totalRealizedPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
            {totalRealizedPnl >= 0 ? '+' : ''}₹{fmt(totalRealizedPnl)}
          </span>
        </div>
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <span className="text-xs text-on-surface-variant font-semibold uppercase tracking-wider">Win Rate %</span>
          <span className="text-xl font-bold text-on-surface tabular-nums mt-1">{closedLegPnls.length > 0 ? fmtPct(winRate) : '—'}</span>
        </div>
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <span className="text-xs text-on-surface-variant font-semibold uppercase tracking-wider">Profit Factor</span>
          <span className="text-xl font-bold text-primary tabular-nums mt-1">{profitFactor !== null ? profitFactor.toFixed(2) : '—'}</span>
        </div>
        <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-4 shadow-sm flex flex-col justify-between">
          <span className="text-xs text-on-surface-variant font-semibold uppercase tracking-wider">Max Drawdown</span>
          <span className="text-xl font-bold text-error tabular-nums mt-1">{maxDrawdown > 0 ? `-₹${fmt(maxDrawdown)}` : '₹0.00'}</span>
        </div>
      </div>

      {/* Equity Growth & Drawdown Chart */}
      <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm p-6">
        <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4 mb-6">
          <div>
            <h3 className="text-base font-bold text-on-surface">Cumulative Realized Returns</h3>
            <p className="text-xs text-on-surface-variant">Running realized P&amp;L across all closed trade legs</p>
          </div>
          <div className="flex items-center gap-1 bg-surface-container p-1 rounded-lg">
            {(['1D', '1W', '1M', '3M', 'ALL'] as const).map((r) => (
              <button
                key={r}
                onClick={() => setRange(r)}
                className={`px-3 py-1 rounded-md text-xs font-semibold transition-all ${
                  range === r
                    ? 'bg-primary-container text-on-primary shadow-sm'
                    : 'text-on-surface-variant hover:text-on-surface'
                }`}
              >
                {r}
              </button>
            ))}
          </div>
        </div>
        {chartData.length === 0 ? (
          <div className="h-64 rounded-lg bg-surface-container-low/50 border border-outline-variant flex items-center justify-center p-6 text-center">
            <span className="text-on-surface-variant text-xs">No closed trades in this range yet.</span>
          </div>
        ) : (
          <div className="h-64">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={chartData} margin={{ top: 8, right: 8, left: 0, bottom: 0 }}>
                <defs>
                  <linearGradient id="equityFill" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor="currentColor" stopOpacity={0.35} className="text-primary" />
                    <stop offset="100%" stopColor="currentColor" stopOpacity={0} className="text-primary" />
                  </linearGradient>
                </defs>
                <CartesianGrid strokeDasharray="3 3" className="stroke-outline-variant" />
                <XAxis dataKey="label" tick={{ fontSize: 10 }} minTickGap={24} />
                <YAxis tick={{ fontSize: 10 }} width={70} tickFormatter={(v: number) => `₹${fmt(v)}`} />
                <Tooltip
                  formatter={(v) => [`₹${fmt(Number(v))}`, 'Cumulative P&L']}
                  labelFormatter={(label) => label}
                />
                <Area type="monotone" dataKey="pnl" stroke="currentColor" className="text-primary" fill="url(#equityFill)" strokeWidth={2} />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        )}
      </div>

      {/* Per-Instrument Performance Breakdown */}
      <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm overflow-hidden">
        <div className="px-6 py-4 border-b border-outline-variant bg-surface-container-low">
          <h3 className="text-xs font-semibold text-on-surface-variant uppercase tracking-wider">Per-Instrument Performance Breakdown</h3>
        </div>
        {perInstrument.length === 0 ? (
          <p className="text-xs text-on-surface-variant p-6">No closed trades yet.</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left border-collapse text-sm">
              <thead>
                <tr className="border-b border-outline-variant bg-surface-container-low/50 text-xs font-semibold text-on-surface-variant uppercase tracking-wider">
                  <th className="px-6 py-3">Instrument</th>
                  <th className="px-6 py-3 text-right">Closed Legs</th>
                  <th className="px-6 py-3 text-right">Win Rate</th>
                  <th className="px-6 py-3 text-right">Net P&amp;L</th>
                  <th className="px-6 py-3 text-right">Profit Factor</th>
                  <th className="px-6 py-3 text-center">Status</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-outline-variant text-on-surface tabular-nums">
                {perInstrument.map((row) => (
                  <tr key={row.ticker} className="hover:bg-surface-container/30 transition-colors">
                    <td className="px-6 py-3 font-semibold">{row.ticker}</td>
                    <td className="px-6 py-3 text-right">{row.trades}</td>
                    <td className="px-6 py-3 text-right">{fmtPct(row.winRate)}</td>
                    <td className={`px-6 py-3 text-right font-bold ${row.netPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                      {row.netPnl >= 0 ? '+' : ''}₹{fmt(row.netPnl)}
                    </td>
                    <td className="px-6 py-3 text-right font-mono">{row.profitFactor !== null ? row.profitFactor.toFixed(2) : '—'}</td>
                    <td className="px-6 py-3 text-center">
                      <span className={`px-2 py-0.5 rounded text-xs font-semibold uppercase ${
                        row.active ? 'bg-[#d1fae5] text-[#065f46]' : 'bg-surface-container text-on-surface-variant'
                      }`}>{row.active ? 'Active' : 'Closed'}</span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* Embedded Daily Reports & Export */}
      <div>
        <h3 className="text-base font-bold text-on-surface mb-4">Daily P&amp;L Statements &amp; Reports</h3>
        <ReportsPage serverBase={serverBase} />
      </div>
    </div>
  );
}
