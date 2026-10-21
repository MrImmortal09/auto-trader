//! `GET /api/v2/system` — the v2 System screen's day counts (`crate::system`)
//! and the lines under its tiles; `GET /api/v2/mode-check` — what switching
//! between Paper and LIVE would do. Read-only; the live tiles and charts come
//! from `/api/telemetry/*`.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;
use shared_domain::TradeState;

use crate::routes::health::SERVER_VERSION;
use crate::system::{self, OrderCounts, SignalCounts};
use crate::AppState;

/// How far back today's log is searched, in rows: well past a busy day, and
/// a primary-key range, so the table's size never matters.
const LOG_WINDOW: i64 = 50_000;

/// Most messages counted in a day.
const MAX_MESSAGES: i64 = 5_000;

#[derive(Serialize)]
struct Telegram {
    /// Chats being listened to, when known.
    listening: Option<usize>,
    /// Messages they posted today.
    messages: i64,
    /// IST `YYYY-MM-DD HH:MM:SS` of the latest.
    last_at: Option<String>,
}

#[derive(Serialize)]
struct Kotak {
    /// The session watchdog's re-logins today.
    relogins: u64,
    /// IST `HH:MM` of today's last successful login.
    logged_in_at: Option<String>,
}

#[derive(Serialize)]
struct Upstox {
    configured: bool,
    /// The IST day its token expires, `YYYY-MM-DD`.
    token_expires: Option<String>,
}

#[derive(Serialize)]
struct SystemAnswer {
    day: String,
    mode: String,
    /// IST `HH:MM` the server started, when that was today — Kotak's order
    /// counters only count from then.
    started_today: Option<String>,
    uptime_secs: Option<u64>,
    version: &'static str,
    /// IST `YYYY-MM-DD HH:MM:SS` of the last Scrip Master download.
    scrip_loaded_at: Option<String>,
    signals: SignalCounts,
    orders: OrderCounts,
    telegram: Telegram,
    kotak: Kotak,
    upstox: Upstox,
}

fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("v2 system: {what}: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("could not read {what}") }))).into_response()
}

/// Today's log lines carrying `event`, newest first.
async fn todays_log(state: &AppState, day: &str, event: &str) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT message, timestamp FROM system_logs
         WHERE id > (SELECT COALESCE(MAX(id), 0) FROM system_logs) - ?
           AND timestamp >= ? AND message LIKE ?
         ORDER BY id DESC",
    )
    .bind(LOG_WINDOW)
    .bind(format!("{day} 00:00:00"))
    .bind(format!("%\"{event}\"%"))
    .fetch_all(&state.db_pool)
    .await
}

/// The chats being listened to: the env-configured ingester's, or the
/// dashboard login's — `None` while that login is busy.
async fn listening(state: &AppState) -> Option<usize> {
    if std::env::var("TELEGRAM_API_ID").is_ok() {
        let ids = std::env::var("TELEGRAM_CHAT_IDS").unwrap_or_default();
        return Some(ids.split(',').filter(|s| s.trim().parse::<i64>().is_ok()).count());
    }
    state.telegram.try_lock().ok().map(|m| m.monitored_chats.len())
}

/// `GET /api/v2/system` — what became of today's calls, today's orders, and
/// the Telegram, Kotak and Upstox lines on the System screen.
pub async fn system_handler(State(state): State<AppState>) -> Response {
    let now = shared_domain::now_ist();
    let day = now.date_naive().format("%Y-%m-%d").to_string();
    let mode = state.trading_cfg.read().await.mode.clone();

    let items = match super::desk::feed_items(&state, &day, MAX_MESSAGES).await {
        Ok(items) => items,
        Err(e) => return e,
    };
    let abandoned: Vec<String> = match todays_log(&state, &day, "ENTRY_ABANDONED").await {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|(message, _)| {
                let v: serde_json::Value = serde_json::from_str(&message).ok()?;
                v.get("reason")?.as_str().map(str::to_string)
            })
            .collect(),
        Err(e) => return server_error("the log", e),
    };
    let signals = system::signal_counts(items.iter().map(|i| &i.status), &abandoned);

    let fills = match sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM paper_trades WHERE mode = ? AND timestamp >= ?")
        .bind(&mode)
        .bind(format!("{day} 00:00:00"))
        .fetch_one(&state.db_pool)
        .await
    {
        Ok(n) => n.max(0) as u64,
        Err(e) => return server_error("trades", e),
    };

    let latest = state.pulse.latest();
    let start = state.pulse.day_start().filter(|s| s.day == now.date_naive());
    let base = start.as_ref().and_then(|s| s.baseline.as_deref());
    let live = mode == "LIVE";
    let (orders, relogins) = match latest.as_deref() {
        Some(s) => (system::order_counts(live, fills, s, base), system::relogins_today(s, base)),
        None => (system::order_counts(live, fills, &Default::default(), None), 0),
    };
    let uptime_secs = latest.as_ref().map(|s| s.process_uptime_secs);
    let started_today = uptime_secs.and_then(|up| {
        let started = now - chrono::Duration::seconds(i64::try_from(up).unwrap_or(i64::MAX / 2));
        (started.date_naive() == now.date_naive()).then(|| started.format("%H:%M").to_string())
    });

    let (messages, last_at) = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT COUNT(*), MAX(sent_at) FROM telegram_messages WHERE sent_at >= ? AND sent_at <= ?",
    )
    .bind(format!("{day} 00:00:00"))
    .bind(format!("{day} 23:59:59"))
    .fetch_one(&state.db_pool)
    .await
    .unwrap_or_else(|e| {
        tracing::error!("v2 system: telegram messages: {e}");
        (0, None)
    });

    let logged_in_at = todays_log(&state, &day, "KOTAK_LOGIN_OK")
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|(_, ts)| ts.get(11..16).map(str::to_string));

    let token_expires = std::env::var("UPSTOX_ANALYTICS_TOKEN")
        .ok()
        .and_then(|t| system::token_expiry(&t))
        .map(|d| d.format("%Y-%m-%d").to_string());

    let scrip_loaded_at = state.scrip_loaded_at.read().await.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string());

    Json(SystemAnswer {
        day,
        mode,
        started_today,
        uptime_secs,
        version: SERVER_VERSION,
        scrip_loaded_at,
        signals,
        orders,
        telegram: Telegram { listening: listening(&state).await, messages, last_at },
        kotak: Kotak { relogins, logged_in_at },
        upstox: Upstox { configured: state.market.configured, token_expires },
    })
    .into_response()
}

#[derive(Serialize)]
struct Funds {
    /// Kotak's net available funds, from the hourly cache — never a fresh call.
    net: f64,
    fetched_at: String,
}

#[derive(Serialize)]
struct IndexLots {
    index: &'static str,
    lots: i32,
}

#[derive(Serialize)]
struct ModeCheck {
    mode: String,
    kotak_connected: bool,
    funds: Option<Funds>,
    kill_switch: bool,
    /// Positions holding quantity now, in the current mode.
    open: usize,
    /// Calls waiting for their entry.
    waiting: usize,
    /// What each index's call buys, as the engine sizes it.
    lots: Vec<IndexLots>,
}

/// `GET /api/v2/mode-check` — the facts behind a switch between Paper and
/// LIVE. The engine trades every position in the one current mode, so on a
/// switch to LIVE the waiting calls buy with real money and open paper
/// positions (which Kotak doesn't hold) are dropped; on a switch to Paper
/// open LIVE positions would stay at Kotak with nothing watching them.
pub async fn mode_check_handler(State(state): State<AppState>) -> Response {
    let cfg = state.trading_cfg.read().await.clone();
    let (open, waiting) = {
        let positions = state.positions.read().await;
        let open = positions
            .iter()
            .filter(|p| matches!(p.state, TradeState::Active | TradeState::Target1Hit) && p.executed_qty > 0)
            .count();
        let waiting = positions.iter().filter(|p| matches!(p.state, TradeState::WaitingForEntry)).count();
        (open, waiting)
    };
    // From the sampler's last reading: never the Kotak client's mutex, which
    // orders wait on.
    let kotak_connected = state.pulse.latest().is_some_and(|s| s.kotak_connected && s.kotak_session_healthy);
    let funds = state.balance.latest().await.map(|b| Funds { net: b.net, fetched_at: b.fetched_at });
    let lots = shared_domain::INDEX_NAMES
        .iter()
        .map(|&index| IndexLots { index, lots: trading_engine::lots_for_instrument(&cfg, index) })
        .collect();
    Json(ModeCheck {
        mode: cfg.mode,
        kotak_connected,
        funds,
        kill_switch: cfg.kill_switch_active,
        open,
        waiting,
        lots,
    })
    .into_response()
}
