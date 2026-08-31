//! HTTP server — wires all crates together.
//!
//! # Modules
//! - [`db`]     — SQLite init, WAL, writer task, config loader
//! - [`routes`] — Axum route handlers (portfolio, settings, auth)

mod db;
mod routes;

use std::sync::Arc;

use axum::{routing::{get, post}, Router};
use chrono::{Datelike, Duration as ChronoDuration};
use dashmap::DashMap;
use shared_domain::{DbWriteMessage, TradeSignal, TradingConfig};
use sqlx::sqlite::SqlitePool;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing_subscriber::EnvFilter;
use axum::{
    extract::Request,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::IntoResponse,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub struct RateLimitEntry {
    pub attempts: u32,
    pub window_start: std::time::Instant,
}

#[derive(Deserialize)]
struct TokenPayload {
    exp: u64,
}

/// Resolves the secret used to sign/verify session tokens: the runtime
/// `AUTH_SECRET` env var (an empty value counts as unset, since a blank
/// secret would let anyone forge a valid signature), falling back to
/// whatever was baked in at compile time. `verify_passkey_handler` (signs
/// tokens) and `auth_middleware` (verifies them) both call this — sharing it
/// is what guarantees they can never end up using two different secrets.
pub(crate) fn resolve_auth_secret() -> Option<String> {
    std::env::var("AUTH_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| option_env!("AUTH_SECRET").map(String::from))
        .filter(|s| !s.is_empty())
}

async fn auth_middleware(
    req: Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let path = req.uri().path();
    
    // Allow public routes
    if path == "/api/auth/verify-passkey" || path == "/api/health" || !path.starts_with("/api/") {
        return Ok(next.run(req).await);
    }

    let auth_header = req.headers().get(header::AUTHORIZATION)
        .and_then(|val| val.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
        
    let token = match auth_header {
        Some(t) => t.to_string(),
        None => {
            // Check query string for SSE
            if path == "/api/logs/stream" {
                let query = req.uri().query().unwrap_or("");
                let token_param = query.split('&').find(|p| p.starts_with("token="));
                match token_param {
                    Some(p) => p.trim_start_matches("token=").to_string(),
                    None => return Err(StatusCode::UNAUTHORIZED),
                }
            } else {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    };
    
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    
    let auth_secret = match resolve_auth_secret() {
        Some(s) => s,
        None => {
            tracing::error!("AUTH_SECRET not configured");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let msg = format!("{}.{}", parts[0], parts[1]);
    let mut mac = match HmacSha256::new_from_slice(auth_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    mac.update(msg.as_bytes());
    let expected_sig = mac.finalize().into_bytes();
    let expected_sig_b64 = URL_SAFE_NO_PAD.encode(expected_sig);

    if parts[2] != expected_sig_b64 {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let payload_bytes = match URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::UNAUTHORIZED),
    };
    let payload: TokenPayload = match serde_json::from_slice(&payload_bytes) {
        Ok(p) => p,
        Err(_) => return Err(StatusCode::UNAUTHORIZED),
    };

    let now = shared_domain::now_ist().timestamp() as u64;
    if now > payload.exp {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(req).await)
}

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
    /// IST timestamp of the last successful Scrip Master download — see
    /// `routes::KotakLoginDeps::scrip_loaded_at`.
    pub scrip_loaded_at: Arc<RwLock<Option<chrono::DateTime<chrono::FixedOffset>>>>,
    /// Raw CSV string for frontend download.
    pub raw_scrip_csv: Arc<RwLock<Option<String>>>,
    /// Handle to the currently running WebSocket task, so we can cancel it on re-auth.
    pub ws_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Channel sender to forward dynamic messages to the Node bridge.
    pub ws_tx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
    /// Rate limit map for passkey auth attempts
    pub rate_limit_map: Arc<DashMap<String, RateLimitEntry>>,
    /// Serializes full Kotak login attempts — see `routes::KotakLoginDeps::login_lock`.
    pub kotak_login_lock: Arc<Mutex<()>>,
}

// ---------------------------------------------------------------------------
// Daily Kotak auto-login — one independent scheduled task per trigger
// ---------------------------------------------------------------------------

/// Owned handles for a scheduled auto-login task (unlike `routes::KotakLoginDeps`,
/// which borrows — these tasks are `'static` and outlive `main()`'s stack frame).
#[derive(Clone)]
struct KotakAutoLoginHandles {
    pool: SqlitePool,
    write_tx: mpsc::Sender<DbWriteMessage>,
    log_tx: broadcast::Sender<String>,
    kotak: Arc<Mutex<Option<kotak_client::KotakClient>>>,
    ws_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    prices: Arc<DashMap<String, f64>>,
    positions: Arc<RwLock<Vec<shared_domain::MonitoredPosition>>>,
    scrip_store: Arc<RwLock<Option<trading_engine::ScripStore>>>,
    raw_scrip_csv: Arc<RwLock<Option<String>>>,
    scrip_loaded_at: Arc<RwLock<Option<chrono::DateTime<chrono::FixedOffset>>>>,
    login_lock: Arc<Mutex<()>>,
}

impl KotakAutoLoginHandles {
    fn as_deps(&self) -> routes::KotakLoginDeps<'_> {
        routes::KotakLoginDeps {
            db_pool: &self.pool,
            db_tx: &self.write_tx,
            log_tx: &self.log_tx,
            kotak: &self.kotak,
            ws_task: &self.ws_task,
            ws_tx: &self.ws_tx,
            prices: &self.prices,
            positions: &self.positions,
            scrip_store: &self.scrip_store,
            raw_scrip_csv: &self.raw_scrip_csv,
            scrip_loaded_at: &self.scrip_loaded_at,
            login_lock: &self.login_lock,
        }
    }
}

/// Runs forever, firing `try_env_auto_login` once at `(hour, minute)` IST
/// every weekday. Each trigger is its own independent task — see the call
/// site's comment for why sharing one loop between the two triggers is what
/// this replaced and why that was unsafe.
async fn run_daily_kotak_trigger(handles: KotakAutoLoginHandles, hour: u32, minute: u32, label: &'static str) {
    loop {
        let now = shared_domain::now_ist();
        let today_at = now
            .date_naive()
            .and_hms_opt(hour, minute, 0)
            .expect("valid trigger time")
            .and_local_timezone(shared_domain::ist_offset())
            .single()
            .expect("IST trigger time is unambiguous");
        let next_at = if today_at > now { today_at } else { today_at + ChronoDuration::hours(24) };

        let secs_until = (next_at - now).num_seconds().max(1) as u64;
        tracing::info!(secs = secs_until, trigger = label, "Kotak auto-login scheduled");
        tokio::time::sleep(tokio::time::Duration::from_secs(secs_until)).await;

        let wd = shared_domain::now_ist().weekday();
        let is_weekend = wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun;

        if is_weekend {
            tracing::info!(trigger = label, "Kotak auto-login skipped — weekend");
        } else if handles.kotak.lock().await.is_none() {
            match routes::try_env_auto_login(handles.as_deps(), label).await {
                Ok(()) => tracing::info!(trigger = label, "Kotak auto-login succeeded"),
                Err(e) => tracing::info!(trigger = label, reason = %e, "Kotak auto-login not performed"),
            }
        } else {
            tracing::info!(trigger = label, "Kotak auto-login skipped — already connected");
        }

        // Step past the trigger instant so the next iteration schedules
        // tomorrow's occurrence of this same trigger, not today's again.
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

// ---------------------------------------------------------------------------
// Daily Kotak session clear — shared by the 09:00 and 15:40 IST triggers
// ---------------------------------------------------------------------------

/// Tears down whatever Kotak session currently exists (websocket task, in-memory
/// client, DB row) — a no-op if there isn't one. `reason` is only for logging.
async fn clear_kotak_session(
    kotak: &Arc<Mutex<Option<kotak_client::KotakClient>>>,
    ws_task: &Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ws_tx: &Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    pool: &SqlitePool,
    log_tx: &broadcast::Sender<String>,
    reason: &str,
) {
    if kotak.lock().await.is_some() {
        if let Some(task) = ws_task.lock().await.take() {
            task.abort();
        }
        *ws_tx.lock().await = None;
        *kotak.lock().await = None;
        let _ = sqlx::query("DELETE FROM kotak_session").execute(pool).await;

        tracing::info!(reason, "Kotak session cleared");
        let _ = log_tx.send(format!(
            r#"{{"event":"KOTAK_SESSION_CLEARED","message":"Kotak session cleared ({reason})"}}"#,
        ));
    }
}

/// Runs forever, calling `clear_kotak_session` once at `(hour, minute)` IST
/// every day (every day, not just weekdays — clearing an absent session is a
/// no-op, so there's no reason to special-case weekends here).
async fn run_daily_kotak_clear(
    hour: u32,
    minute: u32,
    reason: &'static str,
    kotak: Arc<Mutex<Option<kotak_client::KotakClient>>>,
    ws_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    pool: SqlitePool,
    log_tx: broadcast::Sender<String>,
) {
    loop {
        let secs_until = {
            let now = shared_domain::now_ist();
            let today_at = now
                .date_naive()
                .and_hms_opt(hour, minute, 0)
                .expect("valid time")
                .and_local_timezone(shared_domain::ist_offset())
                .single()
                .expect("IST time is unambiguous");
            let diff = today_at.signed_duration_since(now);
            if diff.num_seconds() > 0 {
                diff.num_seconds() as u64
            } else {
                (diff + ChronoDuration::hours(24)).num_seconds().max(1) as u64
            }
        };

        tracing::info!(secs = secs_until, reason, "Kotak session clear scheduled");
        tokio::time::sleep(tokio::time::Duration::from_secs(secs_until)).await;

        clear_kotak_session(&kotak, &ws_task, &ws_tx, &pool, &log_tx, reason).await;

        // Step past the trigger instant so the next iteration schedules
        // tomorrow's occurrence, not today's again.
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

/// Broadcast a log line to the live SSE stream *and* persist it to `system_logs`.
///
/// The Scrip Master refresh this file used to run only ever reached the SSE
/// stream, so when it wedged the server on 2026-08-31 it left no trace in the
/// DB at all — the incident had to be reconstructed from nginx and `sar`.
/// Anything scheduled and unattended needs to survive a restart to be worth
/// anything, hence the DB write.
async fn recycle_log(handles: &KotakAutoLoginHandles, level: &str, payload: serde_json::Value) {
    let message = payload.to_string();
    let _ = handles
        .write_tx
        .send(DbWriteMessage::Log { level: level.to_owned(), message: message.clone() })
        .await;
    let _ = handles.log_tx.send(message);
}

/// Runs forever, performing a full Kotak session recycle at `(hour, minute)`
/// IST every weekday: tear the existing session down, then log in fresh —
/// which, through `fetch_scrip_master_background`, also re-downloads the Scrip
/// Master against the new session.
///
/// **Nothing here holds the Kotak mutex across network I/O**, which is the
/// whole point. `clear_kotak_session` takes it only long enough to swap in
/// `None`; the login path logs in against a *local* `KotakClient` and only
/// takes the mutex to store the finished client; and the Scrip Master download
/// runs against a *clone* pulled out of the mutex. The inline 09:10 Scrip
/// Master refresh this replaced did the opposite — it held the mutex for the
/// entire three-segment download, up to ~180s (6 HTTP requests x the client's
/// 30s timeout). Everything needing that mutex blocked for the whole window:
/// `kotak_place` / `kotak_cancel` in the position monitor, plus the
/// `/api/portfolio` and `/api/status` routes. Observed 2026-08-31 09:10 IST as
/// a total dashboard blackout five minutes before market open.
///
/// A failed login is retried rather than abandoned. The point of recycling at
/// 09:10 is to be holding a working session by the 09:15 open, and deleting a
/// good session only to give up on the first re-login failure would leave the
/// account with no session while the market is live. If every attempt fails,
/// the 09:15 auto-login trigger remains as the last backstop, and the failure
/// is written to `system_logs` so it is visible after the fact.
async fn run_daily_kotak_recycle(
    hour: u32,
    minute: u32,
    label: &'static str,
    handles: KotakAutoLoginHandles,
) {
    const MAX_LOGIN_ATTEMPTS: u32 = 3;
    const RETRY_GAP_SECS: u64 = 20;

    loop {
        let secs_until = {
            let now = shared_domain::now_ist();
            let today_at = now
                .date_naive()
                .and_hms_opt(hour, minute, 0)
                .expect("valid time")
                .and_local_timezone(shared_domain::ist_offset())
                .single()
                .expect("IST time is unambiguous");
            let diff = today_at.signed_duration_since(now);
            if diff.num_seconds() > 0 {
                diff.num_seconds() as u64
            } else {
                (diff + ChronoDuration::hours(24)).num_seconds().max(1) as u64
            }
        };

        tracing::info!(secs = secs_until, trigger = label, "Kotak session recycle scheduled");
        tokio::time::sleep(tokio::time::Duration::from_secs(secs_until)).await;

        let wd = shared_domain::now_ist().weekday();
        if wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun {
            tracing::info!(trigger = label, "Kotak session recycle skipped — weekend");
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            continue;
        }

        // Already logged in *and* holding a Scrip Master downloaded today? Then
        // the 09:05 pre-warm already did this work and there is nothing to
        // recycle. Tearing a healthy session down five minutes before the open
        // only to rebuild it is pure downside: every re-login is a chance to end
        // up with no session at all while the market is live. Both halves are
        // required — a live session with a stale/absent Scrip Master can't
        // resolve a signal to a contract, and today's Scrip Master with a dead
        // session can't place the order. So this pass is a no-op on a normal day
        // and a real recovery pass on a bad one.
        let ready_today = {
            let session_live = handles.kotak.lock().await.is_some();
            // Compare *dates*, not instants — the question is "did today's
            // download happen", not "was it this exact microsecond".
            let scrip_today = handles
                .scrip_loaded_at
                .read()
                .await
                .map(|t| t.date_naive())
                == Some(shared_domain::now_ist().date_naive());
            session_live && scrip_today
        };
        if ready_today {
            tracing::info!(trigger = label, "Kotak session recycle skipped — already logged in with today's Scrip Master");
            recycle_log(&handles, "INFO", serde_json::json!({
                "event": "KOTAK_SESSION_RECYCLE_SKIPPED",
                "level": "INFO",
                "source": label,
                "message": "Session recycle skipped — already logged in and holding today's Scrip Master",
            })).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            continue;
        }

        // Never delete a session we have no way to rebuild. With
        // `KOTAK_AUTO_LOGIN=false` the only way back in is a human clicking
        // Connect, so clearing here would strand the account with no session —
        // strictly worse than leaving the existing one in place.
        if !routes::auto_login_enabled_by_env() {
            tracing::warn!(trigger = label, "Kotak session recycle skipped — KOTAK_AUTO_LOGIN=false");
            recycle_log(&handles, "INFO", serde_json::json!({
                "event": "KOTAK_SESSION_RECYCLE_SKIPPED",
                "level": "INFO",
                "source": label,
                "message": "Session recycle skipped — KOTAK_AUTO_LOGIN=false, refusing to clear a session that cannot be rebuilt automatically",
            })).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            continue;
        }

        recycle_log(&handles, "INFO", serde_json::json!({
            "event": "KOTAK_SESSION_RECYCLE",
            "level": "INFO",
            "source": label,
            "message": format!(
                "Recycling Kotak session ({label}) — deleting the login, logging back in, re-downloading Scrip Master"
            ),
        })).await;

        clear_kotak_session(
            &handles.kotak,
            &handles.ws_task,
            &handles.ws_tx,
            &handles.pool,
            &handles.log_tx,
            label,
        )
        .await;

        let mut logged_in = false;
        for attempt in 1..=MAX_LOGIN_ATTEMPTS {
            match routes::try_env_auto_login(handles.as_deps(), label).await {
                Ok(()) => {
                    tracing::info!(trigger = label, attempt, "Kotak recycle login succeeded");
                    logged_in = true;
                    break;
                }
                Err(e) => {
                    tracing::error!(trigger = label, attempt, reason = %e, "Kotak recycle login failed");
                    if attempt < MAX_LOGIN_ATTEMPTS {
                        tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_GAP_SECS)).await;
                    }
                }
            }
        }

        if !logged_in {
            recycle_log(&handles, "ERROR", serde_json::json!({
                "event": "KOTAK_SESSION_RECYCLE_FAILED",
                "level": "ERROR",
                "source": label,
                "message": format!(
                    "Kotak re-login failed after {MAX_LOGIN_ATTEMPTS} attempts — no session. \
                     The 09:15 auto-login trigger is the remaining backstop; until it succeeds \
                     no order can be placed or cancelled."
                ),
            })).await;
        }

        // Step past the trigger instant so the next iteration schedules
        // tomorrow's occurrence, not today's again.
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // 0. Load `.env` if present (never overrides an already-exported var).
    // Silently a no-op when there's no `.env` file — the process environment
    // (systemd EnvironmentFile=, shell exports, etc.) still works as before.
    let _ = dotenvy::dotenv();

    // 1. Tracing — timestamps in IST (UTC+5:30) so server logs are readable
    let ist_timer = tracing_subscriber::fmt::time::OffsetTime::new(
        time::UtcOffset::from_hms(5, 30, 0).expect("valid IST offset"),
        time::format_description::well_known::Rfc3339,
    );
    tracing_subscriber::fmt()
        .with_timer(ist_timer)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Check PASSKEY status
    if let Some(val) = std::env::var("PASSKEY").ok().or_else(|| option_env!("PASSKEY").map(String::from)) {
        tracing::info!("PASSKEY is set (length: {})", val.len());
    } else {
        tracing::warn!("PASSKEY is NOT set (neither in runtime env nor compiled in)!");
    }

    // Check AUTH_SECRET status — without it, auth_middleware 500s every
    // authenticated route (see main.rs auth_middleware), so a missing secret
    // needs to be loud here, not discovered later as a wall of failed requests.
    match resolve_auth_secret() {
        Some(val) => tracing::info!("AUTH_SECRET is set (length: {})", val.len()),
        None => tracing::warn!("AUTH_SECRET is NOT set — every authenticated /api/* route will fail with 500!"),
    }

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
    // Serializes full Kotak login attempts end-to-end — see
    // `routes::KotakLoginDeps::login_lock` for why a plain `is_none()` check
    // before deciding to log in isn't enough on its own.
    let kotak_login_lock = Arc::new(tokio::sync::Mutex::new(()));

    let ws_scrips = std::env::var("KOTAK_SCRIPS").unwrap_or_else(|_| "nse_cm|11536".into());
    let scrip_store = Arc::new(RwLock::new(None));
    let raw_scrip_csv = Arc::new(RwLock::new(None));
    let scrip_loaded_at: Arc<RwLock<Option<chrono::DateTime<chrono::FixedOffset>>>> = Arc::new(RwLock::new(None));

    // 7. Try to restore Kotak session from DB
    if let Some(session) = db::load_kotak_session(&pool).await {
        tracing::info!("Found active Kotak session for today, restoring...");
        if let Ok(mut client) = kotak_client::KotakClient::new(&session.access_token) {
            client.restore_session(session.auth_token.clone(), session.sid.clone(), session.base_url.clone());
            
            // Fetch Scrip Master before starting WebSocket
            tracing::info!("Fetching Scrip Master...");
            let mut store = trading_engine::ScripStore::default();
            let mut raw_sections: Vec<(&str, String)> = Vec::new();
            
            for segment in ["nse_fo", "bse_fo", "nse_cm"] {
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
                *scrip_loaded_at.write().await = Some(shared_domain::now_ist());
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
                    // Seed a 0.0 placeholder so the position monitor doesn't skip
                    // this position on its first tick (it continues when ltp_map
                    // returns None). The real price will overwrite this once the
                    // first live tick arrives from the WebSocket.
                    prices.insert(key.clone(), 0.0);
                    let _ = tx.send(serde_json::json!({"action": "subscribe", "scrips": key}).to_string());
                }
            }
        }
    } else {
        tracing::info!("No valid Kotak session found for today — attempting env-based Kotak auto-login...");
        let deps = routes::KotakLoginDeps {
            db_pool: &pool,
            db_tx: &write_tx,
            log_tx: &log_tx,
            kotak: &kotak_client_opt,
            ws_task: &ws_task,
            ws_tx: &ws_tx,
            prices: &prices,
            positions: &positions,
            scrip_store: &scrip_store,
            scrip_loaded_at: &scrip_loaded_at,
            raw_scrip_csv: &raw_scrip_csv,
            login_lock: &kotak_login_lock,
        };
        match routes::try_env_auto_login(deps, "server startup").await {
            Ok(()) => tracing::info!("Kotak auto-login succeeded at startup"),
            Err(e) => tracing::info!(reason = %e, "Kotak auto-login not performed at startup — bridge startup deferred until login"),
        }
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
        Arc::clone(&kotak_client_opt),
    ));

    // 8b. Daily Kotak auto-login. Covers the case where the server was already
    // running before the open (so the startup attempt above never fired) or
    // wasn't yet connected for some other reason. Every pass is a no-op when a
    // session already exists or env auto-login isn't configured.
    //
    // Two triggers, both on weekdays only:
    //   09:05 — pre-warm. A successful login kicks off the Scrip Master
    //           download, which gates *all* order placement:
    //           `start_position_monitor` discards any signal that arrives
    //           while the store is still empty (no queue, no retry). Logging
    //           in ten minutes early means the store is loaded before the
    //           first tick. The Kotak session is valid for the whole trading
    //           day, so the early login costs nothing.
    //   09:15 — market open (`shared_domain::MARKET_OPEN_HOUR/MINUTE`).
    //           Safety net that only does anything if the pre-warm failed.
    // Each trigger gets its own independent spawned loop rather than one loop
    // picking "whichever comes next" — with a shared loop, a slow 09:05
    // attempt (network retries, the TOTP retry added in try_env_auto_login)
    // could still be running when 09:15 arrives, and since the next sleep is
    // only computed *after* the previous attempt fully finishes, that would
    // push 09:15 past today's occurrence entirely and skip the one trigger
    // whose entire purpose is to catch a failed pre-warm.
    {
        let handles = KotakAutoLoginHandles {
            pool: pool.clone(),
            write_tx: write_tx.clone(),
            log_tx: log_tx.clone(),
            kotak: Arc::clone(&kotak_client_opt),
            ws_task: Arc::clone(&ws_task),
            ws_tx: Arc::clone(&ws_tx),
            prices: Arc::clone(&prices),
            positions: Arc::clone(&positions),
            scrip_store: Arc::clone(&scrip_store),
            scrip_loaded_at: Arc::clone(&scrip_loaded_at),
            raw_scrip_csv: Arc::clone(&raw_scrip_csv),
            login_lock: Arc::clone(&kotak_login_lock),
        };

        tokio::spawn(run_daily_kotak_trigger(handles.clone(), 9, 5, "pre-warm 09:05 IST"));
        // 09:10 — full session recycle: delete the login, log back in, and
        // re-download the Scrip Master against the fresh session. This replaced
        // a standalone Scrip Master refresh that held the Kotak mutex for the
        // whole download; see `run_daily_kotak_recycle`.
        tokio::spawn(run_daily_kotak_recycle(9, 10, "daily recycle 09:10 IST", handles.clone()));
        tokio::spawn(run_daily_kotak_trigger(
            handles,
            shared_domain::MARKET_OPEN_HOUR,
            shared_domain::MARKET_OPEN_MINUTE,
            "market open 09:15 IST",
        ));
    }


    // 9. Daily Kotak session clear — two triggers:
    //   09:00 — five minutes before the 09:05 pre-warm login. Guarantees a
    //           clean slate for that attempt regardless of what's currently
    //           sitting in memory: a session from a manual "Connect" earlier
    //           that night, a leftover from testing, anything — rather than
    //           the 09:05 trigger's `is_none()` check seeing a stale-but-still-
    //           `Some` session and silently skipping the real login for the day.
    //   15:40 — market close. The session token is only ever valid for the
    //           trading day it was issued, so there's no reason to keep the
    //           WebSocket alive or the DB row around once the market has shut.
    tokio::spawn(run_daily_kotak_clear(
        9, 0, "pre pre-warm 09:00 IST",
        Arc::clone(&kotak_client_opt), Arc::clone(&ws_task), Arc::clone(&ws_tx),
        pool.clone(), log_tx.clone(),
    ));
    tokio::spawn(run_daily_kotak_clear(
        15, 40, "market close 15:40 IST",
        Arc::clone(&kotak_client_opt), Arc::clone(&ws_task), Arc::clone(&ws_tx),
        pool.clone(), log_tx.clone(),
    ));

    // 10. Router
    let state = AppState {
        signal_tx,
        log_tx,
        db_pool: pool,
        db_tx: write_tx.clone(),
        trading_cfg,
        prices,
        kotak: kotak_client_opt,
        telegram: Arc::new(Mutex::new(telegram_ingester::TelegramManager::new())),
        positions,
        scrip_store,
        raw_scrip_csv,
        scrip_loaded_at,
        ws_task,
        ws_tx,
        rate_limit_map: Arc::new(DashMap::new()),
        kotak_login_lock,
    };

    let app = Router::new()
        .route("/api/webhook/telegram",            post(routes::webhook_handler))
        .route("/api/logs/stream",                  get(routes::sse_logs_handler))
        .route("/api/logs/history",                 get(routes::logs_history_handler))
        .route("/api/portfolio",                    get(routes::portfolio_handler))
        .route("/api/health",                       get(routes::health_handler))
        .route("/api/positions",                    get(routes::positions_handler))
        .route("/api/prices",                       get(routes::prices_handler))
        .route("/api/scrip/search",                 get(routes::scrip_search_handler))
        .route("/api/scrip/download",               get(routes::scrip_download_handler))
        .route("/api/positions/:id",                axum::routing::delete(routes::delete_position_handler)
                                                   .patch(routes::patch_position_handler))
        .route("/api/positions/:id/close",          post(routes::close_position_handler))
        .route("/api/positions/:id/sell",           post(routes::sell_position_handler))
        .route("/api/positions/reconcile/preview",  get(routes::reconcile_preview_handler))
        .route("/api/positions/reconcile/apply",    post(routes::reconcile_apply_handler))
        .route("/api/settings",                     get(routes::get_settings_handler)
                                                   .post(routes::post_settings_handler))
        .route("/api/settings/clear_database",      post(routes::post_clear_database_handler))
        .route("/api/wallet/balance",               get(routes::get_wallet_balance_handler)
                                                   .post(routes::post_wallet_balance_handler))
        .route("/api/update_server",                post(routes::post_update_server_handler))
        .route("/api/auth/kotak",                  post(routes::kotak_login_handler)
                                                   .get(routes::kotak_status_handler))
        .route("/api/auth/kotak/auto-login",        post(routes::kotak_auto_login_handler))
        .route("/api/auth/kotak/disconnect",        axum::routing::delete(routes::disconnect_kotak))
        .route("/api/auth/reset",                  axum::routing::delete(routes::reset_creds))
        .route("/api/status",                      get(routes::system_status))
        .route("/api/auth/kotak/scrip-master/raw",  get(routes::kotak_scrip_raw_handler))
        .route("/api/auth/kotak/scrip-master/json", get(routes::kotak_scrip_json_handler))
        .route("/api/auth/telegram/request-code",  post(routes::telegram_request_code_handler))
        .route("/api/auth/telegram/submit-code",    post(routes::telegram_submit_code_handler))
        .route("/api/auth/telegram/submit-2fa",     post(routes::telegram_submit_2fa_handler))
        .route("/api/auth/telegram/status",         get(routes::telegram_status_handler))
        .route("/api/auth/telegram/chats",          get(routes::telegram_chats_handler))
        .route("/api/auth/telegram/start",          post(routes::telegram_start_handler))
        .route("/api/auth/telegram/disconnect",     axum::routing::delete(routes::disconnect_telegram))
        .route("/api/auth/verify-passkey",          post(routes::verify_passkey_handler))
        .fallback_service(ServeDir::new("../frontend/dist"))
        .layer(middleware::from_fn(auth_middleware))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = "0.0.0.0:8080";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind :8080");
    tracing::info!("Server listening on http://{addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .expect("server error");
}
