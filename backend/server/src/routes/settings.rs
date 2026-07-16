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
