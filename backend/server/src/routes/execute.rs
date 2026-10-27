//! Execute screen routes: the live ladder stream, one-tap manual buys, and
//! "exit all scalps".
//!
//! A manual buy never reaches Kotak from here. It becomes a `MonitoredPosition`
//! (`manual`, entry condition `"MARKET"`) — or an add on the one already held
//! — and the position monitor sends it on its next tick with every LIVE
//! safeguard: funds pre-check, RMS-reject retry, recovery of an unconfirmed
//! send, and exits that can never sell more than is held.

use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Extension, Json,
};
use chrono::NaiveDate;
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::json;
use shared_domain::{ManualAdd, MonitoredPosition, TradeSignal, TradeState, INDEX_NAMES};

use crate::auth::Identity;
use crate::execute::{self, STREAM_EVERY};
use crate::routes::positions::persist_positions_snapshot;
use crate::AppState;

/// Most lots one tap may buy — a fat-finger guard, enforced here whatever the
/// screen sends.
pub const MAX_LOTS_PER_TAP: i32 = 10;

#[derive(Deserialize)]
pub struct StreamQuery {
    index: Option<String>,
    /// `YYYY-MM-DD`; missing, the nearest expiry. An unlisted one gives a
    /// board error rather than another expiry's ladder.
    expiry: Option<String>,
    /// A strike to centre the subscribed window on; missing, ATM.
    centre: Option<f64>,
}

/// `GET /api/execute/stream?index=NIFTY&expiry=&centre=` — SSE, one frame
/// every 250 ms: every index's spot, the chosen index's ladder for that
/// expiry (nearest by default) around that strike (ATM by default), and the
/// caller's open positions (the engine's for the owner and the admin, none
/// for a visitor — see `execute::build_frame`). Public: a token, when there
/// is one, arrives as `?token=` because an `EventSource` can't set headers.
/// The view is fixed when the stream opens, so the frontend reopens it
/// whenever the sign-in changes (`useExecuteStream`).
pub async fn execute_stream_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let wanted = q.index.unwrap_or_default().trim().to_uppercase();
    let index = INDEX_NAMES.iter().copied().find(|i| *i == wanted).unwrap_or("NIFTY");
    let expiry = q.expiry.as_deref().and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok());
    let centre = q.centre.filter(|c| c.is_finite() && *c > 0.0);

    let mut tick = tokio::time::interval(STREAM_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stream = futures::stream::unfold((state, tick, identity), move |(state, mut tick, identity)| async move {
        tick.tick().await;
        // A user disabled by the admin loses the stream now, not on reconnect.
        if !crate::auth::still_signed_in(&state.auth, &identity) {
            return None;
        }
        let frame = execute::build_frame(&state, &identity, index, expiry, centre).await;
        let event = Event::default().data(serde_json::to_string(&frame).unwrap_or_default());
        Some((Ok(event), (state, tick, identity)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
pub struct BuyReq {
    pub trading_symbol: String,
    pub lots: i32,
}

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// `POST /api/execute/buy {trading_symbol, lots}` — one tap. Opens a manual
/// position, or adds to the open manual position on the same contract.
/// Answers at once with the verdict; the fill shows up on the stream.
pub async fn execute_buy_handler(State(state): State<AppState>, Json(req): Json<BuyReq>) -> Response {
    if !(1..=MAX_LOTS_PER_TAP).contains(&req.lots) {
        return refuse(StatusCode::BAD_REQUEST, format!("Lots must be between 1 and {MAX_LOTS_PER_TAP}"));
    }
    let (mode, kill_switch) = {
        let cfg = state.trading_cfg.read().await;
        (cfg.mode.clone(), cfg.kill_switch_active)
    };
    if kill_switch {
        return refuse(StatusCode::CONFLICT, "Kill switch is active — trading is halted");
    }
    if !execute::manual_buy_window_open() || trading_engine::is_entry_cutoff_passed() {
        return refuse(StatusCode::CONFLICT, "Market is closed — manual buys run 09:15–15:30 IST");
    }
    if !execute::feed_fresh() {
        return refuse(StatusCode::CONFLICT, "Price feed is stale — not buying blind");
    }
    if mode == "LIVE" && !execute::kotak_ready(&state) {
        return refuse(StatusCode::CONFLICT, "Kotak is not connected");
    }

    let Some((index, chain, record)) = state.execute.find_contract(&state, &req.trading_symbol).await else {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("{} is not a listed index option", req.trading_symbol.trim()),
        );
    };
    let ws_key = record.ws_key();
    let Some(ltp) = execute::price(&state, &ws_key) else {
        return refuse(StatusCode::CONFLICT, format!("No live price for {} yet", record.trading_symbol));
    };
    // The feed being alive says nothing about this contract: its own price
    // must be recent too, or the user is looking at a price that's gone.
    if execute::contract_age_secs(&ws_key).is_none_or(|age| age > execute::MAX_CONTRACT_AGE_SECS) {
        return refuse(
            StatusCode::CONFLICT,
            format!("{}'s price hasn't updated in over {:.0} s — not buying blind", record.trading_symbol, execute::MAX_CONTRACT_AGE_SECS),
        );
    }
    let Some(strike) = record.normalized_strike() else {
        return refuse(StatusCode::BAD_REQUEST, format!("Can't read the strike of {}", record.trading_symbol));
    };
    let label = format!("{index} {strike} {}", record.option_type);
    let now = shared_domain::current_ist_timestamp_string();

    let mut positions = state.positions.write().await;
    // Two positions on one contract can't be told apart at the broker (the
    // Sync and lost-order recovery match by contract), so a manual scalp never
    // shares a contract with a signal position.
    let signal_holds = positions.iter().any(|p| {
        !p.manual
            && !matches!(p.state, TradeState::Closed)
            && p.resolved_order.as_ref().is_some_and(|o| o.trading_symbol == record.trading_symbol)
    });
    if signal_holds {
        return refuse(
            StatusCode::CONFLICT,
            format!("A Telegram-signal position already holds {label} — exit it first or pick another strike"),
        );
    }
    let held = positions.iter_mut().find(|p| {
        p.manual
            && !matches!(p.state, TradeState::Closed)
            && p.resolved_order.as_ref().is_some_and(|o| o.trading_symbol == record.trading_symbol)
    });
    let (position_id, action) = match held {
        Some(p) => {
            let busy = if matches!(p.state, TradeState::WaitingForEntry) {
                Some("its first buy is still being placed")
            } else if p.buy_in_flight() {
                Some("an add is already in flight")
            } else if p.sell_in_flight() {
                Some("it is being sold")
            } else if p.live_halt.is_some() {
                Some("the engine has halted it — check the log")
            } else {
                None
            };
            if let Some(why) = busy {
                return refuse(StatusCode::CONFLICT, format!("Not adding to {label}: {why}"));
            }
            p.manual_add = Some(ManualAdd { lots: req.lots, requested_at: now.clone() });
            (p.id.clone(), "added")
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            positions.push(MonitoredPosition {
                id: id.clone(),
                signal: TradeSignal {
                    instrument_name: index.clone(),
                    strike: Some(strike),
                    option_type: Some(record.option_type.clone()),
                    expiry: Some(chain.expiry.format("%d-%b-%Y").to_string().to_uppercase()),
                    action: "BUY".to_string(),
                    entry_condition: "MARKET".to_string(),
                    entry_price: ltp,
                    entry_price_max: None,
                    // No stop and no target: a manual scalp closes only on the
                    // user's exit, the kill switch, or the expiry square-off.
                    targets: vec![],
                    stop_loss: 0.0,
                    source: "manual".to_string(),
                    signal_id: Some(format!("manual-{id}")),
                    raw_message: Some(format!("Manual BUY {} lot(s) {label} @ MKT (LTP {ltp:.2})", req.lots)),
                    chat: None,
                },
                state: TradeState::WaitingForEntry,
                created_at: now.clone(),
                override_qty: Some(req.lots),
                resolved_order: Some(record.order_template()),
                resolved_expiry: Some(record.expiry_date),
                ws_scrip_key: Some(ws_key),
                tick_size: record.tick_size,
                manual: true,
                ..Default::default()
            });
            (id, "opened")
        }
    };
    let snapshot = positions.clone();
    drop(positions);
    persist_positions_snapshot(&state, &snapshot).await;

    let qty = req.lots * record.lot_size as i32;
    let _ = state.log_tx.send(
        json!({
            "event": "MANUAL_BUY_REQUESTED",
            "instrument": label,
            "qty": qty,
            "ltp": ltp,
            "action": action,
            "mode": mode,
        })
        .to_string(),
    );
    (
        StatusCode::ACCEPTED,
        Json(json!({ "position_id": position_id, "action": action, "symbol": record.trading_symbol, "qty": qty })),
    )
        .into_response()
}

/// `POST /api/execute/exit-all` — exit every manual scalp: open ones are sold
/// at market by the engine, unfilled first buys are abandoned (and their
/// broker order cancelled). Telegram-signal positions are left alone.
pub async fn execute_exit_all_handler(State(state): State<AppState>) -> Response {
    let mut exiting = 0;
    let mut cancelled = 0;
    let mut positions = state.positions.write().await;
    for p in positions.iter_mut().filter(|p| p.manual) {
        match p.state {
            TradeState::Active | TradeState::Target1Hit => {
                if p.force_exit.is_none() {
                    p.force_exit = Some("MANUAL_EXIT_ALL".to_string());
                    p.override_exit_price = None;
                }
                p.manual_add = None;
                exiting += 1;
            }
            TradeState::WaitingForEntry => {
                if p.force_exit.is_none() {
                    p.force_exit = Some("MANUAL_EXIT_ALL".to_string());
                }
                cancelled += 1;
            }
            TradeState::Closed => {}
        }
    }
    let snapshot = positions.clone();
    drop(positions);
    persist_positions_snapshot(&state, &snapshot).await;

    let _ = state.log_tx.send(
        json!({ "event": "MANUAL_EXIT_ALL_REQUESTED", "instrument": "SCALPS", "exiting": exiting, "cancelled": cancelled })
            .to_string(),
    );
    (StatusCode::ACCEPTED, Json(json!({ "exiting": exiting, "cancelled": cancelled }))).into_response()
}
