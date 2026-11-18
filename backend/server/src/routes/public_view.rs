//! `POST /api/public/trades {phone, range?}` — the view-only page: the closed
//! trades and P&L of a number the admin has marked public (`users.public`),
//! for anyone, with no sign-in and no code.
//!
//! Read-only, and deliberately narrow. Only calls already fully sold are
//! answered — never an open position, a wallet, a Telegram message or a
//! channel's name. A user's number answers their paper trades since their
//! last reset; the admin's answers the owner's trades in the current trading
//! mode. A number that isn't public answers exactly as an unknown one does,
//! and lookups are capped per address, so the allow-list can't be read off
//! this route.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{ip::client_ip, otp, Role};
use crate::history::{self, Call, CurvePoint, DayRow, GroupBy, GroupRow, Kpis, Range};
use crate::AppState;

/// Lookups one address may make in [`WINDOW`], found or not.
const MAX_LOOKUPS_PER_IP: u32 = 30;
const WINDOW: Duration = Duration::from_secs(600);

const NOT_PUBLIC: &str = "No public trades for this number.";

fn refuse(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

#[derive(Deserialize)]
pub struct Lookup {
    pub phone: String,
    #[serde(default)]
    pub range: Option<String>,
}

/// One fill of a closed call.
#[derive(Serialize)]
struct PublicLeg {
    t: String,
    action: String,
    qty: i64,
    price: f64,
}

/// A closed call, without its source: no message, channel or signal id.
#[derive(Serialize)]
struct PublicCall {
    index: String,
    side: Option<String>,
    strike: Option<f64>,
    expiry: Option<String>,
    opened_at: String,
    closed_at: Option<String>,
    qty: i64,
    pnl: Option<f64>,
    charges: f64,
    exit_reason: Option<String>,
    legs: Vec<PublicLeg>,
}

impl From<Call> for PublicCall {
    fn from(c: Call) -> Self {
        PublicCall {
            index: c.index,
            side: c.side,
            strike: c.strike,
            expiry: c.expiry,
            opened_at: c.opened_at,
            closed_at: c.closed_at,
            qty: c.bought_qty,
            pnl: c.pnl,
            charges: c.charges,
            exit_reason: c.exit_reason,
            legs: c.legs.into_iter().map(|l| PublicLeg { t: l.t, action: l.action, qty: l.qty, price: l.price }).collect(),
        }
    }
}

#[derive(Serialize)]
struct Answer {
    /// The name the admin gave the number, if any.
    name: Option<String>,
    /// `PAPER` or `LIVE`: what kind of trades these are.
    mode: String,
    range: String,
    from: Option<String>,
    to: String,
    kpis: Kpis,
    curve: Vec<CurvePoint>,
    /// P&L by index.
    groups: Vec<GroupRow>,
    days: Vec<DayRow>,
    /// Newest first.
    calls: Vec<PublicCall>,
    more_calls: usize,
}

pub async fn public_trades_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Lookup>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if let Err(retry) = state.limits.check(&format!("public-view:{ip}"), MAX_LOOKUPS_PER_IP, WINDOW) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry.to_string())],
            Json(json!({ "error": "Too many lookups. Try again later." })),
        )
            .into_response();
    }
    let Some(phone) = otp::normalize_phone(&req.phone) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter a 10-digit Indian mobile number");
    };
    let range_name = req.range.as_deref().unwrap_or("30d").trim().to_ascii_lowercase();
    let Some(range) = Range::parse(&range_name) else {
        return refuse(StatusCode::BAD_REQUEST, "range must be today, 7d, 30d or all");
    };

    let server_error = |what: &str, e: sqlx::Error| {
        tracing::error!("public view: {what}: {e}");
        refuse(StatusCode::INTERNAL_SERVER_ERROR, "could not read trades")
    };

    let found = sqlx::query_as::<_, (i64, Option<String>, String)>("SELECT id, name, role FROM users WHERE phone = ? AND public = 1")
        .bind(&phone)
        .fetch_optional(&state.db_pool)
        .await;
    let (user_id, name, role) = match found {
        Ok(Some(row)) => row,
        Ok(None) => return refuse(StatusCode::NOT_FOUND, NOT_PUBLIC),
        Err(e) => return server_error("user", e),
    };

    let (mode, legs) = if Role::parse(&role) == Role::Admin {
        let mode = state.trading_cfg.read().await.mode.clone();
        let legs = history::load_legs(&state.db_pool, &mode).await;
        (mode, legs)
    } else {
        // The wallet's epoch read as it is: this route writes nothing, so a
        // user who never traded has no wallet row and is on epoch 0.
        let epoch = match sqlx::query_scalar::<_, i64>("SELECT epoch FROM user_wallets WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&state.db_pool)
            .await
        {
            Ok(epoch) => epoch.unwrap_or(0),
            Err(e) => return server_error("wallet", e),
        };
        ("PAPER".to_string(), history::load_user_legs(&state.db_pool, user_id, epoch).await)
    };
    let legs = match legs {
        Ok(legs) => legs,
        Err(e) => return server_error("trades", e),
    };

    // `report` keeps only the calls fully sold inside the range.
    let report = history::report(&history::calls(legs), shared_domain::today_ist(), range, GroupBy::Index);
    Json(Answer {
        name: name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
        mode,
        range: range_name,
        from: report.from,
        to: report.to,
        kpis: report.kpis,
        curve: report.curve,
        groups: report.groups,
        days: report.days,
        calls: report.calls.into_iter().map(PublicCall::from).collect(),
        more_calls: report.more_calls,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::LegRow;

    /// What a visitor is sent about a call: the contract, its fills and its
    /// P&L — never where it came from.
    #[test]
    fn a_public_call_carries_no_source() {
        let call = Call {
            key: "-100123|4711|NIFTY".into(),
            channel: "Paid channel".into(),
            chat_id: Some(-100123),
            signal_id: Some("4711".into()),
            index: "NIFTY".into(),
            side: Some("CE".into()),
            strike: Some(25000.0),
            expiry: None,
            opened_at: "2026-11-17 10:00:00".into(),
            closed_at: Some("2026-11-17 10:30:00".into()),
            bought_qty: 75,
            sold_qty: 75,
            pnl: Some(1436.0),
            charges: 64.0,
            exit_reason: Some("TGT1_HIT".into()),
            message: Some("BUY NIFTY 25000 CE ABOVE 100 SL 80".into()),
            legs: vec![LegRow {
                id: 9,
                t: "2026-11-17 10:00:00".into(),
                action: "BUY".into(),
                qty: 75,
                price: 100.0,
                net: 7528.0,
                charges: 28.0,
                reason: None,
            }],
            outcome: None,
        };
        let sent = serde_json::to_value(PublicCall::from(call)).unwrap();
        let text = sent.to_string();
        for hidden in ["Paid channel", "-100123", "4711", "ABOVE 100", "message", "channel", "chat_id", "signal_id", "key"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        assert_eq!(sent["index"], "NIFTY");
        assert_eq!(sent["qty"], 75);
        assert_eq!(sent["pnl"], 1436.0);
        assert_eq!(sent["legs"][0]["price"], 100.0);
    }
}
