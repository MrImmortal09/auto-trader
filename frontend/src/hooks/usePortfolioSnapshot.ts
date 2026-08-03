import { useEffect, useState } from 'react';
import type { MonitoredPosition, Portfolio } from '../types';
import { apiFetch } from '../lib/api';

// ---------------------------------------------------------------------------
// Shared portfolio data hook — single source of truth for balance, realized
// P&L and live MTM P&L so every screen that shows them agrees.
// ---------------------------------------------------------------------------

export function usePortfolioSnapshot(serverBase: string) {
  const [portfolio, setPortfolio] = useState<Portfolio | null>(null);
  const [positions, setPositions] = useState<MonitoredPosition[]>([]);

  useEffect(() => {
    async function load() {
      try {
        const [portfolioRes, positionsRes] = await Promise.all([
          apiFetch(serverBase, '/api/portfolio'),
          apiFetch(serverBase, '/api/positions'),
        ]);
        const [portfolioJson, positionsJson] = await Promise.all([
          portfolioRes.json(),
          positionsRes.json(),
        ]);
        setPortfolio(portfolioJson);
        setPositions(Array.isArray(positionsJson) ? positionsJson : []);
      } catch (e) {
        console.error(e);
      }
    }
    load();
    const id = setInterval(load, 5_000);
    return () => clearInterval(id);
  }, [serverBase]);

  const realizedPnl = portfolio
    ? portfolio.trades.reduce((acc, t) => acc + (t.action === 'BUY' ? -t.net_value : t.net_value), 0)
    : 0;
  const liveMtmPnl = positions
    .filter((p) => p.state === 'Active' || p.state === 'Target1Hit')
    .reduce((acc, p) => {
      if (p.signal.action !== 'BUY' || p.executed_qty <= 0 || p.ltp === undefined || p.ltp === null) {
        return acc;
      }
      return acc + (p.ltp - p.avg_buy_price) * p.executed_qty;
    }, 0);

  return { portfolio, positions, realizedPnl, liveMtmPnl };
}
