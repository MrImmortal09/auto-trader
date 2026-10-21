//! `GET /api/v2/history` — the v2 Desk's history over a range (`crate::history`).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::history::{self, GroupBy, Leg, Range, Report};
use crate::AppState;

fn bad_request(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    range: Option<String>,
    group: Option<String>,
    mode: Option<String>,
}

#[derive(Serialize)]
struct Answer {
    mode: String,
    range: String,
    #[serde(flatten)]
    report: Report,
}

/// `GET /api/v2/history?range=today|7d|30d|all&group=channel|index|cepe&mode=PAPER|LIVE`
/// — the calls closed in the range (default `7d`), each with its legs and
/// original message, plus their KPIs, P&L curve, P&L grouped by channel (the
/// default), index or CE/PE, and trading days. `mode` defaults to the current
/// trading mode, as `/api/portfolio` does.
pub async fn history_handler(State(state): State<AppState>, Query(q): Query<HistoryQuery>) -> Response {
    let range_name = q.range.as_deref().unwrap_or("7d").trim().to_ascii_lowercase();
    let Some(range) = Range::parse(&range_name) else {
        return bad_request("range must be today, 7d, 30d or all");
    };
    let Some(group) = q.group.as_deref().map_or(Some(GroupBy::Channel), GroupBy::parse) else {
        return bad_request("group must be channel, index or cepe");
    };
    let mode = match q.mode.as_deref().map(|m| m.trim().to_ascii_uppercase()) {
        Some(m) if m == "PAPER" || m == "LIVE" => m,
        Some(_) => return bad_request("mode must be PAPER or LIVE"),
        None => state.trading_cfg.read().await.mode.clone(),
    };

    let legs = match sqlx::query_as::<_, Leg>(
        "SELECT id, ticker, action, qty, executed_price, timestamp, net_value,
                brokerage + stt_charge + sebi_fee + stamp_duty + transaction_charge + gst AS charges,
                signal_id, raw_message, exit_reason, chat_id, chat_title, option_type, strike
         FROM paper_trades WHERE mode = ? ORDER BY id ASC",
    )
    .bind(&mode)
    .fetch_all(&state.db_pool)
    .await
    {
        Ok(legs) => legs,
        Err(e) => {
            tracing::error!("history: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "could not read trades" }))).into_response();
        }
    };

    let calls = history::calls(legs);
    let report = history::report(&calls, shared_domain::today_ist(), range, group);
    Json(Answer { mode, range: range_name, report }).into_response()
}
