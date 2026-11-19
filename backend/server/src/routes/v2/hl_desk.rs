//! Hyperliquid perps on paper, for whoever is signed in — the owner and every
//! user alike, each on their own USDC wallet (`hyperliquid::paper`). All
//! behind the `hyperliquid` switch: while it is off they answer 404.
//!
//! - `GET    /api/v2/hl/account`
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
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Deserializer};
use serde_json::json;

use crate::auth::Identity;
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

fn market(key: &str) -> Result<&'static Market, Response> {
    Market::of_key(&key.to_ascii_uppercase()).ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "market must be BTC, ETH or GOLD"))
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

pub async fn account_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let id = caller_or_return!(state, identity);
    answer(state.perps.view(id).await)
}

pub async fn order_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<OrderBody>) -> Response {
    let id = caller_or_return!(state, identity);
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
    let targets = b.targets.iter().map(|t| (t.price, t.size)).collect();
    answer(state.perps.order(id, OrderReq { market: m, side, size: b.size, leverage: b.leverage, kind, reduce_only: b.reduce_only, stop: b.stop, targets }).await)
}

pub async fn move_order_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(order_id): Path<String>, Json(b): Json<MoveBody>) -> Response {
    let id = caller_or_return!(state, identity);
    answer(state.perps.move_order(id, &order_id, b.price).await)
}

pub async fn cancel_order_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(order_id): Path<String>) -> Response {
    let id = caller_or_return!(state, identity);
    answer(state.perps.cancel_order(id, &order_id).await.map(|()| json!({ "status": "cancelled" })))
}

pub async fn close_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(key): Path<String>, body: Option<Json<CloseBody>>) -> Response {
    let id = caller_or_return!(state, identity);
    let m = match market(&key) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let size = body.and_then(|Json(b)| b.size);
    answer(state.perps.close(id, m, size).await.map(|legs| json!({ "filled": legs })))
}

pub async fn levels_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(key): Path<String>, Json(b): Json<LevelsBody>) -> Response {
    let id = caller_or_return!(state, identity);
    let m = match market(&key) {
        Ok(m) => m,
        Err(e) => return e,
    };
    if b.price.is_some() {
        return refuse(StatusCode::BAD_REQUEST, "a position's price can't be moved — only a line order's");
    }
    answer(state.perps.set_levels(id, m, b.change()).await)
}

pub async fn line_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(b): Json<LineBody>) -> Response {
    let id = caller_or_return!(state, identity);
    let (m, side) = match (market(&b.market), side(&b.side)) {
        (Ok(m), Ok(s)) => (m, s),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let targets = b.targets.iter().map(|t| (t.price, t.size)).collect();
    answer(state.perps.arm_line(id, m, side, b.size, b.leverage, b.price, b.valid_minutes, b.stop, targets).await)
}

pub async fn update_line_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(line_id): Path<String>, Json(b): Json<LevelsBody>) -> Response {
    let id = caller_or_return!(state, identity);
    answer(state.perps.update_line(id, &line_id, b.price, b.change()).await)
}

pub async fn cancel_line_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(line_id): Path<String>) -> Response {
    let id = caller_or_return!(state, identity);
    answer(state.perps.cancel_line(id, &line_id).await.map(|()| json!({ "status": "cancelled" })))
}

pub async fn reset_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let id = caller_or_return!(state, identity);
    answer(state.perps.reset(id).await.map(|()| json!({ "status": "reset" })))
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
