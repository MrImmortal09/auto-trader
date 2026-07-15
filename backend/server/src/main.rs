//! HTTP server — wires all crates together.
//!
//! # Modules
//! - [`db`]     — SQLite init, WAL, writer task, config loader
//! - [`routes`] — Axum route handlers (portfolio, settings, auth)

mod db;
mod routes;

use std::sync::Arc;

use axum::{routing::{get, post}, Router};
use dashmap::DashMap;
use shared_domain::{DbWriteMessage, TradeSignal, TradingConfig};
use sqlx::sqlite::SqlitePool;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct AppState {
    pub signal_tx:   broadcast::Sender<TradeSignal>,
    pub log_tx:      broadcast::Sender<String>,
    pub db_pool:     SqlitePool,
    pub db_tx:       mpsc::Sender<DbWriteMessage>,
    /// Runtime trading config — updated by POST /api/settings.
    pub trading_cfg: Arc<RwLock<TradingConfig>>,
    /// Live price map shared with the position monitor.
    pub prices:      Arc<DashMap<String, f64>>,
    /// Authenticated Kotak client (None until POST /api/auth/kotak).
    pub kotak:       Arc<Mutex<Option<kotak_client::KotakClient>>>,
    /// Telegram step-by-step auth manager.
    pub telegram:    Arc<Mutex<telegram_ingester::TelegramManager>>,
    /// In-memory queue of upcoming and active positions.
    pub positions:   Arc<RwLock<Vec<shared_domain::MonitoredPosition>>>,
    /// Kotak Scrip Master loaded dynamically after login.
    pub scrip_store: Arc<RwLock<Option<trading_engine::ScripStore>>>,
    /// Raw CSV string for frontend download.
    pub raw_scrip_csv: Arc<RwLock<Option<String>>>,
    /// Handle to the currently running WebSocket task, so we can cancel it on re-auth.
    pub ws_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Channel sender to forward dynamic messages to the Node bridge.
    pub ws_tx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // 1. Tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // 2. SQLite
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://trades.db".into());
    let pool = db::init_db(&db_url).await;
    tracing::info!("SQLite ready at {db_url}");

    // 3. Load TradingConfig from DB
    let trading_cfg = Arc::new(RwLock::new(db::load_config_from_db(&pool).await));
    tracing::info!(mode = %trading_cfg.read().await.mode, "TradingConfig loaded");

    // 4. Shared state
    let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
    let restored_positions = db::load_open_positions(&pool).await;
    let positions = Arc::new(RwLock::new(restored_positions));
    let (signal_tx, signal_rx) = broadcast::channel::<TradeSignal>(100);
    let (log_tx, _)            = broadcast::channel::<String>(1000);
    let (write_tx, write_rx)   = mpsc::channel::<DbWriteMessage>(1000);

    // 5. DB writer task
    tokio::spawn(db::db_writer(write_rx, pool.clone()));

    // 6. Telegram ingester (optional — requires TELEGRAM_API_ID env var)
    if let Ok(raw_id) = std::env::var("TELEGRAM_API_ID") {
        let api_id: i32 = raw_id.parse().expect("TELEGRAM_API_ID must be i32");
        let api_hash    = std::env::var("TELEGRAM_API_HASH")
            .expect("TELEGRAM_API_HASH required when TELEGRAM_API_ID is set");
        let chat_ids: Vec<i64> = std::env::var("TELEGRAM_CHAT_IDS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        tokio::spawn(telegram_ingester::start_ingester_loop(
            api_id, api_hash, chat_ids, signal_tx.clone(), Some(log_tx.clone()),
        ));
    } else {
        tracing::info!("TELEGRAM_API_ID not set — ingester disabled (use /api/auth/telegram)");
    }

    let kotak_client_opt = Arc::new(tokio::sync::Mutex::new(None));
    let ws_task = Arc::new(tokio::sync::Mutex::new(None));
    let ws_tx = Arc::new(tokio::sync::Mutex::new(None));
    
    let ws_scrips = std::env::var("KOTAK_SCRIPS").unwrap_or_else(|_| "nse_cm|11536".into());
    let scrip_store = Arc::new(RwLock::new(None));
    let raw_scrip_csv = Arc::new(RwLock::new(None));

    // 7. Try to restore Kotak session from DB
    if let Some(session) = db::load_kotak_session(&pool).await {
        tracing::info!("Found active Kotak session for today, restoring...");
        if let Ok(mut client) = kotak_client::KotakClient::new(&session.access_token) {
            client.restore_session(session.auth_token.clone(), session.sid.clone(), session.base_url.clone());
            
            // Fetch Scrip Master before starting WebSocket
            tracing::info!("Fetching Scrip Master...");
            let mut store = trading_engine::ScripStore::default();
            let mut raw_sections: Vec<(&str, String)> = Vec::new();
            
            for segment in ["nse_fo", "bse_fo"] {
                if let Ok(csv) = client.get_scrip_master_csv(segment).await {
                    store.merge(trading_engine::ScripStore::parse_csv(&csv, segment));
                    raw_sections.push((segment, csv));
                }
            }
            
            if !raw_sections.is_empty() {
                *scrip_store.write().await = Some(store);
                
                // Combine raw CSVs (simplified for startup)
                let mut combined = String::new();
                for (i, (_, csv)) in raw_sections.iter().enumerate() {
                    let mut lines = csv.lines();
                    if let Some(header) = lines.next() {
                        if i == 0 { combined.push_str(header); combined.push('\n'); }
                        for line in lines {
                            if !line.trim().is_empty() { combined.push_str(line); combined.push('\n'); }
                        }
                    }
                }
                *raw_scrip_csv.write().await = Some(combined);
                tracing::info!("Scrip Master fetched successfully.");
            }

            *kotak_client_opt.lock().await = Some(client);

            // Start WebSocket
            let (initial_ws_tx, ws_rx) = mpsc::unbounded_channel::<String>();
            let ws_handle = tokio::spawn(kotak_client::start_market_data_stream(
                session.auth_token, session.sid, ws_scrips, 1, Arc::clone(&prices), ws_rx,
            ));
            *ws_task.lock().await = Some(ws_handle);
            
            let mut tx_guard = ws_tx.lock().await;
            *tx_guard = Some(initial_ws_tx);
            if let Some(tx) = tx_guard.as_ref() {
                let keys: Vec<String> = positions
                    .read()
                    .await
                    .iter()
                    .filter_map(|p| p.ws_scrip_key.clone())
                    .collect();
                for key in keys {
                    let _ = tx.send(serde_json::json!({"action": "subscribe", "scrips": key}).to_string());
                }
            }
        }
    } else {
        tracing::info!("No valid Kotak session found for today — bridge startup deferred until login");
    }


    // 8. Position Monitor
    tracing::info!(mode = %trading_cfg.read().await.mode, "Starting position monitor");
    let monitor_write_tx = write_tx.clone();
    tokio::spawn(trading_engine::start_position_monitor(
        signal_rx,
        monitor_write_tx,
        Arc::clone(&prices),
        Arc::clone(&trading_cfg),
        Arc::clone(&positions),
        log_tx.clone(),
        Arc::clone(&scrip_store),
        Arc::clone(&ws_tx),
    ));

    // 9. Router
    let state = AppState {
        signal_tx,
        log_tx,
        db_pool: pool,
        db_tx: write_tx.clone(),
        trading_cfg,
        prices,
        kotak: Arc::new(Mutex::new(None)),
        telegram: Arc::new(Mutex::new(telegram_ingester::TelegramManager::new())),
        positions,
        scrip_store,
        raw_scrip_csv,
        ws_task,
        ws_tx,
    };

    let app = Router::new()
        .route("/api/webhook/telegram",            post(routes::webhook_handler))
        .route("/api/logs/stream",                  get(routes::sse_logs_handler))
        .route("/api/portfolio",                    get(routes::portfolio_handler))
        .route("/api/health",                       get(routes::health_handler))
        .route("/api/positions",                    get(routes::positions_handler))
        .route("/api/prices",                       get(routes::prices_handler))
        .route("/api/scrip/search",                 get(routes::scrip_search_handler))
        .route("/api/scrip/download",               get(routes::scrip_download_handler))
        .route("/api/positions/:id",                axum::routing::delete(routes::delete_position_handler)
                                                   .patch(routes::patch_position_handler))
        .route("/api/positions/:id/close",          post(routes::close_position_handler))
        .route("/api/settings",                     get(routes::get_settings_handler)
                                                   .post(routes::post_settings_handler))
        .route("/api/wallet/balance",               get(routes::get_wallet_balance_handler)
                               .post(routes::post_wallet_balance_handler))
        .route("/api/auth/kotak",                  post(routes::kotak_login_handler)
                                                   .get(routes::kotak_status_handler))
        .route("/api/auth/kotak/scrip-master/raw",  get(routes::kotak_scrip_raw_handler))
        .route("/api/auth/kotak/scrip-master/json", get(routes::kotak_scrip_json_handler))
        .route("/api/auth/telegram/request-code",  post(routes::telegram_request_code_handler))
        .route("/api/auth/telegram/submit-code",    post(routes::telegram_submit_code_handler))
        .route("/api/auth/telegram/submit-2fa",     post(routes::telegram_submit_2fa_handler))
        .route("/api/auth/telegram/status",         get(routes::telegram_status_handler))
        .route("/api/auth/telegram/chats",          get(routes::telegram_chats_handler))
        .route("/api/auth/telegram/start",          post(routes::telegram_start_handler))
        .fallback_service(ServeDir::new("../frontend/dist"))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = "0.0.0.0:8080";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind :8080");
    tracing::info!("Server listening on http://{addr}");
    axum::serve(listener, app).await.expect("server error");
}
