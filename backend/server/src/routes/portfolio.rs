use std::convert::Infallible;

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use futures::stream::StreamExt as _;
use serde::Serialize;
use shared_domain::TradeSignal;
use sqlx::{FromRow, Row};
use tokio_stream::wrappers::BroadcastStream;

use crate::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize, FromRow)]
pub struct PaperTrade {
    pub id: i64,
    pub ticker: String,
    pub action: String,
    pub qty: i64,
    pub executed_price: f64,
    pub gross_value: f64,
    pub brokerage: f64,
    pub stt_charge: f64,
    pub sebi_fee: f64,
    pub stamp_duty: f64,
    pub transaction_charge: f64,
    pub gst: f64,
    pub timestamp: String,
    pub signal_id: Option<String>,
    pub raw_message: Option<String>,
}

#[derive(Serialize)]
pub struct Portfolio {
    pub balance: f64,
    pub trades: Vec<PaperTrade>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/webhook/telegram` — inject a `TradeSignal` directly.
pub async fn webhook_handler(
    State(state): State<AppState>,
    Json(signal): Json<TradeSignal>,
) -> impl IntoResponse {
    tracing::info!(instrument = %signal.instrument_name, "Webhook signal received");
    match state.signal_tx.send(signal) {
        Ok(_) => StatusCode::OK,
        Err(_) => {
            tracing::warn!("No engine receivers — signal dropped");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

/// `GET /api/logs/stream` — SSE stream of live engine events.
pub async fn sse_logs_handler(
    State(state): State<AppState>,
) -> Sse<impl futures::stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.log_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| async move {
        msg.ok().map(|data| Ok(Event::default().data(data)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /api/portfolio` — wallet balance + last 100 paper trades.
pub async fn portfolio_handler(
    State(state): State<AppState>,
) -> Result<Json<Portfolio>, StatusCode> {
    let balance: f64 = sqlx::query("SELECT balance FROM wallet WHERE id = 1")
        .fetch_one(&state.db_pool)
        .await
        .map_err(|e| { tracing::error!("wallet: {e}"); StatusCode::INTERNAL_SERVER_ERROR })?
        .get::<f64, _>(0);

    let trades: Vec<PaperTrade> = sqlx::query_as(
        "SELECT id, ticker, action, qty, executed_price,
                gross_value, brokerage, stt_charge, sebi_fee,
                stamp_duty, transaction_charge, gst, net_value, timestamp,
                signal_id, raw_message
         FROM paper_trades ORDER BY timestamp DESC LIMIT 1000",
    )
    .fetch_all(&state.db_pool)
    .await
    .map_err(|e| { tracing::error!("trades: {e}"); StatusCode::INTERNAL_SERVER_ERROR })?;

    Ok(Json(Portfolio { balance, trades }))
}

/// `GET /api/logs/history` — Fetch recent logs from DB
pub async fn logs_history_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT message, timestamp FROM (SELECT id, message, timestamp FROM system_logs ORDER BY id DESC LIMIT 500) ORDER BY id ASC"
    )
    .fetch_all(&state.db_pool)
    .await
    .map_err(|e| { tracing::error!("logs history: {e}"); StatusCode::INTERNAL_SERVER_ERROR })?;

    let logs = rows.into_iter().filter_map(|(msg, ts)| {
        let parsed = serde_json::from_str::<serde_json::Value>(&msg).ok()?;
        Some(serde_json::json!({
            "message": parsed,
            "timestamp": ts
        }))
    }).collect();
    Ok(Json(logs))
}
