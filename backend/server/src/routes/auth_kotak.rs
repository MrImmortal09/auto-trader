use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::AppState;

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

#[derive(Deserialize)]
pub struct KotakLoginReq {
    pub access_token: String,
    pub mobile_number: String,
    pub ucc: String,
    pub totp: String,
    pub mpin: String,
}

/// `POST /api/auth/kotak` — log in and restart the HSM WebSocket.
pub async fn kotak_login_handler(
    State(state): State<AppState>,
    Json(req): Json<KotakLoginReq>,
) -> impl IntoResponse {
    let mut client = match kotak_client::KotakClient::new(&req.access_token) {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response(),
    };

    let creds = kotak_client::KotakCredentials {
        access_token: req.access_token.clone(),
        mobile_number: req.mobile_number,
        ucc: req.ucc,
        totp: req.totp,
        mpin: req.mpin,
    };

    if let Err(e) = client.login(creds).await {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()}))).into_response();
    }

    // Save session to DB
    if let Some((auth, sid)) = client.session_credentials() {
        let base_url = client.session.as_ref().map(|s| s.base_url.clone()).unwrap_or_default();
        crate::db::save_kotak_session(
            &state.db_pool,
            &req.access_token,
            auth,
            sid,
            &base_url,
        ).await;
    }

    // (Re)start the HSM WebSocket with fresh tokens.
    if let Some((auth, sid)) = client.session_credentials() {
        let scrips = std::env::var("KOTAK_SCRIPS").unwrap_or_else(|_| "nse_cm|11536".into());
        
        // Abort the previous WebSocket task to prevent dual connections
        let mut ws_guard = state.ws_task.lock().await;
        if let Some(old_task) = ws_guard.take() {
            old_task.abort();
            tracing::info!("Aborted previous Kotak WebSocket task.");
        }

        let (ws_tx, ws_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let new_handle = tokio::spawn(kotak_client::start_market_data_stream(
            auth.to_owned(), sid.to_owned(), scrips, 1,
            Arc::clone(&state.prices),
            ws_rx,
        ));
        *ws_guard = Some(new_handle);
        
        let mut tx_guard = state.ws_tx.lock().await;
        *tx_guard = Some(ws_tx);
        if let Some(tx) = tx_guard.as_ref() {
            let keys: Vec<String> = state
                .positions
                .read()
                .await
                .iter()
                .filter_map(|p| p.ws_scrip_key.clone())
                .collect();

            for key in keys {
                let payload = serde_json::json!({
                    "action": "subscribe",
                    "scrips": key,
                });
                let _ = tx.send(payload.to_string());
            }
        }
    }

    // Fetch and parse Scrip Master
    let _ = state.log_tx.send(r#"{"event":"SCRIP_FETCH","message":"Fetching Kotak Scrip Master..."}"#.into());
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
                let _ = state.log_tx.send(format!(r#"{{"event":"SCRIP_FETCH_ERROR","message":"Failed to fetch {} scrip master: {}"}}"#, segment, e));
            }
        }
    }

    if raw_sections.is_empty() {
        let _ = state.log_tx.send(r#"{"event":"SCRIP_FETCH_ERROR","message":"Failed to fetch all scrip master segments"}"#.into());
    } else {
        *state.scrip_store.write().await = Some(store);
        *state.raw_scrip_csv.write().await = merge_csv_sections(&raw_sections);
        let _ = state.log_tx.send(r#"{"event":"SCRIP_FETCH_SUCCESS","message":"Scrip Master loaded successfully"}"#.into());
    }

    let _ = state.log_tx.send(r#"{"event":"KOTAK_CONNECTED","status":"ok"}"#.into());
    *state.kotak.lock().await = Some(client);
    let kotak = state.kotak.lock().await;
    match *kotak {
        Some(_) => (StatusCode::OK, Json(serde_json::json!({"status": "connected"}))).into_response(),
        None => (StatusCode::OK, Json(serde_json::json!({"status": "disconnected"}))).into_response(),
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

/// `GET /api/auth/kotak` — return connection status.
pub async fn kotak_status_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"connected": state.kotak.lock().await.is_some()}))
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
