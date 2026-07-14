use axum::{
    extract::{Path, State, Query},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use trading_engine::FeeCalculator;

use crate::AppState;
use shared_domain::{MonitoredPosition, TradeState};

#[derive(Deserialize)]
pub struct ScripSearchParams {
    pub q: String,
}

#[derive(Deserialize)]
pub struct PatchPositionReq {
    pub override_qty: Option<i32>,
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
    let mut positions = state.positions.write().await;
    let len_before = positions.len();
    positions.retain(|p| p.id != id);
    if positions.len() < len_before {
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
            )
        })
    };

    let Some((position_state, instrument, is_options, qty, avg_buy_price, ws_scrip_key)) = snapshot else {
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

    if let Err(e) = sqlx::query(
        "INSERT INTO paper_trades
         (ticker, action, qty, executed_price,
          gross_value, brokerage, stt_charge, sebi_fee,
          stamp_duty, transaction_charge, gst, net_value)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&instrument)
    .bind("SELL")
    .bind(qty as i64)
    .bind(exit_price)
    .bind(fees.gross_value)
    .bind(fees.brokerage)
    .bind(fees.stt_charge)
    .bind(fees.sebi_fee)
    .bind(fees.stamp_duty)
    .bind(fees.transaction_charge)
    .bind(fees.gst)
    .bind(fees.net_value)
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

/// Debug endpoint to show all live prices in the shared map.
pub async fn prices_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let map: std::collections::HashMap<String, f64> = state.prices
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect();
    Json(serde_json::json!(map))
}
