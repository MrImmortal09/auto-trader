//! Hyperliquid perps for whoever is signed in — each on paper with their own
//! USDC wallet (`hyperliquid::paper`), or on their own Hyperliquid account
//! while their Hyperliquid is LIVE (`hyperliquid::live`; LIVE ids are
//! Hyperliquid's oids). All behind the `hyperliquid` switch: while it is off
//! they answer 404. A LIVE order that would open is refused while the
//! admin's kill switch or the person's own is on, or while the admin has
//! switched LIVE off for them (close-only until they go back to Paper).
//!
//! - `GET    /api/v2/hl/account`
//! - `GET    /api/v2/hl/today` — today from IST midnight: P&L, fees, funding, closes
//! - `POST   /api/v2/hl/orders` — a market or limit order
//! - `PATCH  /api/v2/hl/orders/:id` `{price}` — move a resting limit
//! - `DELETE /api/v2/hl/orders/:id`
//! - `POST   /api/v2/hl/positions/:market/close` `{size?}`
//! - `PATCH  /api/v2/hl/positions/:market` — its stop and targets
//! - `POST   /api/v2/hl/lines` — a line order
//! - `PATCH  /api/v2/hl/lines/:id` — move it, or change its stop and targets
//! - `DELETE /api/v2/hl/lines/:id`
//! - `POST   /api/v2/hl/reset`

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use tokio::sync::OwnedRwLockReadGuard;

use std::sync::Arc;

use crate::auth::Identity;
use crate::hyperliquid::live::{LiveDesk, LiveOrder, LiveOrderAnswer};
use crate::hyperliquid::paper::{Kind, LevelsChange, OrderReq, Refusal};
use crate::hyperliquid::perp::Side;
use crate::hyperliquid::Market;
use crate::AppState;

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// The caller's account id, with the switch on.
async fn caller(state: &AppState, identity: &Identity) -> Result<i64, Response> {
    if !state.flags.read().await.hyperliquid {
        return Err(refuse(StatusCode::NOT_FOUND, "Hyperliquid is switched off"));
    }
    identity.user_id().ok_or_else(|| refuse(StatusCode::UNAUTHORIZED, "Sign in to trade perps on paper"))
}

/// `user_id`'s LIVE desk and network while their Hyperliquid is LIVE.
async fn live_for(state: &AppState, user_id: i64) -> Result<Option<(Arc<LiveDesk>, hyperliquid_client::Network)>, Response> {
    let desk = state.hl_live.desk(user_id).await.map_err(IntoResponse::into_response)?;
    Ok(desk.live_network().await.map(|n| (desk, n)))
}

/// Where a change goes: the caller's paper account, or (LIVE) their
/// Hyperliquid account on that network.
enum Desk {
    Paper(i64),
    Live(Arc<LiveDesk>, hyperliquid_client::Network),
}

/// The desk a change goes to, with the caller's LIVE switch's gate held
/// from this read until answered, so the mode can't change in between
/// (`LiveDesk::paper_gate`).
async fn desk_for(state: &AppState, user_id: i64) -> Result<(Desk, OwnedRwLockReadGuard<()>), Response> {
    let desk = state.hl_live.desk(user_id).await.map_err(IntoResponse::into_response)?;
    let gate = desk.paper_gate().await;
    Ok(match desk.live_network().await {
        Some(net) => (Desk::Live(desk, net), gate),
        None => (Desk::Paper(user_id), gate),
    })
}

/// A LIVE order that would open is refused while the admin's kill switch
/// is on (everyone's: the owner's pick, 2026-11-21), while the person's own
/// is, or while the admin has switched LIVE off for them; closes and
/// cancels still go through.
async fn may_open(state: &AppState, desk: &LiveDesk) -> Result<(), Response> {
    if state.trading_cfg.read().await.kill_switch_active {
        return Err(refuse(StatusCode::CONFLICT, "The kill switch is on: no new LIVE orders until it is reset"));
    }
    desk.may_open().map_err(IntoResponse::into_response)
}

/// The perp `key` names (any case): one on the app's list, added or
/// close-only (whether it takes an order is the desk's call).
fn market(key: &str) -> Result<&'static Market, Response> {
    Market::of_key(key.trim()).ok_or_else(|| refuse(StatusCode::BAD_REQUEST, format!("{key} isn't a perp the app trades")))
}

fn side(raw: &str) -> Result<Side, Response> {
    match raw {
        "buy" | "long" => Ok(Side::Long),
        "sell" | "short" => Ok(Side::Short),
        _ => Err(refuse(StatusCode::BAD_REQUEST, "side must be buy or sell")),
    }
}

/// Tells a field that is there, even as `null`, from one left out.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<f64>>, D::Error> {
    Option::<f64>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetBody {
    price: f64,
    /// Left out or `null`: all that is left.
    #[serde(default)]
    size: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderBody {
    market: String,
    side: String,
    size: f64,
    leverage: u32,
    /// `market` or `limit`.
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    reduce_only: bool,
    #[serde(default)]
    stop: Option<f64>,
    #[serde(default)]
    targets: Vec<TargetBody>,
}

/// One target set: a new one (no `id`), or one moved and resized.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetTarget {
    #[serde(default)]
    id: Option<String>,
    price: f64,
    /// Left out: a moved target keeps its size; `null`: all that is left.
    #[serde(default, deserialize_with = "present")]
    size: Option<Option<f64>>,
}

/// A change to a stop and targets; anything left out stays. `stop: null`
/// clears it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelsBody {
    #[serde(default, deserialize_with = "present")]
    stop: Option<Option<f64>>,
    #[serde(default)]
    set_target: Option<SetTarget>,
    #[serde(default)]
    remove_target: Option<String>,
    /// A line order's own price, moved (`PATCH /api/v2/hl/lines/:id` only).
    #[serde(default)]
    price: Option<f64>,
}

impl LevelsBody {
    fn change(&self) -> LevelsChange {
        LevelsChange {
            stop: self.stop,
            set_target: self.set_target.as_ref().map(|t| (t.id.clone(), t.price, t.size)),
            remove_target: self.remove_target.clone(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseBody {
    #[serde(default)]
    size: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveBody {
    price: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineBody {
    market: String,
    side: String,
    size: f64,
    leverage: u32,
    price: f64,
    /// Left out or `null`: until cancelled.
    #[serde(default)]
    valid_minutes: Option<i64>,
    #[serde(default)]
    stop: Option<f64>,
    #[serde(default)]
    targets: Vec<TargetBody>,
}

fn answer<T: serde::Serialize>(r: Result<T, Refusal>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(r) => r.into_response(),
    }
}

macro_rules! caller_or_return {
    ($state:expr, $identity:expr) => {
        match caller(&$state, &$identity).await {
            Ok(id) => id,
            Err(e) => return e,
        }
    };
}

/// The desk a route that changes something goes to, with the LIVE
/// switch's gate, held until the route answers.
macro_rules! desk_or_return {
    ($state:expr, $identity:expr) => {{
        let id = caller_or_return!($state, $identity);
        match desk_for(&$state, id).await {
            Ok(d) => d,
            Err(e) => return e,
        }
    }};
}

/// `user_id`'s LIVE desk and network while LIVE, or the route answers.
macro_rules! live_or_return {
    ($state:expr, $id:expr) => {
        match live_for(&$state, $id).await {
            Ok(l) => l,
            Err(e) => return e,
        }
    };
}

#[derive(Debug, Default, Deserialize)]
pub struct AccountQuery {
    /// LIVE: a perp's key, for what a new position in it can use now.
    #[serde(default)]
    market: Option<String>,
}

pub async fn account_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Query(q): Query<AccountQuery>) -> Response {
    let id = caller_or_return!(state, identity);
    // LIVE: their account as Hyperliquid holds it.
    if let Some((desk, net)) = live_or_return!(state, id) {
        let m = q.market.as_deref().and_then(|k| Market::of_key(k.trim()));
        return answer(desk.account(net, m).await);
    }
    answer(state.perps.view(id).await)
}

/// `GET /api/v2/hl/today` — the caller's perps today, from IST midnight:
/// their Hyperliquid account while LIVE, else their paper desk.
pub async fn today_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let id = caller_or_return!(state, identity);
    if let Some((desk, net)) = live_or_return!(state, id) {
        return answer(desk.today(net).await);
    }
    answer(state.perps.today(id).await)
}

pub async fn order_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<OrderBody>) -> Response {
    let (desk, _gate) = desk_or_return!(state, identity);
    let (m, side) = match (market(&b.market), side(&b.side)) {
        (Ok(m), Ok(s)) => (m, s),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let kind = match (b.kind.as_str(), b.price) {
        ("market", _) => Kind::Market,
        ("limit", Some(p)) => Kind::Limit(p),
        ("limit", None) => return refuse(StatusCode::BAD_REQUEST, "a limit order needs its price"),
        _ => return refuse(StatusCode::BAD_REQUEST, "type must be market or limit"),
    };
    let targets: Vec<(f64, Option<f64>)> = b.targets.iter().map(|t| (t.price, t.size)).collect();
    match desk {
        Desk::Paper(id) => answer(state.perps.order(id, OrderReq { market: m, side, size: b.size, leverage: b.leverage, kind, reduce_only: b.reduce_only, stop: b.stop, targets }).await),
        Desk::Live(desk, net) => {
            if !b.reduce_only {
                if let Err(e) = may_open(&state, &desk).await {
                    return e;
                }
            }
            let limit = match kind {
                Kind::Market => None,
                Kind::Limit(p) => Some(p),
            };
            answer(desk.order(net, &LiveOrder { market: m, side, size: b.size, leverage: b.leverage, limit, reduce_only: b.reduce_only, stop: b.stop, targets }).await)
        }
    }
}

pub async fn move_order_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(order_id): Path<String>, Json(b): Json<MoveBody>) -> Response {
    match desk_or_return!(state, identity) {
        (Desk::Paper(id), _gate) => answer(state.perps.move_order(id, &order_id, b.price).await),
        (Desk::Live(desk, net), _gate) => {
            // A moved limit can fill, a moved line is armed again: neither while the kill switch is on.
            if let Err(e) = may_open(&state, &desk).await {
                return e;
            }
            answer(desk.move_to(net, &order_id, b.price).await.map(|()| LiveOrderAnswer::default()))
        }
    }
}

pub async fn cancel_order_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(order_id): Path<String>) -> Response {
    let r = match desk_or_return!(state, identity) {
        (Desk::Paper(id), _gate) => state.perps.cancel_order(id, &order_id).await.map(|()| Vec::new()),
        (Desk::Live(desk, net), _gate) => desk.cancel(net, &order_id).await,
    };
    answer(r.map(|warnings| json!({ "status": "cancelled", "warnings": warnings })))
}

pub async fn close_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(key): Path<String>, body: Option<Json<CloseBody>>) -> Response {
    let (desk, _gate) = desk_or_return!(state, identity);
    let m = match market(&key) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let size = body.and_then(|Json(b)| b.size);
    match desk {
        Desk::Paper(id) => answer(state.perps.close(id, m, size).await.map(|legs| json!({ "filled": legs }))),
        Desk::Live(desk, net) => answer(desk.close(net, m, size).await),
    }
}

pub async fn levels_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(key): Path<String>, Json(b): Json<LevelsBody>) -> Response {
    let (desk, _gate) = desk_or_return!(state, identity);
    let m = match market(&key) {
        Ok(m) => m,
        Err(e) => return e,
    };
    if b.price.is_some() {
        return refuse(StatusCode::BAD_REQUEST, "a position's price can't be moved — only a line order's");
    }
    match desk {
        Desk::Paper(id) => answer(state.perps.set_levels(id, m, b.change()).await),
        Desk::Live(desk, net) => answer(desk.set_levels(net, m, &b.change()).await.map(|warnings| json!({ "status": "set", "warnings": warnings }))),
    }
}

pub async fn line_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<LineBody>) -> Response {
    let (desk, _gate) = desk_or_return!(state, identity);
    let (m, side) = match (market(&b.market), side(&b.side)) {
        (Ok(m), Ok(s)) => (m, s),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let targets: Vec<(f64, Option<f64>)> = b.targets.iter().map(|t| (t.price, t.size)).collect();
    match desk {
        Desk::Paper(id) => answer(state.perps.arm_line(id, m, side, b.size, b.leverage, b.price, b.valid_minutes, b.stop, targets).await),
        Desk::Live(desk, net) => {
            if let Err(e) = may_open(&state, &desk).await {
                return e;
            }
            // A LIVE line order rests on Hyperliquid until cancelled (the owner's pick).
            if b.valid_minutes.is_some() {
                return refuse(StatusCode::BAD_REQUEST, "A LIVE line order lasts until cancelled: leave valid_minutes out");
            }
            answer(desk.arm_line(net, &LiveOrder { market: m, side, size: b.size, leverage: b.leverage, limit: Some(b.price), reduce_only: false, stop: b.stop, targets }).await)
        }
    }
}

pub async fn update_line_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(line_id): Path<String>, Json(b): Json<LevelsBody>) -> Response {
    match desk_or_return!(state, identity) {
        (Desk::Paper(id), _gate) => answer(state.perps.update_line(id, &line_id, b.price, b.change()).await),
        (Desk::Live(desk, net), _gate) => {
            // A changed line order is armed again: not while the kill switch is on.
            if let Err(e) = may_open(&state, &desk).await {
                return e;
            }
            answer(desk.update_line(net, &line_id, b.price, &b.change()).await.map(|()| json!({ "status": "changed" })))
        }
    }
}

pub async fn cancel_line_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(line_id): Path<String>) -> Response {
    let r = match desk_or_return!(state, identity) {
        (Desk::Paper(id), _gate) => state.perps.cancel_line(id, &line_id).await.map(|()| Vec::new()),
        (Desk::Live(desk, net), _gate) => desk.cancel(net, &line_id).await,
    };
    answer(r.map(|warnings| json!({ "status": "cancelled", "warnings": warnings })))
}

pub async fn reset_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    match desk_or_return!(state, identity) {
        (Desk::Paper(id), _gate) => answer(state.perps.reset(id).await.map(|()| json!({ "status": "reset" }))),
        (Desk::Live(..), _gate) => refuse(StatusCode::CONFLICT, "A LIVE account isn't reset: Hyperliquid holds it"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_tell_a_cleared_level_from_one_left_out() {
        let b: LevelsBody = serde_json::from_str(r#"{"stop": null}"#).unwrap();
        assert_eq!(b.change().stop, Some(None));
        let b: LevelsBody = serde_json::from_str(r#"{"set_target": {"price": 64000}}"#).unwrap();
        assert_eq!(b.change().stop, None);
        assert_eq!(b.change().set_target, Some((None, 64_000.0, None)));
        let b: LevelsBody = serde_json::from_str(r#"{"set_target": {"id": "t1", "price": 64000, "size": null}}"#).unwrap();
        assert_eq!(b.change().set_target, Some((Some("t1".into()), 64_000.0, Some(None))));
        assert!(serde_json::from_str::<LevelsBody>(r#"{"sl": 1}"#).is_err());
        let o: OrderBody = serde_json::from_str(r#"{"market":"BTC","side":"sell","size":0.01,"leverage":5,"type":"limit","price":65000,"targets":[{"price":60000}]}"#).unwrap();
        assert_eq!((o.kind.as_str(), o.price, o.targets.len(), o.reduce_only), ("limit", Some(65_000.0), 1, false));
    }
}
