//! Kotak's order book for v2's Orders tab (`GET /api/v2/orders`): orders
//! still working first, then the rest with what they filled at — newest first
//! in each.
//!
//! Display only. The engine reads the book itself when it settles an order;
//! this never feeds it. Kotak is read only while the tab asks, at most once
//! every [`FRESH_FOR`] whoever asks, and the client is cloned out of its
//! mutex so a slow read never holds an order up behind it (see `balance`).

use std::time::{Duration, Instant};

use axum::{extract::State, response::IntoResponse, Json};
use kotak_client::KotakOrder;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::AppState;

/// How long one read of the book answers every caller.
const FRESH_FOR: Duration = Duration::from_secs(4);

/// One order, as the tab shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderRow {
    pub id: String,
    pub symbol: String,
    /// "NIFTY 25100 CE", when the scrip master knows the symbol.
    pub contract: Option<String>,
    /// `BUY` / `SELL`.
    pub side: &'static str,
    /// `MKT`, `L`, `SL`, `SL-M`.
    pub order_type: String,
    pub qty: i32,
    pub filled: i32,
    /// The average fill price, once anything filled.
    pub avg: Option<f64>,
    /// The limit price asked, for a limit order.
    pub price: Option<f64>,
    /// The stop trigger, for a stop order.
    pub trigger: Option<f64>,
    /// Kotak's word for it: `complete`, `rejected`, `open`, …
    pub status: String,
    /// Still working at Kotak: not complete, rejected or cancelled.
    pub open: bool,
    pub reason: Option<String>,
    /// IST `HH:MM:SS` it was placed (or last updated), when Kotak says.
    pub time: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OrdersAnswer {
    /// IST `HH:MM:SS` of the read these orders are from.
    pub fetched_at: Option<String>,
    pub orders: Vec<OrderRow>,
    /// Why the latest read failed; the orders are then the last good read's.
    pub error: Option<String>,
}

#[derive(Default)]
pub struct OrderBookCache {
    /// The last answer and when it was read. Held across a read, so callers
    /// at the same moment share one.
    latest: Mutex<Option<(Instant, OrdersAnswer)>>,
}

impl OrderBookCache {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The `HH:MM:SS` in a Kotak timestamp (`05-Nov-2026 10:15:30`,
/// `2026/11/05 10:15:30`).
fn clock_of(stamp: &str) -> Option<String> {
    let b = stamp.as_bytes();
    (0..b.len().saturating_sub(7)).find_map(|i| {
        let w = &b[i..i + 8];
        let digits = [0, 1, 3, 4, 6, 7].iter().all(|&j| w[j].is_ascii_digit());
        (digits && w[2] == b':' && w[5] == b':').then(|| stamp[i..i + 8].to_string())
    })
}

/// Kotak's order numbers count up through the day: a larger one is newer.
fn newer_first(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.trim(), b.trim());
    b.len().cmp(&a.len()).then_with(|| b.cmp(a))
}

/// `book` as the tab lists it: working orders, then the rest, each newest
/// first. `label` names a trading symbol's contract, when it can.
pub fn rows(book: &[KotakOrder], label: impl Fn(&str) -> Option<String>) -> Vec<OrderRow> {
    let positive = |v: f64| (v > 0.0).then_some(v);
    let mut out: Vec<OrderRow> = book
        .iter()
        .map(|o| OrderRow {
            id: o.order_no.trim().to_string(),
            symbol: o.trading_symbol.trim().to_string(),
            contract: label(o.trading_symbol.trim()),
            side: if o.is_sell() { "SELL" } else { "BUY" },
            order_type: o.order_type.trim().to_string(),
            qty: o.ordered_qty(),
            filled: o.filled_qty(),
            avg: positive(o.avg_price()).filter(|_| o.filled_qty() > 0),
            price: positive(o.limit_price()),
            trigger: positive(o.trigger()),
            status: o.status.trim().to_lowercase(),
            open: !o.is_terminal(),
            reason: Some(o.reject_reason.trim().to_string()).filter(|r| !r.is_empty()),
            time: clock_of(&o.order_time).or_else(|| clock_of(&o.updated_time)),
        })
        .collect();
    out.sort_by(|a, b| b.open.cmp(&a.open).then_with(|| newer_first(&a.id, &b.id)));
    out
}

/// `GET /api/v2/orders` — today's Kotak orders: working ones first, then the
/// rest with their fill price, newest first in each.
pub async fn orders_handler(State(state): State<AppState>) -> impl IntoResponse {
    let mut latest = state.order_book.latest.lock().await;
    if let Some((at, answer)) = latest.as_ref() {
        if at.elapsed() < FRESH_FOR {
            return Json(answer.clone());
        }
    }
    let previous = latest.as_ref().map(|(_, a)| a.clone()).unwrap_or_default();
    // Cloned out of its mutex: the read below never holds it.
    let client = state.kotak.lock().await.clone();
    let answer = match client {
        None => OrdersAnswer { error: Some("Kotak isn't connected".to_string()), ..previous },
        Some(c) => match c.get_order_book().await {
            Ok(book) => {
                let store = state.scrip_store.read().await;
                let label = |sym: &str| {
                    let r = store.as_ref()?.find_index_option(sym)?;
                    Some(crate::desk::contract_label(&r.symbol_name, r.normalized_strike(), Some(&r.option_type)))
                };
                OrdersAnswer {
                    fetched_at: Some(shared_domain::now_ist().format("%H:%M:%S").to_string()),
                    orders: rows(&book, label),
                    error: None,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Orders tab: Kotak order book read failed");
                OrdersAnswer { error: Some(format!("Kotak's order book didn't load: {e}")), ..previous }
            }
        },
    };
    *latest = Some((Instant::now(), answer.clone()));
    Json(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(no: &str, status: &str, side: &str, filled: i32, avg: &str, time: &str) -> KotakOrder {
        serde_json::from_value(serde_json::json!({
            "nOrdNo": no,
            "ordSt": status,
            "trdSym": "NIFTY26N1125100CE",
            "trnsTp": side,
            "prcTp": "MKT",
            "qty": "75",
            "fldQty": filled.to_string(),
            "avgPrc": avg,
            "prc": "0",
            "ordDtTm": time,
        }))
        .unwrap()
    }

    #[test]
    fn working_orders_come_first_then_the_rest_newest_first() {
        let book = [
            order("261105000000101", "complete", "B", 75, "100.50", "05-Nov-2026 09:20:01"),
            order("261105000000140", "open", "S", 0, "0", "05-Nov-2026 10:02:11"),
            order("261105000000120", "rejected", "S", 0, "0", ""),
            order("261105000000099", "trigger pending", "S", 0, "0", ""),
            order("261105000000130", "complete", "S", 75, "112.25", "05-Nov-2026 09:58:40"),
        ];
        let rows = rows(&book, |s| (s == "NIFTY26N1125100CE").then(|| "NIFTY 25100 CE".to_string()));
        let ids: Vec<_> = rows.iter().map(|r| &r.id[12..]).collect();
        assert_eq!(ids, ["140", "099", "130", "120", "101"]);
        assert!(rows[0].open && rows[1].open && !rows[2].open);
        // A fill shows what it filled at; nothing filled shows no price.
        assert_eq!((rows[2].filled, rows[2].avg, rows[2].side), (75, Some(112.25), "SELL"));
        assert_eq!(rows[3].avg, None);
        assert_eq!(rows[2].time.as_deref(), Some("09:58:40"));
        assert_eq!(rows[3].time, None);
        assert_eq!(rows[0].contract.as_deref(), Some("NIFTY 25100 CE"));
    }

    #[test]
    fn a_clock_is_read_out_of_either_of_kotaks_timestamps() {
        assert_eq!(clock_of("05-Nov-2026 10:15:30").as_deref(), Some("10:15:30"));
        assert_eq!(clock_of("2026/11/05 10:15:31").as_deref(), Some("10:15:31"));
        assert_eq!(clock_of(""), None);
        assert_eq!(clock_of("10:15"), None);
    }
}
