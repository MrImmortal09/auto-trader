use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;
use shared_domain::{current_ist_timestamp_string, DbWriteMessage, TradeState, TradeTag};
use trading_engine::{Exchange, FeeCalculator};

use crate::routes::positions::persist_positions_snapshot;
use crate::AppState;

/// `POST /api/kill-switch` — Engages the emergency kill switch:
/// 1. Flips in-memory `kill_switch_active = true`.
/// 2. Sets `force_exit = Some("KILL_SWITCH")` on all open positions (safely canceling
///    resting stops before selling in LIVE, or booking at LTP in PAPER).
/// 3. Cancels all `WaitingForEntry` positions (canceling resting entry orders at broker in LIVE).
pub async fn post_kill_switch_handler(State(state): State<AppState>) -> impl IntoResponse {
    // 1. Activate kill switch flag in memory
    {
        let mut cfg = state.trading_cfg.write().await;
        cfg.kill_switch_active = true;
    }

    let mode = state.trading_cfg.read().await.mode.clone();
    let brokerage = state.trading_cfg.read().await.brokerage_per_order;

    let mut open_liquidated = 0;
    let mut waiting_abandoned = 0;

    // 2. Process all positions
    let mut positions = state.positions.write().await;

    for pos in positions.iter_mut() {
        match pos.state {
            TradeState::Active | TradeState::Target1Hit => {
                pos.force_exit = Some("KILL_SWITCH".to_string());
                open_liquidated += 1;

                if mode == "PAPER" && pos.executed_qty > 0 {
                    let instrument = pos.signal.instrument_name.clone();
                    let qty = pos.executed_qty;
                    let is_options = pos.signal.option_type.is_some();

                    let ltp = pos
                        .ws_scrip_key
                        .as_ref()
                        .and_then(|k| state.prices.get(k).map(|v| *v))
                        .or_else(|| state.prices.get(&instrument).map(|v| *v))
                        .filter(|p| *p > 0.0)
                        .unwrap_or(pos.avg_buy_price);

                    let fees = FeeCalculator::calculate_on(
                        Exchange::for_underlying(&instrument), qty, ltp, "SELL", is_options, brokerage,
                    );
                    let timestamp = current_ist_timestamp_string();

                    let tag = TradeTag::of(&pos.signal);
                    let _ = sqlx::query(
                        "INSERT INTO paper_trades
                         (ticker, action, qty, executed_price, timestamp,
                          gross_value, brokerage, stt_charge, sebi_fee,
                          stamp_duty, transaction_charge, gst, net_value,
                          signal_id, raw_message, exit_reason, mode,
                          chat_id, chat_title, option_type, strike)
                         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    )
                    .bind(&instrument)
                    .bind("SELL")
                    .bind(qty as i64)
                    .bind(ltp)
                    .bind(&timestamp)
                    .bind(fees.gross_value)
                    .bind(fees.brokerage)
                    .bind(fees.stt_charge)
                    .bind(fees.sebi_fee)
                    .bind(fees.stamp_duty)
                    .bind(fees.transaction_charge)
                    .bind(fees.gst)
                    .bind(fees.net_value)
                    .bind(&tag.signal_id)
                    .bind(&tag.raw_message)
                    .bind("KILL_SWITCH")
                    .bind("PAPER")
                    .bind(tag.chat_id)
                    .bind(&tag.chat_title)
                    .bind(&tag.option_type)
                    .bind(tag.strike)
                    .execute(&state.db_pool)
                    .await;

                    pos.executed_qty = 0;
                    pos.state = TradeState::Closed;
                }
            }
            TradeState::WaitingForEntry => {
                pos.force_exit = Some("KILL_SWITCH".to_string());
                waiting_abandoned += 1;

                if mode == "PAPER" {
                    pos.state = TradeState::Closed;
                }
            }
            TradeState::Closed => {}
        }
    }

    let snapshot = positions.clone();
    drop(positions);
    persist_positions_snapshot(&state, &snapshot).await;

    // 3. Broadcast emergency log event
    let event_msg = json!({
        "event": "KILL_SWITCH_ENGAGED",
        "level": "WARN",
        "mode": mode,
        "liquidated_positions": open_liquidated,
        "abandoned_entries": waiting_abandoned,
        "message": format!(
            "EMERGENCY KILL SWITCH ENGAGED: Liquidated {open_liquidated} position(s), cancelled {waiting_abandoned} entry(ies). All future entries blocked."
        )
    });

    let _ = state
        .db_tx
        .send(DbWriteMessage::Log {
            level: "WARN".to_string(),
            message: event_msg.to_string(),
        })
        .await;
    let _ = state.log_tx.send(event_msg.to_string());

    tracing::warn!(
        mode = %mode,
        liquidated = open_liquidated,
        abandoned = waiting_abandoned,
        "Emergency Kill Switch engaged"
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "kill_switch_engaged",
            "kill_switch_active": true,
            "mode": mode,
            "liquidated_positions": open_liquidated,
            "abandoned_entries": waiting_abandoned,
        })),
    )
}

/// `POST /api/kill-switch/reset` — Disengages the kill switch to allow trading again.
pub async fn post_kill_switch_reset_handler(State(state): State<AppState>) -> impl IntoResponse {
    {
        let mut cfg = state.trading_cfg.write().await;
        cfg.kill_switch_active = false;
    }

    let event_msg = json!({
        "event": "KILL_SWITCH_RESET",
        "level": "INFO",
        "message": "Kill switch disengaged — trading resumed."
    });

    let _ = state
        .db_tx
        .send(DbWriteMessage::Log {
            level: "INFO".to_string(),
            message: event_msg.to_string(),
        })
        .await;
    let _ = state.log_tx.send(event_msg.to_string());

    tracing::info!("Kill switch disengaged");

    (
        StatusCode::OK,
        Json(json!({
            "status": "kill_switch_reset",
            "kill_switch_active": false,
        })),
    )
}

/// `GET /api/kill-switch/status` — Returns the current kill switch state.
pub async fn get_kill_switch_status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let active = state.trading_cfg.read().await.kill_switch_active;
    Json(json!({
        "kill_switch_active": active,
    }))
}
