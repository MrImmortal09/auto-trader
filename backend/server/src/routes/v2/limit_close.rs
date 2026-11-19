//! `POST|DELETE /api/v2/positions/:id/limit-close` — the Trade panel's limit
//! close on the owner's desk: a limit sell of some of a scalp's lots, which
//! only ever closes what is held (see `shared_domain::LimitClose`). The route
//! only records it; the engine places it at Kotak (LIVE) or fills it when the
//! price reaches it (Paper), and cancels it before any other sell.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use shared_domain::{LimitClose, MonitoredPosition, TradeState};

use crate::execute;
use crate::routes::auth_kotak::send_log;
use crate::routes::positions::persist_positions_snapshot;
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitCloseReq {
    pub lots: i32,
    pub price: f64,
}

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// The contract's lot size, as the engine reads it.
fn lot_of(pos: &MonitoredPosition) -> i32 {
    pos.resolved_order.as_ref().and_then(|o| o.quantity.parse::<i32>().ok()).filter(|v| *v > 0).unwrap_or(1)
}

/// Why `pos` can't take a limit close of `lots` at `price` now, if it can't.
/// Only an open scalp with nothing else selling it, whole lots of what it
/// holds, a positive price.
pub(crate) fn refusal(pos: &MonitoredPosition, lots: i32, price: f64) -> Option<(StatusCode, String)> {
    let conflict = |m: &str| Some((StatusCode::CONFLICT, m.to_string()));
    if !pos.manual {
        return conflict("Only a scalp takes a limit close");
    }
    if !matches!(pos.state, TradeState::Active | TradeState::Target1Hit) || pos.executed_qty <= 0 {
        return conflict("Only an open position can be closed");
    }
    if pos.live_halt.is_some() {
        return conflict("The engine has halted this position — see the log");
    }
    if pos.limit_close.is_some() {
        return conflict("A limit sell already rests for this position — cancel it first");
    }
    if pos.sell_in_flight() {
        return conflict("A sell is already on its way for this position");
    }
    if pos.sl_order_id.is_some() {
        return conflict("A sell order already rests at Kotak for this position");
    }
    let lot = lot_of(pos);
    let held_lots = pos.executed_qty / lot;
    if !(1..=held_lots).contains(&lots) {
        return Some((StatusCode::BAD_REQUEST, format!("Lots must be between 1 and {held_lots} (what is held)")));
    }
    if !(price.is_finite() && price > 0.0) {
        return Some((StatusCode::BAD_REQUEST, "The price must be a positive number".to_string()));
    }
    None
}

/// `POST /api/v2/positions/:id/limit-close {lots, price}` — rest a limit sell
/// of `lots` of a scalp at `price` (put on the contract's tick, rounded up)
/// or better. LIVE the engine sends it to Kotak on its next tick, as a DAY
/// order; Paper it fills when the price reaches it, or at once at the last
/// price if it is already there. `fills_now` says the price is already at
/// or above it.
pub async fn place_handler(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<LimitCloseReq>) -> Response {
    let (live, kill_switch) = {
        let cfg = state.trading_cfg.read().await;
        (cfg.mode == "LIVE", cfg.kill_switch_active)
    };
    if kill_switch {
        return refuse(StatusCode::CONFLICT, "The kill switch is on");
    }
    if !shared_domain::is_market_open() {
        return refuse(StatusCode::CONFLICT, "The market is closed — a limit sell lasts the day it is placed");
    }
    if live && !execute::kotak_ready(&state) {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "Kotak isn't connected — nothing can be placed");
    }

    let mut positions = state.positions.write().await;
    let Some(pos) = positions.iter_mut().find(|p| p.id == id) else {
        return refuse(StatusCode::NOT_FOUND, "not found");
    };
    if let Some((status, msg)) = refusal(pos, req.lots, req.price) {
        return refuse(status, msg);
    }
    let lot = lot_of(pos);
    let qty = req.lots * lot;
    let price = shared_domain::limit_price_on_tick(req.price, pos.tick_size);
    let ltp = pos.ws_scrip_key.as_deref().and_then(|k| execute::price(&state, k));
    let fills_now = ltp.is_some_and(|l| price <= l);
    pos.limit_close = Some(LimitClose::new(qty, price, shared_domain::current_ist_timestamp_string(), fills_now));
    let instrument = pos.signal.instrument_name.clone();
    let contract = crate::desk::contract_label(&instrument, pos.signal.strike, pos.signal.option_type.as_deref());
    let snapshot = positions.clone();
    drop(positions);
    persist_positions_snapshot(&state, &snapshot).await;

    send_log(&state.db_tx, &state.log_tx, "INFO", json!({
        "event": "LIMIT_CLOSE_REQUESTED",
        "level": "INFO",
        "message": format!("Limit sell of {qty} {contract} at ₹{price:.2} requested"),
        "instrument": instrument,
        "position_id": id,
        "qty": qty,
        "limit_price": price,
        "mode": if live { "LIVE" } else { "PAPER" },
    }))
    .await;
    Json(json!({ "id": id, "qty": qty, "price": price, "fills_now": fills_now })).into_response()
}

/// `DELETE /api/v2/positions/:id/limit-close` — cancel a scalp's limit
/// sell. The engine does it on its next tick (LIVE: asks Kotak, and books
/// whatever filled before the cancel landed). One missing from Kotak's order
/// book past the grace window is released instead (the owner has checked
/// Kotak): a best-effort cancel, then the engine stops counting it.
pub async fn cancel_handler(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut positions = state.positions.write().await;
    let Some(pos) = positions.iter_mut().find(|p| p.id == id) else {
        return refuse(StatusCode::NOT_FOUND, "not found");
    };
    let Some(lc) = pos.limit_close.as_mut() else {
        return refuse(StatusCode::NOT_FOUND, "No limit sell rests for this position");
    };
    if lc.cancel_requested {
        return Json(json!({ "id": id, "status": "cancelling" })).into_response();
    }
    // Only ever flagged: the engine may be sending it this very moment, and
    // must keep its order number to cancel it.
    lc.cancel_requested = true;
    let instrument = pos.signal.instrument_name.clone();
    let snapshot = positions.clone();
    drop(positions);
    persist_positions_snapshot(&state, &snapshot).await;
    send_log(&state.db_tx, &state.log_tx, "INFO", json!({
        "event": "LIMIT_CLOSE_CANCEL_REQUESTED",
        "level": "INFO",
        "message": "Limit sell cancel requested",
        "instrument": instrument,
        "position_id": id,
    }))
    .await;
    Json(json!({ "id": id, "status": "cancelling" })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_domain::{AmoFlag, ExchangeSegment, OrderRequest, OrderType, ProductCode, TradeSignal, TransactionType, Validity};

    fn order() -> OrderRequest {
        OrderRequest {
            after_market_order: AmoFlag::No,
            disclosed_quantity: "0".into(),
            exchange_segment: ExchangeSegment::NseFo,
            market_protection: "0".into(),
            product_code: ProductCode::Nrml,
            portfolio_flag: "N".into(),
            price: "0".into(),
            order_type: OrderType::Market,
            quantity: "65".into(),
            validity: Validity::Day,
            trigger_price: "0".into(),
            trading_symbol: "NIFTY25NOV25100CE".into(),
            transaction_type: TransactionType::Buy,
        }
    }

    fn scalp() -> MonitoredPosition {
        MonitoredPosition {
            id: "p1".into(),
            signal: TradeSignal { instrument_name: "NIFTY".into(), strike: Some(25_100.0), option_type: Some("CE".into()), action: "BUY".into(), ..Default::default() },
            state: TradeState::Active,
            executed_qty: 260,
            manual: true,
            resolved_order: Some(order()),
            ..Default::default()
        }
    }

    #[test]
    fn a_limit_close_takes_whole_lots_of_an_open_scalp_and_nothing_more() {
        assert_eq!(refusal(&scalp(), 1, 150.0), None);
        assert_eq!(refusal(&scalp(), 4, 150.0), None);
        // More lots than are held, none, or no price.
        assert_eq!(refusal(&scalp(), 5, 150.0).map(|r| r.0), Some(StatusCode::BAD_REQUEST));
        assert_eq!(refusal(&scalp(), 0, 150.0).map(|r| r.0), Some(StatusCode::BAD_REQUEST));
        assert_eq!(refusal(&scalp(), 1, 0.0).map(|r| r.0), Some(StatusCode::BAD_REQUEST));
        assert_eq!(refusal(&scalp(), 1, f64::NAN).map(|r| r.0), Some(StatusCode::BAD_REQUEST));
        // A Telegram call's position, or one still waiting on its buy.
        let mut signal = scalp();
        signal.manual = false;
        assert_eq!(refusal(&signal, 1, 150.0).map(|r| r.0), Some(StatusCode::CONFLICT));
        let mut waiting = scalp();
        waiting.state = TradeState::WaitingForEntry;
        waiting.executed_qty = 0;
        assert_eq!(refusal(&waiting, 1, 150.0).map(|r| r.0), Some(StatusCode::CONFLICT));
    }

    #[test]
    fn a_limit_close_waits_for_any_other_sell_and_there_is_only_ever_one() {
        let mut resting = scalp();
        resting.limit_close = Some(LimitClose::new(65, 150.0, "2026-11-19 10:00:00".into(), false));
        assert_eq!(refusal(&resting, 1, 152.0).map(|r| r.0), Some(StatusCode::CONFLICT));
        let mut closing = scalp();
        closing.force_exit = Some("CLOSED_VIA_FRONTEND".into());
        assert_eq!(refusal(&closing, 1, 150.0).map(|r| r.0), Some(StatusCode::CONFLICT));
        let mut selling = scalp();
        selling.manual_sell_qty = Some(65);
        assert_eq!(refusal(&selling, 1, 150.0).map(|r| r.0), Some(StatusCode::CONFLICT));
        let mut sent = scalp();
        sent.pending_exit_order_id = Some("ORD1".into());
        assert_eq!(refusal(&sent, 1, 150.0).map(|r| r.0), Some(StatusCode::CONFLICT));
        let mut stop_at_kotak = scalp();
        stop_at_kotak.sl_order_id = Some("SL1".into());
        assert_eq!(refusal(&stop_at_kotak, 1, 150.0).map(|r| r.0), Some(StatusCode::CONFLICT));
    }
}
