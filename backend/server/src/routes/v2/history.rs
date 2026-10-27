//! `GET /api/v2/history` — the v2 Desk's history over a range (`crate::history`).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::history::{self, GroupBy, Range, Report};
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
pub(crate) struct Answer {
    pub(crate) mode: String,
    pub(crate) range: String,
    #[serde(flatten)]
    pub(crate) report: Report,
}

/// `range` (default `7d`, lower-cased as answered) and `group` (default
/// `channel`) from a query, or the 400 to answer.
pub(crate) fn range_and_group(range: Option<&str>, group: Option<&str>) -> Result<(String, Range, GroupBy), Response> {
    let name = range.unwrap_or("7d").trim().to_ascii_lowercase();
    let Some(range) = Range::parse(&name) else {
        return Err(bad_request("range must be today, 7d, 30d or all"));
    };
    let Some(group) = group.map_or(Some(GroupBy::Channel), GroupBy::parse) else {
        return Err(bad_request("group must be channel, index or cepe"));
    };
    Ok((name, range, group))
}

/// `GET /api/v2/history?range=today|7d|30d|all&group=channel|index|cepe&mode=PAPER|LIVE`
/// — the calls closed in the range (default `7d`), each with its legs and
/// original message, plus their KPIs, P&L curve, P&L grouped by channel (the
/// default), index or CE/PE, and trading days. `mode` defaults to the current
/// trading mode, as `/api/portfolio` does.
pub async fn history_handler(State(state): State<AppState>, Query(q): Query<HistoryQuery>) -> Response {
    let (range_name, range, group) = match range_and_group(q.range.as_deref(), q.group.as_deref()) {
        Ok(parsed) => parsed,
        Err(e) => return e,
    };
    let mode = match q.mode.as_deref().map(|m| m.trim().to_ascii_uppercase()) {
        Some(m) if m == "PAPER" || m == "LIVE" => m,
        Some(_) => return bad_request("mode must be PAPER or LIVE"),
        None => state.trading_cfg.read().await.mode.clone(),
    };

    let legs = match history::load_legs(&state.db_pool, &mode).await {
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
