use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use shared_domain::TradingConfig;
use serde::Deserialize;

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
    Json(cfg): Json<TradingConfig>,
) -> impl IntoResponse {
    if let Err(e) = sqlx::query(
        "UPDATE trading_config
         SET max_trade_amount_inr=?, index_lots=?, other_lots=?, mode=?, brokerage_per_order=?,
             target_1_exit_pct=?, target_2_exit_pct=?
         WHERE id=1",
    )
    .bind(cfg.max_trade_amount_inr)
    .bind(cfg.index_lots.max(1))
    .bind(cfg.other_lots.max(1))
    .bind(&cfg.mode)
    .bind(cfg.brokerage_per_order)
    .bind(cfg.target_1_exit_pct)
    .bind(cfg.target_2_exit_pct)
    .execute(&state.db_pool)
    .await
    {
        tracing::error!("persist TradingConfig: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    *state.trading_cfg.write().await = cfg.clone();

    let _ = state.log_tx.send(format!(
        r#"{{"event":"CONFIG_UPDATED","mode":"{}","max_trade":{:.2},"index_lots":{},"other_lots":{}}}"#,
        cfg.mode, cfg.max_trade_amount_inr, cfg.index_lots.max(1), cfg.other_lots.max(1)
    ));
    tracing::info!(mode = %cfg.mode, max_trade = cfg.max_trade_amount_inr, index_lots = cfg.index_lots.max(1), other_lots = cfg.other_lots.max(1), "Config updated");
    StatusCode::OK
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

    // 3. Trigger update.sh detached
    tracing::info!("Spawning update.sh...");
    if let Err(e) = std::process::Command::new("nohup")
        .arg("bash")
        .arg("./update.sh")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::error!("Failed to spawn update.sh: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Failed to trigger update"}))).into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "updating"}))).into_response()
}
