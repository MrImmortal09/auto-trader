// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

import type { PaperTrade } from '../types';

export function fmt(n: number) {
  return new Intl.NumberFormat('en-IN', {
    maximumFractionDigits: 2,
    minimumFractionDigits: 2,
  }).format(n);
}

export function totalCharges(t: PaperTrade) {
  return t.brokerage + t.stt_charge + t.sebi_fee +
    t.stamp_duty + t.transaction_charge + t.gst;
}

/** Label for the balance stat tile, matching what `balance_source` actually is. */
export function balanceLabel(source?: string): string {
  if (source === 'LIVE') return 'Wallet Balance';
  if (source === 'LIVE_UNAVAILABLE') return 'Wallet Balance (unavailable)';
  return 'Virtual Balance';
}

export function formatExitReason(reason?: string): string {
  if (!reason || reason === 'ENTRY') return '';
  switch (reason) {
    case 'SL_HIT': return 'SL Hit';
    case 'TRAILED_SL_HIT':
    case 'TRAIL_SL_HIT': return 'Trailed SL Hit';
    case 'CLOSED_VIA_FRONTEND': return 'Closed via Frontend';
    case 'TELEGRAM_EXIT': return 'Exit via Telegram Msg';
    case 'OPPOSITE_SIGNAL_EXIT': return 'Opposite Signal Exit';
    case 'TGT1_FULL':
    case 'TGT1_PARTIAL':
    case 'TGT1_HIT': return 'Target 1 Hit';
    case 'TGT2_HIT': return 'Target 2 Hit';
    case 'EXPIRY_SQUAREOFF': return 'Expiry Square-off';
    default:
      if (reason.startsWith('EXIT_AT_')) {
        const price = reason.replace('EXIT_AT_', '');
        return price ? `Exit via Telegram (@ ₹${price})` : 'Exit via Telegram Msg';
      }
      return reason.replace(/_/g, ' ');
  }
}

export function fmtPct(n: number) {
  return `${n.toFixed(1)}%`;
}

// Trades are timestamped in IST server-side (see current_ist_timestamp_string),
// so "today" must be computed in IST regardless of the browser's timezone.
export function todayIsoIST(): string {
  const d = new Date(new Date().toLocaleString('en-US', { timeZone: 'Asia/Kolkata' }));
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

// Average BUY cost per unit, keyed by `${signal_id}|${ticker}`, computed
// across ALL trades (a position may be opened/closed on different days).
// net_value on a BUY leg already includes fees, so this is the true per-unit
// cost basis.
export function computeAvgBuyPerUnit(trades: PaperTrade[]): Record<string, number> {
  const buyQty: Record<string, number> = {};
  const buyNet: Record<string, number> = {};
  trades.forEach((t) => {
    if (t.action?.toUpperCase() !== 'BUY') return;
    const key = `${t.signal_id || 'legacy'}|${t.ticker}`;
    buyQty[key] = (buyQty[key] || 0) + (t.qty || 0);
    buyNet[key] = (buyNet[key] || 0) + (t.net_value || 0);
  });
  const avg: Record<string, number> = {};
  Object.keys(buyQty).forEach((k) => {
    avg[k] = buyQty[k] > 0 ? buyNet[k] / buyQty[k] : 0;
  });
  return avg;
}

// Realized PnL for a single trade leg.
//  - BUY  → 0 (opening/adding to a position realizes nothing).
//  - SELL → net proceeds − cost basis for the quantity sold (avg buy cost).
export function getRealizedPnl(t: PaperTrade, avgBuyPerUnit: Record<string, number>): number {
  if (t.action?.toUpperCase() !== 'SELL') return 0;
  const key = `${t.signal_id || 'legacy'}|${t.ticker}`;
  const costBasis = (t.qty || 0) * (avgBuyPerUnit[key] || 0);
  return (t.net_value || 0) - costBasis;
}

export function fmtUptime(totalSecs: number) {
  const days = Math.floor(totalSecs / 86_400);
  const hours = Math.floor((totalSecs % 86_400) / 3_600);
  const mins = Math.floor((totalSecs % 3_600) / 60);
  if (days > 0) return `${days}d ${hours}h ${mins}m`;
  if (hours > 0) return `${hours}h ${mins}m`;
  return `${mins}m`;
}
