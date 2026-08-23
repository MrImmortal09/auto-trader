use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

use crate::AppState;

// ---------------------------------------------------------------------------
// Env credential resolution (manual request fields fall back to env vars)
// ---------------------------------------------------------------------------

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn non_empty(opt: Option<String>) -> Option<String> {
    opt.filter(|s| !s.trim().is_empty())
}

/// Whether unattended (startup / scheduled) auto-login is allowed. Defaults
/// to `true` — set `KOTAK_AUTO_LOGIN=false` to require a manual Connect even
/// when all env credentials are present.
fn auto_login_enabled_by_env() -> bool {
    std::env::var("KOTAK_AUTO_LOGIN")
        .map(|v| !v.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// Fully-resolved credentials ready to hand to `KotakClient::login`.
struct ResolvedKotakLogin {
    access_token: String,
    mobile_number: String,
    ucc: String,
    mpin: String,
    totp: String,
}

/// Resolve a (possibly partial) login request against env-var fallbacks.
/// An empty/omitted `totp` triggers auto-generation from
/// `KOTAK_TOTP_SECRET` / `KOTAK_TOTP_HASH`.
fn resolve_kotak_login(req: KotakLoginReq) -> Result<ResolvedKotakLogin, String> {
    let access_token = non_empty(req.access_token)
        .or_else(|| env_var("KOTAK_ACCESS_TOKEN"))
        .ok_or("access_token missing (set it in the request or KOTAK_ACCESS_TOKEN)")?;
    let mobile_number = non_empty(req.mobile_number)
        .or_else(|| env_var("KOTAK_MOBILE_NUMBER"))
        .ok_or("mobile_number missing (set it in the request or KOTAK_MOBILE_NUMBER)")?;
    let ucc = non_empty(req.ucc)
        .or_else(|| env_var("KOTAK_UCC"))
        .ok_or("ucc missing (set it in the request or KOTAK_UCC)")?;
    let mpin = non_empty(req.mpin)
        .or_else(|| env_var("KOTAK_MPIN"))
        .ok_or("mpin missing (set it in the request or KOTAK_MPIN)")?;

    let totp = match non_empty(req.totp) {
        Some(manual) => manual,
        None => {
            let secret = env_var("KOTAK_TOTP_SECRET")
                .or_else(|| env_var("KOTAK_TOTP_HASH"))
                .ok_or("totp missing and no KOTAK_TOTP_SECRET/KOTAK_TOTP_HASH configured for auto-generation")?;
            kotak_client::generate_totp(&secret).map_err(|e| format!("failed to generate TOTP: {e}"))?
        }
    };

    Ok(ResolvedKotakLogin { access_token, mobile_number, ucc, mpin, totp })
}

// ---------------------------------------------------------------------------
// Shared login lifecycle — used by the manual login route, the explicit
// auto-login route, server startup, and the scheduled 09:00 IST retry.
// ---------------------------------------------------------------------------

/// Borrowed handles to everything a Kotak login needs to wire up: the client
/// itself, the WebSocket task, and the scrip master store. Exists so
/// `perform_kotak_login` can be called both from route handlers (which have
/// an `AppState`) and from `main.rs` at startup (before `AppState` exists).
pub struct KotakLoginDeps<'a> {
    pub db_pool: &'a sqlx::SqlitePool,
    pub db_tx: &'a mpsc::Sender<shared_domain::DbWriteMessage>,
    pub log_tx: &'a broadcast::Sender<String>,
    pub kotak: &'a Arc<Mutex<Option<kotak_client::KotakClient>>>,
    pub ws_task: &'a Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub ws_tx: &'a Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    pub prices: &'a Arc<dashmap::DashMap<String, f64>>,
    pub positions: &'a Arc<RwLock<Vec<shared_domain::MonitoredPosition>>>,
    pub scrip_store: &'a Arc<RwLock<Option<trading_engine::ScripStore>>>,
    pub raw_scrip_csv: &'a Arc<RwLock<Option<String>>>,
}

impl<'a> KotakLoginDeps<'a> {
    pub fn from_state(state: &'a AppState) -> Self {
        Self {
            db_pool: &state.db_pool,
            db_tx: &state.db_tx,
            log_tx: &state.log_tx,
            kotak: &state.kotak,
            ws_task: &state.ws_task,
            ws_tx: &state.ws_tx,
            prices: &state.prices,
            positions: &state.positions,
            scrip_store: &state.scrip_store,
            raw_scrip_csv: &state.raw_scrip_csv,
        }
    }
}

/// Authenticate with Kotak, persist the session, (re)start the HSM
/// WebSocket, resubscribe open positions, and kick off a background Scrip
/// Master refresh. Real money follows a successful call to this function —
/// every caller (manual login, auto-login route, startup, 09:00 IST retry)
/// goes through the same path so none of them can drift from the others.
async fn perform_kotak_login(
    deps: KotakLoginDeps<'_>,
    resolved: ResolvedKotakLogin,
) -> Result<(), kotak_client::KotakError> {
    let ResolvedKotakLogin { access_token, mobile_number, ucc, mpin, totp } = resolved;

    let mut client = kotak_client::KotakClient::new(&access_token)?;
    let creds = kotak_client::KotakCredentials {
        access_token: access_token.clone(),
        mobile_number,
        ucc,
        totp,
        mpin,
    };
    client.login(creds).await?;

    if let Some((auth, sid)) = client.session_credentials() {
        let base_url = client.session.as_ref().map(|s| s.base_url.clone()).unwrap_or_default();
        crate::db::save_kotak_session(deps.db_pool, &access_token, auth, sid, &base_url).await;

        let scrips = std::env::var("KOTAK_SCRIPS").unwrap_or_else(|_| "nse_cm|11536".into());

        // Abort the previous WebSocket task to prevent dual connections.
        let mut ws_guard = deps.ws_task.lock().await;
        if let Some(old_task) = ws_guard.take() {
            old_task.abort();
            tracing::info!("Aborted previous Kotak WebSocket task.");
        }

        let (new_ws_tx, ws_rx) = mpsc::unbounded_channel::<String>();
        let new_handle = tokio::spawn(kotak_client::start_market_data_stream(
            auth.to_owned(), sid.to_owned(), scrips, 1,
            Arc::clone(deps.prices), ws_rx,
        ));
        *ws_guard = Some(new_handle);
        drop(ws_guard);

        let mut tx_guard = deps.ws_tx.lock().await;
        *tx_guard = Some(new_ws_tx);
        if let Some(tx) = tx_guard.as_ref() {
            let keys: Vec<String> = deps
                .positions
                .read()
                .await
                .iter()
                .filter_map(|p| p.ws_scrip_key.clone())
                .collect();

            for key in keys {
                // Seed 0.0 so the position monitor doesn't skip these until the
                // first live tick arrives (same logic as server startup).
                deps.prices.insert(key.clone(), 0.0);
                let payload = serde_json::json!({ "action": "subscribe", "scrips": key });
                let _ = tx.send(payload.to_string());
            }
        }
    }

    let _ = deps.log_tx.send(r#"{"event":"KOTAK_CONNECTED","status":"ok"}"#.into());
    *deps.kotak.lock().await = Some(client.clone());

    // Scrip master download (3 segments, can be slow) runs in the background
    // so callers on the HTTP path don't block long enough to trip a proxy
    // timeout — see the comment this replaced in `kotak_login_handler`.
    tokio::spawn(fetch_scrip_master_background(
        Arc::clone(deps.kotak),
        Arc::clone(deps.scrip_store),
        Arc::clone(deps.raw_scrip_csv),
        deps.db_tx.clone(),
        deps.log_tx.clone(),
    ));

    Ok(())
}

async fn fetch_scrip_master_background(
    kotak: Arc<Mutex<Option<kotak_client::KotakClient>>>,
    scrip_store: Arc<RwLock<Option<trading_engine::ScripStore>>>,
    raw_scrip_csv: Arc<RwLock<Option<String>>>,
    db_tx: mpsc::Sender<shared_domain::DbWriteMessage>,
    log_tx: broadcast::Sender<String>,
) {
    let scrip_start_msg = r#"{"event":"SCRIP_FETCH","message":"Fetching Kotak Scrip Master..."}"#;
    let _ = db_tx.send(shared_domain::DbWriteMessage::Log {
        level: "INFO".into(), message: scrip_start_msg.into(),
    }).await;
    let _ = log_tx.send(scrip_start_msg.into());

    // Clone the client out of the mutex for the duration of the fetch.
    let client_opt = kotak.lock().await.clone();
    let client = match client_opt {
        Some(c) => c,
        None => {
            let err = r#"{"event":"SCRIP_FETCH_ERROR","message":"Kotak client disappeared before scrip fetch"}"#;
            let _ = log_tx.send(err.into());
            return;
        }
    };

    let mut store = trading_engine::ScripStore::default();
    let mut raw_sections: Vec<(&str, String)> = Vec::new();

    for segment in ["nse_fo", "bse_fo", "nse_cm"] {
        match client.get_scrip_master_csv(segment).await {
            Ok(csv) => {
                store.merge(trading_engine::ScripStore::parse_csv(&csv, segment));
                raw_sections.push((segment, csv));
            }
            Err(e) => {
                tracing::error!(segment = %segment, "Failed to fetch Scrip Master: {}", e);
                let err_msg = format!(r#"{{"event":"SCRIP_FETCH_ERROR","message":"Failed to fetch {segment} scrip master: {e}"}}"#);
                let _ = db_tx.send(shared_domain::DbWriteMessage::Log {
                    level: "ERROR".into(), message: err_msg.clone(),
                }).await;
                let _ = log_tx.send(err_msg);
            }
        }
    }

    if raw_sections.is_empty() {
        let err_msg = r#"{"event":"SCRIP_FETCH_ERROR","message":"Failed to fetch all scrip master segments"}"#;
        let _ = db_tx.send(shared_domain::DbWriteMessage::Log {
            level: "ERROR".into(), message: err_msg.into(),
        }).await;
        let _ = log_tx.send(err_msg.into());
    } else {
        *scrip_store.write().await = Some(store);
        *raw_scrip_csv.write().await = merge_csv_sections(&raw_sections);
        let ok_msg = r#"{"event":"SCRIP_FETCH_SUCCESS","message":"Scrip Master loaded successfully"}"#;
        let _ = db_tx.send(shared_domain::DbWriteMessage::Log {
            level: "INFO".into(), message: ok_msg.into(),
        }).await;
        let _ = log_tx.send(ok_msg.into());
    }
}

/// Attempt an unattended login purely from env credentials — used at server
/// startup and by the scheduled 09:00 IST retry. Returns `Err` (never
/// panics) when auto-login is disabled, credentials are incomplete, or the
/// login itself fails, so callers can just log the reason and move on.
pub async fn try_env_auto_login(deps: KotakLoginDeps<'_>) -> Result<(), String> {
    if !auto_login_enabled_by_env() {
        return Err("KOTAK_AUTO_LOGIN=false".into());
    }
    let resolved = resolve_kotak_login(KotakLoginReq::default())?;
    perform_kotak_login(deps, resolved).await.map_err(|e| e.to_string())
}

fn merge_csv_sections(csvs: &[(&str, String)]) -> Option<String> {
    let mut combined = String::new();

    for (index, (segment, csv)) in csvs.iter().enumerate() {
        let mut lines = csv.lines();
        let header = lines.next()?;

        if index == 0 {
            combined.push_str(header);
            combined.push('\n');
        }

        for line in lines {
            if !line.trim().is_empty() {
                combined.push_str(line);
                combined.push('\n');
            }
        }

        tracing::info!(segment = %segment, "Merged scrip master segment");
    }

    Some(combined)
}

#[derive(Deserialize, Default)]
pub struct KotakLoginReq {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub mobile_number: Option<String>,
    #[serde(default)]
    pub ucc: Option<String>,
    /// Optional — if empty/omitted, auto-generated from
    /// `KOTAK_TOTP_SECRET` / `KOTAK_TOTP_HASH`.
    #[serde(default)]
    pub totp: Option<String>,
    #[serde(default)]
    pub mpin: Option<String>,
}

/// `POST /api/auth/kotak` — log in and restart the HSM WebSocket. Any field
/// left empty falls back to its `KOTAK_*` env var; an empty/omitted `totp`
/// is generated from the env TOTP secret.
pub async fn kotak_login_handler(
    State(state): State<AppState>,
    Json(req): Json<KotakLoginReq>,
) -> impl IntoResponse {
    let resolved = match resolve_kotak_login(req) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response(),
    };

    match perform_kotak_login(KotakLoginDeps::from_state(&state), resolved).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "connected"}))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

/// `POST /api/auth/kotak/auto-login` — log in using only `KOTAK_*` env
/// credentials (access token, mobile, UCC, MPIN, TOTP secret), no body
/// required. Lets the frontend offer a one-click "Auto Connect" once the
/// server reports `has_env_credentials: true`.
pub async fn kotak_auto_login_handler(State(state): State<AppState>) -> impl IntoResponse {
    let resolved = match resolve_kotak_login(KotakLoginReq::default()) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response(),
    };

    match perform_kotak_login(KotakLoginDeps::from_state(&state), resolved).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "connected"}))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

/// `GET /api/auth/kotak/scrip-master/raw` — download raw CSV.
pub async fn kotak_scrip_raw_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let raw = state.raw_scrip_csv.read().await;
    match raw.as_ref() {
        Some(csv) => {
            let headers = [
                (axum::http::header::CONTENT_TYPE, "text/csv"),
                (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"scrip_master.csv\""),
            ];
            (StatusCode::OK, headers, csv.clone()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Scrip Master not loaded yet".to_string()).into_response(),
    }
}

/// `GET /api/auth/kotak/scrip-master/json` — return parsed JSON of Scrip Store.
pub async fn kotak_scrip_json_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let store = state.scrip_store.read().await;
    match store.as_ref() {
        Some(s) => (StatusCode::OK, Json(s)).into_response(),
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Scrip Master not loaded yet"}))).into_response(),
    }
}

fn mask_ucc(ucc: &str) -> String {
    let len = ucc.chars().count();
    if len <= 2 {
        return "*".repeat(len);
    }
    let mut chars = ucc.chars();
    let first = chars.next().unwrap();
    let last = ucc.chars().last().unwrap();
    format!("{first}{}{last}", "*".repeat(len - 2))
}

/// `GET /api/auth/kotak` — connection status plus whether env-based
/// auto-login is configured, so the frontend can offer it without exposing
/// any secret values.
pub async fn kotak_status_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let has_totp_secret = env_var("KOTAK_TOTP_SECRET").or_else(|| env_var("KOTAK_TOTP_HASH")).is_some();
    let has_env_credentials = has_totp_secret
        && env_var("KOTAK_ACCESS_TOKEN").is_some()
        && env_var("KOTAK_MOBILE_NUMBER").is_some()
        && env_var("KOTAK_UCC").is_some()
        && env_var("KOTAK_MPIN").is_some();

    Json(serde_json::json!({
        "connected": state.kotak.lock().await.is_some(),
        "has_env_credentials": has_env_credentials,
        "has_totp_secret": has_totp_secret,
        "auto_login_enabled": auto_login_enabled_by_env(),
        "masked_ucc": env_var("KOTAK_UCC").map(|u| mask_ucc(&u)),
    }))
}

// ---------------------------------------------------------------------------
// Reset Credentials
// ---------------------------------------------------------------------------

pub async fn reset_creds(State(state): State<AppState>) -> impl IntoResponse {
    let _ = std::fs::remove_file("session.json");
    let _ = sqlx::query("DELETE FROM kotak_session").execute(&state.db_pool).await;
    
    // Spawn a task to exit after a short delay so the HTTP response goes through
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        std::process::exit(0);
    });
    
    (StatusCode::OK, Json(serde_json::json!({"status": "reset"})))
}

/// `DELETE /api/auth/kotak/disconnect` — drop the Kotak session without restarting.
/// Clears the DB session, kills the WebSocket task, and sets kotak = None in memory.
/// Does NOT clear any frontend fields.
pub async fn disconnect_kotak(State(state): State<AppState>) -> impl IntoResponse {
    // 1. Kill the running WebSocket task
    if let Some(task) = state.ws_task.lock().await.take() {
        task.abort();
        tracing::info!("Kotak WebSocket task aborted on disconnect.");
    }
    // 2. Clear the ws_tx sender so no stale messages are sent
    *state.ws_tx.lock().await = None;
    // 3. Remove the Kotak client from memory
    *state.kotak.lock().await = None;
    // 4. Delete session from DB so it isn't restored on next startup
    let _ = sqlx::query("DELETE FROM kotak_session").execute(&state.db_pool).await;

    tracing::info!("Kotak disconnected via /api/auth/kotak/disconnect");
    (StatusCode::OK, Json(serde_json::json!({"status": "disconnected"})))
}

// ---------------------------------------------------------------------------
// System Status
// ---------------------------------------------------------------------------

pub async fn system_status(State(state): State<AppState>) -> impl IntoResponse {
    let telegram_ok = {
        let t = state.telegram.lock().await;
        t.state == "running"
    };
    let kotak_ok = {
        let k = state.kotak.lock().await;
        k.is_some()
    };
    (StatusCode::OK, Json(serde_json::json!({
        "telegram_connected": telegram_ok,
        "kotak_connected": kotak_ok
    })))
}
