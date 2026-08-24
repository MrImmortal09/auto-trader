use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use shared_domain::{TradeState, TradingConfig};
use serde::Deserialize;

use crate::routes::positions::persist_positions_snapshot;
use crate::AppState;

#[derive(Deserialize)]
pub struct WalletBalanceReq {
    pub balance: f64,
}

/// `GET /api/settings`
pub async fn get_settings_handler(State(state): State<AppState>) -> Json<TradingConfig> {
    Json(state.trading_cfg.read().await.clone())
}

/// `POST /api/settings` — persist to SQLite and update in-memory config.
pub async fn post_settings_handler(
    State(state): State<AppState>,
    Json(mut cfg): Json<TradingConfig>,
) -> impl IntoResponse {
    // Same floor `lots_for_instrument` applies at read time, but enforced here
    // too so the DB row, the in-memory config, and what GET echoes back never
    // disagree with what an order actually sizes to. Also drops keys outside
    // the known index list (typos, a stale symbol) and normalizes casing —
    // `lots_for_instrument` only ever looks up the uppercase `INDEX_NAMES`.
    cfg.index_lots_by_symbol = cfg
        .index_lots_by_symbol
        .into_iter()
        .filter_map(|(k, v)| {
            let upper = k.to_uppercase();
            let idx = shared_domain::INDEX_NAMES.iter().find(|&&n| n == upper)?;
            (v > 0).then(|| (idx.to_string(), v))
        })
        .collect();
    // Same reasoning: keep the in-memory copy written below in lockstep with
    // what's actually bound to SQL a few lines down, instead of only clamping
    // at the DB-bind call site.
    cfg.index_lots = cfg.index_lots.max(1);
    cfg.other_lots = cfg.other_lots.max(1);
    // Clamped to [0, 1]: at 1.0 the trailed stop on the first rung sits at
    // exactly the entry price (breakeven); anything above that would trail
    // the stop below entry, i.e. accept a loss on a position that already
    // hit target 1. See the field's doc comment in shared_domain.
    cfg.dynamic_targeting_trail_factor = cfg.dynamic_targeting_trail_factor.clamp(0.0, 1.0);
    // Clamped strictly positive: at 0 (or below) the next rung would sit at
    // or below the one just hit, so a dynamic runner would re-trigger every
    // tick instead of climbing. Capped at 5x mainly to catch a fat-fingered
    // value, not for safety — an oversized value just means a runner sits
    // un-trailed for longer while waiting for the next (very distant) rung.
    cfg.dynamic_targeting_extension_factor = cfg.dynamic_targeting_extension_factor.clamp(0.05, 5.0);

    let index_lots_by_symbol_json = serde_json::to_string(&cfg.index_lots_by_symbol)
        .unwrap_or_else(|_| "{}".to_string());

    if let Err(e) = sqlx::query(
        "UPDATE trading_config
         SET max_trade_amount_inr=?, index_lots=?, other_lots=?, mode=?, brokerage_per_order=?,
             target_1_exit_pct=?, target_2_exit_pct=?, entry_market_protection=?, dynamic_targeting=?,
             index_lots_by_symbol=?, dynamic_targeting_trail_factor=?, dynamic_targeting_extension_factor=?
         WHERE id=1",
    )
    .bind(cfg.max_trade_amount_inr)
    .bind(cfg.index_lots)
    .bind(cfg.other_lots)
    .bind(&cfg.mode)
    .bind(cfg.brokerage_per_order)
    .bind(cfg.target_1_exit_pct)
    .bind(cfg.target_2_exit_pct)
    .bind(cfg.entry_market_protection)
    .bind(cfg.dynamic_targeting)
    .bind(&index_lots_by_symbol_json)
    .bind(cfg.dynamic_targeting_trail_factor)
    .bind(cfg.dynamic_targeting_extension_factor)
    .execute(&state.db_pool)
    .await
    {
        tracing::error!("persist TradingConfig: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    *state.trading_cfg.write().await = cfg.clone();
    recompute_open_dynamic_runners(&state, &cfg).await;

    let _ = state.log_tx.send(format!(
        r#"{{"event":"CONFIG_UPDATED","mode":"{}","max_trade":{:.2},"index_lots":{},"other_lots":{}}}"#,
        cfg.mode, cfg.max_trade_amount_inr, cfg.index_lots, cfg.other_lots
    ));
    tracing::info!(mode = %cfg.mode, max_trade = cfg.max_trade_amount_inr, index_lots = cfg.index_lots, other_lots = cfg.other_lots, "Config updated");
    StatusCode::OK
}

/// Re-derive `current_sl` and `next_dynamic_target` for every open
/// `Target1Hit` dynamic-targeting position against the just-saved factors,
/// so a settings change takes effect immediately instead of only at the next
/// rung hit. Bounded the same way `decide_live` is — `current_sl` always
/// lands in `[last_dynamic_rung - diff, last_dynamic_rung]`, so this can
/// never trail a stop below the position's entry price. No-op for positions
/// with `last_dynamic_rung: None` (dynamic targeting was off when target 1
/// hit, or target 1 hasn't hit yet) — those keep whatever the fixed-target-2
/// path already gave them.
async fn recompute_open_dynamic_runners(state: &AppState, cfg: &TradingConfig) {
    let mut positions = state.positions.write().await;
    let mut changed = false;
    for p in positions.iter_mut() {
        if !matches!(p.state, TradeState::Target1Hit) {
            continue;
        }
        let (Some(rung), Some(t1)) = (p.last_dynamic_rung, p.signal.targets.first().copied()) else {
            continue;
        };
        let diff = t1 - p.avg_buy_price;
        p.next_dynamic_target = Some(rung + diff * cfg.dynamic_targeting_extension_factor);
        p.current_sl = trading_engine::round_down_tick(
            rung - diff * cfg.dynamic_targeting_trail_factor,
            p.tick_size,
        );
        changed = true;
    }
    if changed {
        let snapshot = positions.clone();
        drop(positions);
        persist_positions_snapshot(state, &snapshot).await;
        tracing::info!("Recomputed dynamic-targeting SL/next-target for open runners after settings change");
    }
}

/// `GET /api/wallet/balance`
pub async fn get_wallet_balance_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let balance: f64 = sqlx::query_scalar("SELECT balance FROM wallet WHERE id = 1")
        .fetch_one(&state.db_pool)
        .await
        .map_err(|e| {
            tracing::error!("get wallet balance: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(serde_json::json!({ "balance": balance })))
}

/// `POST /api/wallet/balance` — set virtual wallet balance.
pub async fn post_wallet_balance_handler(
    State(state): State<AppState>,
    Json(req): Json<WalletBalanceReq>,
) -> impl IntoResponse {
    if req.balance.is_sign_negative() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "balance must be >= 0"}))).into_response();
    }

    if let Err(e) = sqlx::query("UPDATE wallet SET balance = ? WHERE id = 1")
        .bind(req.balance)
        .execute(&state.db_pool)
        .await
    {
        tracing::error!("set wallet balance: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "failed to update balance"}))).into_response();
    }

    let _ = state.log_tx.send(format!(
        r#"{{"event":"WALLET_BALANCE_UPDATED","balance":{:.2}}}"#,
        req.balance
    ));

    (StatusCode::OK, Json(serde_json::json!({ "balance": req.balance }))).into_response()
}

/// `POST /api/settings/clear_database` — clear logs, trades, and positions.
pub async fn post_clear_database_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let mut tx = match state.db_pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!("Failed to begin tx for clear_db: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    
    let res1 = sqlx::query("DELETE FROM system_logs").execute(&mut *tx).await;
    let res2 = sqlx::query("DELETE FROM paper_trades").execute(&mut *tx).await;
    let res3 = sqlx::query("UPDATE open_positions SET json = '[]' WHERE id = 1").execute(&mut *tx).await;
    
    if res1.is_err() || res2.is_err() || res3.is_err() {
        let _ = tx.rollback().await;
        tracing::error!("Failed to clear database tables");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    
    if let Err(e) = tx.commit().await {
        tracing::error!("Failed to commit clear_db tx: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    // Also update in-memory positions
    *state.positions.write().await = vec![];
    
    let _ = state.log_tx.send(r#"{"event":"DATABASE_CLEARED","message":"Database cleared successfully"}"#.to_string());
    tracing::info!("Database tables cleared");
    
    StatusCode::OK
}

/// `POST /api/update_server` — Disconnect websockets and trigger server update script.
pub async fn post_update_server_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::info!("Server update requested. Disconnecting services...");
    
    // 1. Disconnect Kotak
    if let Some(task) = state.ws_task.lock().await.take() {
        task.abort();
    }
    *state.ws_tx.lock().await = None;
    *state.kotak.lock().await = None;
    let _ = sqlx::query("DELETE FROM kotak_session").execute(&state.db_pool).await;

    // 2. Disconnect Telegram
    {
        let mut mgr = state.telegram.lock().await;
        *mgr = telegram_ingester::TelegramManager::new();
    }
    let _ = std::fs::remove_file("session.json");

    // 3. Trigger update.sh in a NEW session (setsid) so it survives this server
    //    being stopped — the script sends C-c to our own tmux pane. The binary
    //    runs from ~/auto-trader/backend, so the script lives at ./server/update.sh.
    //    Use an absolute HOME-based path so this does not depend on the CWD.
    tracing::info!("Spawning update.sh...");
    let script_path = match std::env::var("HOME") {
        Ok(home) => format!("{home}/auto-trader/backend/server/update.sh"),
        Err(_) => "./server/update.sh".to_string(),
    };
    if let Err(e) = std::process::Command::new("setsid")
        .arg("bash")
        .arg(&script_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::error!("Failed to spawn update.sh ({script_path}): {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Failed to trigger update"}))).into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "updating"}))).into_response()
}
