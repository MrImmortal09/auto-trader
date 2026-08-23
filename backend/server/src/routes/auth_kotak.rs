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
    /// True when `totp` was derived from `KOTAK_TOTP_SECRET` rather than
    /// typed in — surfaced in the login logs so it's obvious from the UI
    /// whether a human or the server produced the code.
    totp_auto_generated: bool,
}

/// Broadcast a log line to the live SSE stream *and* persist it to
/// `system_logs`, so the frontend sees login progress whether it was already
/// watching (SSE) or connects afterwards (`/api/logs/history`).
///
/// Payloads are built with `serde_json` rather than `format!` so a broker
/// error string containing quotes can't produce malformed JSON in the UI.
/// Never put the TOTP code, MPIN, access token, or TOTP secret in here.
async fn send_log(
    db_tx: &mpsc::Sender<shared_domain::DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    level: &str,
    payload: serde_json::Value,
) {
    let message = payload.to_string();
    let _ = db_tx
        .send(shared_domain::DbWriteMessage::Log {
            level: level.to_owned(),
            message: message.clone(),
        })
        .await;
    let _ = log_tx.send(message);
}

/// Resolve a (possibly partial) login request against env-var fallbacks.
/// An empty/omitted `totp` triggers auto-generation from
/// `KOTAK_TOTP_SECRET` / `KOTAK_TOTP_HASH`.
fn resolve_kotak_login(req: KotakLoginReq) -> Result<ResolvedKotakLogin, String> {
    // All-or-nothing on the four identity fields: filling in some by hand and
    // leaving others blank would silently pull the blanks from KOTAK_* env
    // vars, which may belong to a different account than the one just typed
    // — a live-money credential-mixing risk, not just a UX rough edge. TOTP
    // is exempt: typing the other four by hand while still wanting the TOTP
    // auto-generated from KOTAK_TOTP_SECRET is a normal, safe workflow.
    let identity_fields = [&req.access_token, &req.mobile_number, &req.ucc, &req.mpin];
    let filled = identity_fields.iter().filter(|f| non_empty((**f).clone()).is_some()).count();
    if filled > 0 && filled < identity_fields.len() {
        return Err(
            "partial login form — fill in all of access token, mobile number, UCC, and MPIN, \
             or leave all four blank to use the configured KOTAK_* env credentials"
                .to_string(),
        );
    }

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

    let (totp, totp_auto_generated) = match non_empty(req.totp) {
        Some(manual) => (manual, false),
        None => {
            let secret = env_var("KOTAK_TOTP_SECRET")
                .or_else(|| env_var("KOTAK_TOTP_HASH"))
                .ok_or("totp missing and no KOTAK_TOTP_SECRET/KOTAK_TOTP_HASH configured for auto-generation")?;
            let code = kotak_client::generate_totp(&secret)
                .map_err(|e| format!("failed to generate TOTP: {e}"))?;
            (code, true)
        }
    };

    Ok(ResolvedKotakLogin { access_token, mobile_number, ucc, mpin, totp, totp_auto_generated })
}

// ---------------------------------------------------------------------------
// Shared login lifecycle — used by the manual login route, the explicit
// auto-login route, server startup, and the scheduled 09:00 IST retry.
// ---------------------------------------------------------------------------

/// Borrowed handles to everything a Kotak login needs to wire up: the client
/// itself, the WebSocket task, and the scrip master store. Exists so
/// `perform_kotak_login` can be called both from route handlers (which have
/// an `AppState`) and from `main.rs` at startup (before `AppState` exists).
#[derive(Clone, Copy)]
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
    /// Serializes full login attempts end-to-end (network round-trips, the
    /// `ws_task` swap, and the final `kotak` assignment) so a manual "Connect"
    /// click racing the scheduled 09:05/09:15 trigger can't interleave with
    /// it — the plain `kotak.lock().await.is_none()` check callers do before
    /// deciding to log in only guards the *decision*, not the login itself.
    pub login_lock: &'a Arc<Mutex<()>>,
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
            login_lock: &state.kotak_login_lock,
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
    source: &str,
) -> Result<(), kotak_client::KotakError> {
    // Held for the whole attempt — see `KotakLoginDeps::login_lock` doc.
    let _login_guard = deps.login_lock.lock().await;

    let ResolvedKotakLogin {
        access_token, mobile_number, ucc, mpin, totp, totp_auto_generated,
    } = resolved;

    let masked_ucc = mask_ucc(&ucc);
    send_log(deps.db_tx, deps.log_tx, "INFO", serde_json::json!({
        "event": "KOTAK_LOGIN_START",
        "level": "INFO",
        "source": source,
        "ucc": masked_ucc,
        "message": format!(
            "Kotak login starting ({source}) — UCC {masked_ucc}, TOTP {}",
            if totp_auto_generated { "auto-generated from KOTAK_TOTP_SECRET" } else { "entered manually" },
        ),
    })).await;

    let mut client = kotak_client::KotakClient::new(&access_token)?;
    let creds = kotak_client::KotakCredentials {
        access_token: access_token.clone(),
        mobile_number,
        ucc,
        totp,
        mpin,
    };

    // Two-step login: TOTP -> MPIN validate. Both legs surface to the UI.
    if let Err(e) = client.login(creds).await {
        send_log(deps.db_tx, deps.log_tx, "ERROR", serde_json::json!({
            "event": "KOTAK_LOGIN_FAILED",
            "level": "ERROR",
            "source": source,
            "message": format!("Kotak login failed ({source}): {e}"),
        })).await;
        return Err(e);
    }

    send_log(deps.db_tx, deps.log_tx, "INFO", serde_json::json!({
        "event": "KOTAK_LOGIN_OK",
        "level": "INFO",
        "source": source,
        "message": "Kotak TOTP + MPIN accepted — trading session established".to_string(),
    })).await;

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
        let mut resubscribed = 0usize;
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
                resubscribed += 1;
            }
        }
        drop(tx_guard);

        send_log(deps.db_tx, deps.log_tx, "INFO", serde_json::json!({
            "event": "KOTAK_WS_START",
            "level": "INFO",
            "message": format!(
                "Session saved — market data feed starting, {resubscribed} open position(s) resubscribed",
            ),
        })).await;
    }

    send_log(deps.db_tx, deps.log_tx, "INFO", serde_json::json!({
        "event": "KOTAK_CONNECTED",
        "level": "INFO",
        "status": "ok",
        "source": source,
        "message": format!("Kotak connected ({source}) — loading Scrip Master next"),
    })).await;
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
    send_log(&db_tx, &log_tx, "INFO", serde_json::json!({
        "event": "SCRIP_FETCH",
        "level": "INFO",
        "message": "Fetching Kotak Scrip Master (nse_fo, bse_fo, nse_cm) — no orders can be placed until this finishes",
    })).await;

    // Clone the client out of the mutex for the duration of the fetch.
    let client_opt = kotak.lock().await.clone();
    let client = match client_opt {
        Some(c) => c,
        None => {
            send_log(&db_tx, &log_tx, "ERROR", serde_json::json!({
                "event": "SCRIP_FETCH_ERROR",
                "level": "ERROR",
                "message": "Kotak client disappeared before scrip fetch",
            })).await;
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
                send_log(&db_tx, &log_tx, "ERROR", serde_json::json!({
                    "event": "SCRIP_FETCH_ERROR",
                    "level": "ERROR",
                    "segment": segment,
                    "message": format!("Failed to fetch {segment} scrip master: {e}"),
                })).await;
            }
        }
    }

    if raw_sections.is_empty() {
        send_log(&db_tx, &log_tx, "ERROR", serde_json::json!({
            "event": "SCRIP_FETCH_ERROR",
            "level": "ERROR",
            "message": "Failed to fetch all scrip master segments — signals cannot be resolved to contracts",
        })).await;
    } else {
        *scrip_store.write().await = Some(store);
        *raw_scrip_csv.write().await = merge_csv_sections(&raw_sections);
        send_log(&db_tx, &log_tx, "INFO", serde_json::json!({
            "event": "SCRIP_FETCH_SUCCESS",
            "level": "INFO",
            "message": "Scrip Master loaded — ready to resolve and place orders",
        })).await;
    }
}

/// Attempt an unattended login purely from env credentials — used at server
/// startup and by the scheduled 09:00 IST retry. Returns `Err` (never
/// panics) when auto-login is disabled, credentials are incomplete, or the
/// login itself fails, so callers can just log the reason and move on.
pub async fn try_env_auto_login(deps: KotakLoginDeps<'_>, source: &str) -> Result<(), String> {
    if !auto_login_enabled_by_env() {
        let reason = "KOTAK_AUTO_LOGIN=false".to_string();
        send_log(deps.db_tx, deps.log_tx, "INFO", serde_json::json!({
            "event": "KOTAK_AUTO_LOGIN_SKIPPED",
            "level": "INFO",
            "source": source,
            "message": format!("Kotak auto-login skipped ({source}): {reason}"),
        })).await;
        return Err(reason);
    }

    // Auto-login uses a TOTP freshly generated from KOTAK_TOTP_SECRET, so a
    // failure right at a 30s window boundary (code generated a moment before
    // Kotak's side rolls the counter over) gets one retry with a newly
    // generated code before giving up — never an unbounded retry loop, and
    // no human is present to intervene here (this runs unattended at startup
    // and on the 09:05/09:15 schedule), so it must fail loudly rather than
    // hang or retry forever.
    const MAX_ATTEMPTS: u32 = 2;
    let mut last_err = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let resolved = match resolve_kotak_login(KotakLoginReq::default()) {
            Ok(r) => r,
            Err(reason) => {
                // Missing/incomplete env credentials — deterministic across
                // attempts (nothing here is TOTP-timing-related), and not an
                // error condition: a deployment with no auto-login
                // credentials configured is expected to log in from the UI.
                send_log(deps.db_tx, deps.log_tx, "INFO", serde_json::json!({
                    "event": "KOTAK_AUTO_LOGIN_SKIPPED",
                    "level": "INFO",
                    "source": source,
                    "message": format!("Kotak auto-login skipped ({source}): {reason}"),
                })).await;
                return Err(reason);
            }
        };

        match perform_kotak_login(deps, resolved, source).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                if attempt < MAX_ATTEMPTS {
                    tracing::warn!(
                        source, attempt, error = %last_err,
                        "Kotak auto-login attempt failed — retrying once with a freshly-generated TOTP code"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    send_log(deps.db_tx, deps.log_tx, "ERROR", serde_json::json!({
        "event": "KOTAK_AUTO_LOGIN_FAILED",
        "level": "ERROR",
        "source": source,
        "message": format!(
            "Kotak auto sign-in failed after {MAX_ATTEMPTS} attempts ({source}): {last_err} — sign in manually from the dashboard"
        ),
    })).await;
    tracing::error!(source, attempts = MAX_ATTEMPTS, error = %last_err, "Kotak auto sign-in failed — manual sign-in required");

    Err(last_err)
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

    match perform_kotak_login(KotakLoginDeps::from_state(&state), resolved, "manual login").await {
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

    match perform_kotak_login(KotakLoginDeps::from_state(&state), resolved, "Auto Connect button").await {
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
    let auto_login_enabled = auto_login_enabled_by_env();

    // Single source of truth for "will unattended auto-login actually run
    // today" — every credential present *and* not explicitly disabled. The
    // frontend shouldn't have to re-derive this from the individual flags.
    let auto_login_ready = has_env_credentials && auto_login_enabled;
    let auto_login_reason = if !auto_login_enabled {
        Some("KOTAK_AUTO_LOGIN=false".to_string())
    } else if !has_env_credentials {
        Some(
            "one or more of KOTAK_ACCESS_TOKEN, KOTAK_MOBILE_NUMBER, KOTAK_UCC, KOTAK_MPIN, \
             KOTAK_TOTP_SECRET (or KOTAK_TOTP_HASH) is not set"
                .to_string(),
        )
    } else {
        None
    };

    Json(serde_json::json!({
        "connected": state.kotak.lock().await.is_some(),
        "has_env_credentials": has_env_credentials,
        "has_totp_secret": has_totp_secret,
        "auto_login_enabled": auto_login_enabled,
        "auto_login_ready": auto_login_ready,
        "auto_login_reason": auto_login_reason,
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
