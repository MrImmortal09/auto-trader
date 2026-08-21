// Known tradable indexes and their exchange-mandated lot size. Mirrors
// shared_domain::INDEX_NAMES (backend/shared_domain/src/lib.rs) — keep the
// symbol list in sync if that changes.
//
// Lot sizes are reference/display only, confirmed against Zerodha's lot-size
// page and NSE's Jan 2026 index-derivatives revision circular:
// https://support.zerodha.com/category/trading-and-markets/trading-faqs/f-otrading/articles/lot-size-for-index-derivatives
// Actual order quantity always uses the live lot size Kotak's scrip master
// resolves for the specific contract at signal time — these numbers are
// never sent to the broker, only shown so a "lots to buy" input is legible.
export interface IndexLotInfo {
  symbol: string;
  label: string;
  lotSize: number;
}

export const INDEX_LOT_REFERENCE: IndexLotInfo[] = [
  { symbol: 'NIFTY', label: 'Nifty 50', lotSize: 65 },
  { symbol: 'BANKNIFTY', label: 'Nifty Bank', lotSize: 30 },
  { symbol: 'FINNIFTY', label: 'Nifty Financial Services', lotSize: 60 },
  { symbol: 'MIDCPNIFTY', label: 'Nifty Midcap Select', lotSize: 120 },
  { symbol: 'SENSEX', label: 'BSE Sensex', lotSize: 20 },
  { symbol: 'BANKEX', label: 'BSE Bankex', lotSize: 30 },
];
