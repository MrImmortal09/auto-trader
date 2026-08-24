use axum::{
    extract::{Path, State, Query},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use shared_domain::current_ist_timestamp_string;
use trading_engine::FeeCalculator;

use crate::AppState;
use shared_domain::{DbWriteMessage, MonitoredPosition, TradeState};

pub(crate) async fn persist_positions_snapshot(state: &AppState, snapshot: &[MonitoredPosition]) {
    if let Ok(json) = serde_json::to_string(snapshot) {
        let _ = state
            .db_tx
            .send(DbWriteMessage::PositionsSnapshot { json })
            .await;
    }
}

#[derive(Deserialize)]
pub struct ScripSearchParams {
    pub q: String,
}

#[derive(Deserialize)]
pub struct PatchPositionReq {
    pub override_qty: Option<i32>,
}

#[derive(Deserialize)]
pub struct SellPositionReq {
    pub qty: i32,
}

pub async fn positions_handler(State(state): State<AppState>) -> Json<Vec<MonitoredPosition>> {
    let positions = state.positions.read().await;
    let mut live_positions: Vec<MonitoredPosition> = positions
        .iter()
        .filter(|p| !matches!(p.state, TradeState::Closed))
        .cloned()
        .collect();

    for p in &mut live_positions {
        // Try ws_scrip_key first (precise lookup like "nse_fo|51386")
        if let Some(ref key) = p.ws_scrip_key {
            if let Some(price) = state.prices.get(key) {
                if *price > 0.0 {
                    p.ltp = Some(*price);
                }
            }
        }
        // Fallback: try instrument name
        if p.ltp.is_none() {
            if let Some(price) = state.prices.get(&p.signal.instrument_name) {
                if *price > 0.0 {
                    p.ltp = Some(*price);
                }
            }
        }
    }
    Json(live_positions)
}




pub async fn scrip_search_handler(
    State(state): State<AppState>,
    Query(params): Query<ScripSearchParams>,
) -> impl IntoResponse {
    let q = params.q.to_lowercase();
    let store_guard = state.scrip_store.read().await;
    
    if let Some(store) = &*store_guard {
        // Collect matches up to 50 items
        let mut results = Vec::new();
        for (sym, records) in &store.records {
            for rec in records {
                if sym.to_lowercase().contains(&q) || rec.instrument_token.to_lowercase().contains(&q) || rec.trading_symbol.to_lowercase().contains(&q) {
                    results.push(rec.clone());
                    if results.len() >= 50 { break; }
                }
            }
            if results.len() >= 50 { break; }
        }
        (StatusCode::OK, Json(results)).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "Scrip Master not loaded"}))).into_response()
    }
}


pub async fn scrip_download_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let raw_guard = state.raw_scrip_csv.read().await;
    if let Some(csv) = &*raw_guard {
        ([(axum::http::header::CONTENT_TYPE, "text/csv")], csv.clone()).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "Scrip Master not loaded".to_string()).into_response()
    }
}

pub async fn delete_position_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // In LIVE, forgetting a position that still holds stock or tracks a broker
    // order would leave a real, unmonitored exposure behind — and the engine
    // would cancel its stop as an orphan. Close it properly first.
    if state.trading_cfg.read().await.mode == "LIVE" {
        let blocked = {
            let positions = state.positions.read().await;
            positions.iter().find(|p| p.id == id).map(|p| {
                p.executed_qty > 0
                    || p.entry_order_id.is_some()
                    || p.sl_order_id.is_some()
                    || p.pending_exit_order_id.is_some()
            })
        };
        if blocked == Some(true) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "This position is live at the broker. Close it first, then delete."
                })),
            ).into_response();
        }
    }

    let mut positions = state.positions.write().await;
    let len_before = positions.len();
    positions.retain(|p| p.id != id);
    if positions.len() < len_before {
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(&state, &snapshot).await;
        (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response()
    }
}

pub async fn patch_position_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PatchPositionReq>,
) -> impl IntoResponse {
    let mut positions = state.positions.write().await;
    if let Some(pos) = positions.iter_mut().find(|p| p.id == id) {
        pos.override_qty = req.override_qty;
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(&state, &snapshot).await;
        (StatusCode::OK, Json(serde_json::json!({"status": "updated"}))).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response()
    }
}

pub async fn close_position_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = {
        let positions = state.positions.read().await;
        positions.iter().find(|p| p.id == id).map(|p| {
            (
                p.state.clone(),
                p.signal.instrument_name.clone(),
                p.signal.option_type.is_some(),
                p.executed_qty,
                p.avg_buy_price,
                p.ws_scrip_key.clone(),
                p.signal.signal_id.clone(),
                p.signal.raw_message.clone(),
            )
        })
    };

    let Some((position_state, instrument, is_options, qty, avg_buy_price, ws_scrip_key, signal_id, raw_message)) = snapshot else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response();
    };

    if !matches!(position_state, TradeState::Active | TradeState::Target1Hit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Only ongoing trades can be manually closed"})),
        ).into_response();
    }

    if qty <= 0 {
        let mut positions = state.positions.write().await;
        if let Some(pos) = positions.iter_mut().find(|p| p.id == id) {
            pos.state = TradeState::Closed;
        }
        return (StatusCode::OK, Json(serde_json::json!({"status": "closed", "qty": 0}))).into_response();
    }

    // LIVE: hand the close to the monitor rather than booking it here. It has to
    // cancel the resting stop before selling, and the trade must be recorded at
    // the price that actually filled — not at our last tick.
    if state.trading_cfg.read().await.mode == "LIVE" {
        let mut positions = state.positions.write().await;
        let Some(pos) = positions.iter_mut().find(|p| p.id == id) else {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response();
        };
        pos.force_exit = Some("CLOSED_VIA_FRONTEND".to_string());
        pos.override_exit_price = None;
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(&state, &snapshot).await;

        let _ = state.log_tx.send(format!(
            r#"{{"event":"MANUAL_CLOSE_REQUESTED","instrument":"{}","qty":{},"mode":"LIVE"}}"#,
            instrument, qty
        ));
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "exit_requested",
                "instrument": instrument,
                "qty": qty,
            })),
        ).into_response();
    }

    let ltp = ws_scrip_key
        .as_ref()
        .and_then(|k| state.prices.get(k).map(|v| *v))
        .or_else(|| state.prices.get(&instrument).map(|v| *v));

    let Some(exit_price) = ltp.filter(|p| *p > 0.0) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "No live LTP available for this position"})),
        ).into_response();
    };

    let cfg = state.trading_cfg.read().await;
    let fees = FeeCalculator::calculate(
        qty,
        exit_price,
        "SELL",
        is_options,
        cfg.brokerage_per_order,
    );

    let mut tx = match state.db_pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(position_id = %id, error = %e, "Failed to start DB transaction for manual close");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to persist manual close trade"})),
            ).into_response();
        }
    };

    let timestamp = current_ist_timestamp_string();

    if let Err(e) = sqlx::query(
        "INSERT INTO paper_trades
         (ticker, action, qty, executed_price, timestamp,
          gross_value, brokerage, stt_charge, sebi_fee,
          stamp_duty, transaction_charge, gst, net_value,
          signal_id, raw_message, exit_reason, mode)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&instrument)
    .bind("SELL")
    .bind(qty as i64)
    .bind(exit_price)
    .bind(&timestamp)
    .bind(fees.gross_value)
    .bind(fees.brokerage)
    .bind(fees.stt_charge)
    .bind(fees.sebi_fee)
    .bind(fees.stamp_duty)
    .bind(fees.transaction_charge)
    .bind(fees.gst)
    .bind(fees.net_value)
    .bind(&signal_id)
    .bind(&raw_message)
    .bind("CLOSED_VIA_FRONTEND")
    .bind("PAPER")
    .execute(&mut *tx)
    .await
    {
        tracing::error!(position_id = %id, error = %e, "Failed to insert manual close trade");
        let _ = tx.rollback().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to persist manual close trade"})),
        ).into_response();
    }

    if let Err(e) = sqlx::query("UPDATE wallet SET balance = balance + ? WHERE id = 1")
        .bind(fees.net_value)
        .execute(&mut *tx)
        .await
    {
        tracing::error!(position_id = %id, error = %e, "Failed to update wallet for manual close");
        let _ = tx.rollback().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to update wallet"})),
        ).into_response();
    }

    if let Err(e) = tx.commit().await {
        tracing::error!(position_id = %id, error = %e, "Failed to commit manual close transaction");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to persist manual close trade"})),
        ).into_response();
    }

    {
        let mut positions = state.positions.write().await;
        if let Some(pos) = positions.iter_mut().find(|p| p.id == id) {
            pos.executed_qty = 0;
            pos.state = TradeState::Closed;
        }
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(&state, &snapshot).await;
    }

    let pnl = fees.net_value - (avg_buy_price * qty as f64);
    let _ = state.log_tx.send(format!(
        r#"{{"event":"MANUAL_CLOSE","instrument":"{}","price":{:.2},"qty":{},"pnl":{:.2}}}"#,
        instrument, exit_price, qty, pnl
    ));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "closed",
            "instrument": instrument,
            "qty": qty,
            "exit_price": exit_price,
            "pnl": pnl,
        })),
    ).into_response()
}

/// `POST /api/positions/:id/sell` — sell a specific quantity at market,
/// leaving the rest of the position running untouched. Unlike Close, this
/// never fully exits by itself — request the full held quantity for that.
pub async fn sell_position_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SellPositionReq>,
) -> impl IntoResponse {
    let snapshot = {
        let positions = state.positions.read().await;
        positions.iter().find(|p| p.id == id).map(|p| {
            (
                p.state.clone(),
                p.signal.instrument_name.clone(),
                p.signal.option_type.is_some(),
                p.executed_qty,
                p.avg_buy_price,
                p.ws_scrip_key.clone(),
                p.signal.signal_id.clone(),
                p.signal.raw_message.clone(),
                p.resolved_order.as_ref().and_then(|o| o.quantity.parse::<i32>().ok()).filter(|v| *v > 0),
            )
        })
    };

    let Some((position_state, instrument, is_options, held_qty, avg_buy_price, ws_scrip_key, signal_id, raw_message, lot_size)) = snapshot else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response();
    };

    if !matches!(position_state, TradeState::Active | TradeState::Target1Hit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Only ongoing trades can be manually sold"})),
        ).into_response();
    }

    if req.qty <= 0 || req.qty > held_qty {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("quantity must be between 1 and {held_qty} (currently held)")})),
        ).into_response();
    }
    if let Some(lot) = lot_size {
        if req.qty % lot != 0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("quantity must be a multiple of the lot size ({lot})")})),
            ).into_response();
        }
    }

    // LIVE: hand the sell to the monitor, same as Close — it has to record the
    // trade at whatever price actually fills, not our last tick.
    if state.trading_cfg.read().await.mode == "LIVE" {
        let mut positions = state.positions.write().await;
        let Some(pos) = positions.iter_mut().find(|p| p.id == id) else {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response();
        };
        pos.manual_sell_qty = Some(req.qty);
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(&state, &snapshot).await;

        let _ = state.log_tx.send(format!(
            r#"{{"event":"MANUAL_SELL_REQUESTED","instrument":"{}","qty":{},"mode":"LIVE"}}"#,
            instrument, req.qty
        ));
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "sell_requested",
                "instrument": instrument,
                "qty": req.qty,
            })),
        ).into_response();
    }

    // PAPER: book the partial sell immediately at the current LTP.
    let ltp = ws_scrip_key
        .as_ref()
        .and_then(|k| state.prices.get(k).map(|v| *v))
        .or_else(|| state.prices.get(&instrument).map(|v| *v));

    let Some(exit_price) = ltp.filter(|p| *p > 0.0) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "No live LTP available for this position"})),
        ).into_response();
    };

    let cfg = state.trading_cfg.read().await;
    let fees = FeeCalculator::calculate(req.qty, exit_price, "SELL", is_options, cfg.brokerage_per_order);

    let mut tx = match state.db_pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(position_id = %id, error = %e, "Failed to start DB transaction for manual sell");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to persist manual sell trade"})),
            ).into_response();
        }
    };

    let timestamp = current_ist_timestamp_string();

    if let Err(e) = sqlx::query(
        "INSERT INTO paper_trades
         (ticker, action, qty, executed_price, timestamp,
          gross_value, brokerage, stt_charge, sebi_fee,
          stamp_duty, transaction_charge, gst, net_value,
          signal_id, raw_message, exit_reason, mode)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&instrument)
    .bind("SELL")
    .bind(req.qty as i64)
    .bind(exit_price)
    .bind(&timestamp)
    .bind(fees.gross_value)
    .bind(fees.brokerage)
    .bind(fees.stt_charge)
    .bind(fees.sebi_fee)
    .bind(fees.stamp_duty)
    .bind(fees.transaction_charge)
    .bind(fees.gst)
    .bind(fees.net_value)
    .bind(&signal_id)
    .bind(&raw_message)
    .bind("MANUAL_SELL")
    .bind("PAPER")
    .execute(&mut *tx)
    .await
    {
        tracing::error!(position_id = %id, error = %e, "Failed to insert manual sell trade");
        let _ = tx.rollback().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to persist manual sell trade"})),
        ).into_response();
    }

    if let Err(e) = sqlx::query("UPDATE wallet SET balance = balance + ? WHERE id = 1")
        .bind(fees.net_value)
        .execute(&mut *tx)
        .await
    {
        tracing::error!(position_id = %id, error = %e, "Failed to update wallet for manual sell");
        let _ = tx.rollback().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to update wallet"})),
        ).into_response();
    }

    if let Err(e) = tx.commit().await {
        tracing::error!(position_id = %id, error = %e, "Failed to commit manual sell transaction");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to persist manual sell trade"})),
        ).into_response();
    }

    let remaining = held_qty - req.qty;
    {
        let mut positions = state.positions.write().await;
        if let Some(pos) = positions.iter_mut().find(|p| p.id == id) {
            pos.executed_qty = remaining;
            if remaining <= 0 {
                pos.state = TradeState::Closed;
            }
        }
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(&state, &snapshot).await;
    }

    let pnl = fees.net_value - (avg_buy_price * req.qty as f64);
    let _ = state.log_tx.send(format!(
        r#"{{"event":"MANUAL_SELL","instrument":"{}","price":{:.2},"qty":{},"pnl":{:.2}}}"#,
        instrument, exit_price, req.qty, pnl
    ));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "sold",
            "instrument": instrument,
            "qty": req.qty,
            "remaining": remaining,
            "exit_price": exit_price,
            "pnl": pnl,
        })),
    ).into_response()
}

/// `GET /api/positions/reconcile/preview` — live comparison against Kotak,
/// LIVE mode only. Read-only: returns a list of findings for the frontend to
/// show as questions, never mutates anything by itself.
pub async fn reconcile_preview_handler(State(state): State<AppState>) -> impl IntoResponse {
    if state.trading_cfg.read().await.mode != "LIVE" {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Only meaningful in LIVE mode"}))).into_response();
    }
    match trading_engine::preview_reconciliation(&state.positions, &state.kotak).await {
        Ok(findings) => (StatusCode::OK, Json(findings)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e}))).into_response(),
    }
}

/// `POST /api/positions/reconcile/apply` — apply user-confirmed actions from
/// a preview report. Re-checks broker truth at apply time rather than
/// trusting anything echoed back by the client.
pub async fn reconcile_apply_handler(
    State(state): State<AppState>,
    Json(items): Json<Vec<shared_domain::ReconcileApplyItem>>,
) -> impl IntoResponse {
    if state.trading_cfg.read().await.mode != "LIVE" {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Only meaningful in LIVE mode"}))).into_response();
    }
    match trading_engine::apply_reconciliation(&state.positions, &state.kotak, &state.db_tx, &state.log_tx, &items).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "applied", "count": items.len()}))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e}))).into_response(),
    }
}

/// Debug endpoint to show all live prices in the shared map.
pub async fn prices_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let map: std::collections::HashMap<String, f64> = state.prices
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect();
    Json(serde_json::json!(map))
}
