//! `/api/v2/me/*` — a signed-in user's own paper portfolio (`crate::paper`):
//! its Desk, history, chart marks, positions and activity, and the buys,
//! sells, chart levels, exit-all and reset that change it. Only a user's
//! phone sign-in reaches these (`Access::PhoneUser`); each answers about the
//! caller alone, and another user's position is a plain 404.
//!
//! Nothing here touches the owner's engine, positions or tables. Fills are
//! booked at once at the live price, checked the way the owner's Execute
//! buys are: market hours, a live feed, a listed option (an index's; a
//! stock's too behind the `stock_paper` switch — `paper::resolve`), and a
//! price of its own from the last 10 s. No REST quote ever fills a paper
//! order: the price is the contract's own feed tick or nothing.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::auth::Identity;
use crate::desk::{self, DeskPosition};
use crate::execute;
use crate::history::{self, Call, GroupBy, Range};
use crate::paper::{self, PaperPosition, Refusal};
use crate::routes::v2::desk::{ClosedCall, DeskAnswer, Wallet};
use crate::routes::v2::history::{range_and_group, Answer};
use crate::routes::v2::positions::{sells_now, LevelsUpdate};
use crate::AppState;

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        refuse(self.status, self.message)
    }
}

/// The caller's user id. The policy lets only a user's sign-in in; anything
/// else is refused here too.
fn user_of(identity: &Identity) -> Result<i64, Response> {
    match identity {
        Identity::User { user_id } => Ok(*user_id),
        _ => Err(refuse(StatusCode::FORBIDDEN, "Sign in with your phone to trade on paper")),
    }
}

/// The caller's legs since their last reset, as calls, and their wallet.
async fn calls_of(state: &AppState, user_id: i64) -> Result<(paper::Wallet, Vec<Call>), Response> {
    let wallet = state.paper.wallet(user_id).await.map_err(IntoResponse::into_response)?;
    let legs = history::load_user_legs(&state.db_pool, user_id, wallet.epoch).await.map_err(|e| {
        tracing::error!("me: trades: {e}");
        refuse(StatusCode::INTERNAL_SERVER_ERROR, "could not read trades")
    })?;
    Ok((wallet, history::calls(legs)))
}

/// The underlying's live value: the index's feed value, or the stock's cash
/// row, whichever the position watches (however old).
fn underlying_ltp(state: &AppState, p: &PaperPosition) -> Option<f64> {
    p.underlying_price_key().and_then(|key| state.paper.price(&key))
}

fn desk_row(state: &AppState, p: &PaperPosition) -> DeskPosition {
    DeskPosition::of_paper(p, state.paper.price(&p.ws_key), underlying_ltp(state, p))
}

/// A held contract's price, while it is recent enough to trade on.
fn fresh_price(state: &AppState, p: &PaperPosition) -> Option<f64> {
    state
        .paper
        .price(&p.ws_key)
        .filter(|_| execute::contract_age_secs(&p.ws_key).is_some_and(|age| age <= execute::MAX_CONTRACT_AGE_SECS))
}

fn paper_buy_closed_message() -> String {
    if let Some(session) = shared_domain::special_session_on(shared_domain::today_ist()) {
        format!(
            "Market is closed — today's special session runs {}–{} IST",
            shared_domain::format_hhmm(session.start),
            shared_domain::format_hhmm(session.end)
        )
    } else {
        "Market is closed — paper trades run 09:15–15:30 IST".to_string()
    }
}

/// Sells run in the same hours as buys: there is no price to fill at outside them.
fn market_open() -> Result<(), Response> {
    if execute::manual_buy_window_open() {
        Ok(())
    } else {
        Err(refuse(StatusCode::CONFLICT, paper_buy_closed_message()))
    }
}

/// The caller's figures for today: booked plus open P&L, and the legs after
/// `after` for toasts with the newest leg's id — what `/api/v2/status` gives
/// a user.
pub(crate) async fn status_figures(
    state: &AppState,
    user_id: i64,
    after: Option<i64>,
    max_fills: usize,
) -> Result<(f64, Vec<desk::Fill>, i64, Vec<desk::Expiring>), Response> {
    let (_, calls) = calls_of(state, user_id).await?;
    let today = shared_domain::today_ist();
    let day = today.format("%Y-%m-%d").to_string();
    let held = state.paper.open(user_id).await;
    let open: Vec<DeskPosition> = held.iter().map(|p| desk_row(state, p)).collect();
    let day_pnl = desk::kpis(desk::booked_on(&calls, &day), &open, 0, 0).net;
    let fills = after.map(|after| desk::fills_after(&calls, &day, after, max_fills)).unwrap_or_default();
    Ok((day_pnl, fills, desk::last_leg_id(&calls), desk::paper_expiring_today(&held, today)))
}

/// `GET /api/v2/me/desk` — the caller's Desk Today, in `/api/v2/desk`'s
/// shape: `PAPER`, no kill switch, their wallet, today's figures, their open
/// positions (nothing ever waits) and the calls they closed today.
pub async fn desk_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let (wallet, calls) = match calls_of(&state, user_id).await {
        Ok(found) => found,
        Err(e) => return e,
    };
    let today = shared_domain::today_ist();
    let open: Vec<DeskPosition> = state.paper.open(user_id).await.iter().map(|p| desk_row(&state, p)).collect();
    let closed: Vec<ClosedCall> = history::report(&calls, today, Range::Today, GroupBy::Channel)
        .calls
        .into_iter()
        .map(|call| ClosedCall { call, message_at: None })
        .collect();
    let booked = desk::booked_on(&calls, &today.format("%Y-%m-%d").to_string());
    Json(DeskAnswer {
        mode: "PAPER".to_string(),
        kill_switch: false,
        wallet: Wallet { balance: Some(wallet.balance), source: "PAPER", updated_at: None },
        kpis: desk::kpis(booked, &open, 0, closed.len()),
        open,
        waiting: Vec::new(),
        closed,
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    range: Option<String>,
    group: Option<String>,
}

/// `GET /api/v2/me/history?range=today|7d|30d|all&group=channel|index|cepe`
/// — the caller's closed calls over the range, in `/api/v2/history`'s shape.
pub async fn history_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let (range_name, range, group) = match range_and_group(q.range.as_deref(), q.group.as_deref()) {
        Ok(parsed) => parsed,
        Err(e) => return e,
    };
    let (_, calls) = match calls_of(&state, user_id).await {
        Ok(found) => found,
        Err(e) => return e,
    };
    let report = history::report(&calls, shared_domain::today_ist(), range, group);
    Json(Answer { mode: "PAPER".to_string(), range: range_name, report }).into_response()
}

/// `GET /api/v2/me/marks` — the caller's fills today, for the Trade charts,
/// in `/api/v2/marks`' shape (no Telegram calls or positions).
pub async fn marks_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let epoch = match state.paper.wallet(user_id).await {
        Ok(w) => w.epoch,
        Err(r) => return r.into_response(),
    };
    match history::load_user_legs(&state.db_pool, user_id, epoch).await {
        Ok(legs) => crate::routes::v2::scalp::paper_marks(legs),
        Err(e) => {
            tracing::error!("me: marks: {e}");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "could not read trades")
        }
    }
}

/// `GET /api/v2/me/positions` — the caller's open positions, as Desk rows.
pub async fn positions_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let rows: Vec<DeskPosition> = state.paper.open(user_id).await.iter().map(|p| desk_row(&state, p)).collect();
    Json(rows).into_response()
}

/// `GET /api/v2/me/activity` — the caller's last 100 fills, levels and
/// resets, newest first. Kept in memory: a restart starts it over.
pub async fn activity_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    match user_of(&identity) {
        Ok(user_id) => Json(state.paper.activity(user_id)).into_response(),
        Err(e) => e,
    }
}

#[derive(Deserialize)]
pub struct BuyReq {
    trading_symbol: String,
    lots: i32,
}

/// `POST /api/v2/me/buy {trading_symbol, lots}` — buy at the live price: a
/// new position, or an add to the one on the same contract. Answers with
/// the fill. `trading_symbol` is an index option's, or — behind the
/// `stock_paper` switch — any NSE F&O underlying's CE/PE (`RELIANCE26NOV1400CE`),
/// resolved by `paper::resolve`; a future, a stock or an expired contract is
/// refused there. Every gate is the same for both: the market window and
/// the entry cutoff, a fresh feed, the contract's own tick from the last 10 s,
/// not being squared off, and the desk's caps and wallet.
pub async fn buy_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(req): Json<BuyReq>,
) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    if !(1..=paper::MAX_LOTS_PER_TAP).contains(&req.lots) {
        return refuse(StatusCode::BAD_REQUEST, format!("Lots must be between 1 and {}", paper::MAX_LOTS_PER_TAP));
    }
    if !execute::manual_buy_window_open() || trading_engine::is_entry_cutoff_passed() {
        return refuse(StatusCode::CONFLICT, paper_buy_closed_message());
    }
    if !execute::feed_fresh() {
        return refuse(StatusCode::CONFLICT, "Price feed is stale — not buying blind");
    }
    let contract = match paper::resolve::find_paper_contract(&state, &req.trading_symbol).await {
        Ok(contract) => contract,
        Err(r) => return r.into_response(),
    };
    // The contract's own feed tick, and a recent one: the feed being alive
    // says nothing about this contract, and a REST quote never fills.
    let Some(ltp) = execute::price(&state, &contract.ws_key) else {
        return refuse(StatusCode::CONFLICT, format!("No live price for {} yet", contract.trading_symbol));
    };
    if execute::contract_age_secs(&contract.ws_key).is_none_or(|age| age > execute::MAX_CONTRACT_AGE_SECS) {
        return refuse(
            StatusCode::CONFLICT,
            format!("{}'s price hasn't updated in over {:.0} s — not buying blind", contract.trading_symbol, execute::MAX_CONTRACT_AGE_SECS),
        );
    }
    if paper::engine::squaring_off(contract.expiry, shared_domain::now_ist()) {
        return refuse(
            StatusCode::CONFLICT,
            format!("{} expires today and is being squared off — pick a later expiry", contract.trading_symbol),
        );
    }
    match state.paper.buy(user_id, &contract, req.lots, ltp).await {
        Ok(filled) => Json(filled).into_response(),
        Err(r) => r.into_response(),
    }
}

#[derive(Deserialize)]
pub struct SellReq {
    lots: i32,
}

/// `POST /api/v2/me/positions/:id/sell {lots}` — sell some lots at the live
/// price. Never more than is held.
pub async fn sell_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    Json(req): Json<SellReq>,
) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    if let Err(e) = market_open() {
        return e;
    }
    match state.paper.sell(user_id, &id, Some(req.lots), "MANUAL_SELL", |p| fresh_price(&state, p)).await {
        Ok(filled) => Json(filled).into_response(),
        Err(r) => r.into_response(),
    }
}

/// `POST /api/v2/me/positions/:id/close` — sell all of it at the live price.
pub async fn close_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    if let Err(e) = market_open() {
        return e;
    }
    match state.paper.sell(user_id, &id, None, "MANUAL_CLOSE", |p| fresh_price(&state, p)).await {
        Ok(filled) => Json(filled).into_response(),
        Err(r) => r.into_response(),
    }
}

/// `POST /api/v2/me/exit-all` — sell everything the caller holds at the live
/// prices: what was sold, and why anything wasn't.
pub async fn exit_all_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    if let Err(e) = market_open() {
        return e;
    }
    let (sold, kept) = state.paper.exit_all(user_id, |p| fresh_price(&state, p)).await;
    Json(json!({ "sold": sold, "not_sold": kept })).into_response()
}

/// `PATCH /api/v2/me/positions/:id {stop?, set_target?, remove_target?}` —
/// a position's chart stop and targets, as `/api/v2/positions/:id` does for
/// the owner's scalps: `{price, trigger}` with `trigger` `"premium"` or
/// `"index"` (the underlying: an index option's index, a stock option's
/// stock price), a target with its `lots` (`null` = all left). One already
/// crossed sells on the watch loop's next pass (within half a second, at a
/// fresh price) — everything for the stop, a target's lots for a target:
/// `sells_now` and `sells_lots` say so.
pub async fn patch_levels_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    Json(update): Json<LevelsUpdate>,
) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let (pos, target_id) = match state.paper.set_levels(user_id, &id, &update).await {
        Ok(set) => set,
        Err(r) => return r.into_response(),
    };
    let premium = state.paper.price(&pos.ws_key);
    let index = underlying_ltp(&state, &pos);
    let hit = shared_domain::chart_hit(Some(&pos.option_type), &pos.chart, premium, index, pos.lots());
    let (sells, sells_lots) = sells_now(hit.as_ref());
    Json(json!({ "id": id, "chart": pos.chart, "sells_now": sells, "sells_lots": sells_lots, "target_id": target_id })).into_response()
}

/// `POST /api/v2/me/reset` — start over with ₹10,00,000 and no history.
/// Refused while anything is open.
pub async fn reset_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
    let user_id = match user_of(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match state.paper.reset(user_id).await {
        Ok(wallet) => Json(wallet).into_response(),
        Err(r) => r.into_response(),
    }
}
