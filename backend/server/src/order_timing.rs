//! Order timing, for the admin: how long each LIVE order took — sent, Kotak's
//! answer (from the send tap in `place_live_order`), and accepted and filled
//! (from Kotak's push stream of order updates), all on this server's clock,
//! plus Kotak's and the exchange's own timestamps.
//!
//! Analytics only. The send tap is a copy made after Kotak answered; the push
//! stream only reads. The engine still learns fills from its own order-book
//! poll and decides nothing from here.
//!
//! The push stream only runs with `KOTAK_ORDER_STREAM=on`: Kotak documents
//! its login two ways, so it is tried first with `server order-feed-probe`.
//!
//! Each order also ends in one `ORDER_TIMING` line in the engine log (and the
//! server log): every stage counted from the send, up to when the engine's
//! own order-book poll saw it finished — see `timeline`.

mod timeline;

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use kotak_client::orders::{stream_order_updates, AuthStyle, FeedEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared_domain::DbWriteMessage;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::AppState;
use timeline::{Push, Timelines};

/// How often a missing session is looked for, and a live one checked for a
/// re-login.
const SESSION_CHECK: Duration = Duration::from_secs(15);
const BACKOFF_START: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Messages of a shape this code doesn't read are logged, this many per
/// connection, so a live session shows what Kotak sends.
const LOG_UNKNOWN: usize = 5;
/// Order updates logged in full per connection, for the same reason.
const LOG_ORDERS: usize = 3;

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

/// The push stream switch: `KOTAK_ORDER_STREAM=on` (or `1`, `true`).
pub fn push_enabled() -> bool {
    std::env::var("KOTAK_ORDER_STREAM")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "on" | "1" | "true" | "yes"))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PushStatus {
    pub enabled: bool,
    pub connected: bool,
    /// The login style Kotak acknowledged: `json` or `raw`.
    pub style: Option<&'static str>,
    pub last_event_ms: Option<i64>,
    pub error: Option<String>,
    /// Order updates received since the server started.
    pub updates: u64,
}

pub struct OrderTiming {
    push: StdMutex<PushStatus>,
    timelines: StdMutex<Timelines>,
}

impl OrderTiming {
    pub fn new(enabled: bool) -> Self {
        Self {
            push: StdMutex::new(PushStatus { enabled, ..PushStatus::default() }),
            timelines: StdMutex::new(Timelines::default()),
        }
    }

    fn push(&self) -> std::sync::MutexGuard<'_, PushStatus> {
        self.push.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn timelines(&self) -> std::sync::MutexGuard<'_, Timelines> {
        self.timelines.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> PushStatus {
        self.push().clone()
    }

    /// The lines of the orders whose timeline is complete now.
    fn due(&self) -> Vec<serde_json::Value> {
        let push = {
            let p = self.push();
            match (p.enabled, p.connected) {
                (false, _) => Push::Off,
                (true, false) => Push::NotConnected,
                (true, true) => Push::Live,
            }
        };
        self.timelines().due(now_ms(), push)
    }
}

/// How often finished timelines are looked for.
const TIMELINE_EVERY: Duration = Duration::from_millis(500);

/// Records every timed send and every order the engine's poll first sees
/// finished, and writes each order's `ORDER_TIMING` line once its timeline is
/// complete. Both taps are subscribed here, before any order can go.
pub fn start_recorder(me: Arc<OrderTiming>, db_tx: mpsc::Sender<DbWriteMessage>, log_tx: broadcast::Sender<String>) {
    let mut sends = kotak_client::subscribe_orders();
    let mut finished = kotak_client::orders::subscribe_book_finished();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TIMELINE_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let mut lines = Vec::new();
            tokio::select! {
                sent = sends.recv() => match sent {
                    Ok(sent) => {
                        lines.extend(me.timelines().on_sent(&sent));
                        if db_tx.send(DbWriteMessage::OrderSent(sent)).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => tracing::warn!(missed = n, "Order timing fell behind the send tap"),
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                seen = finished.recv() => match seen {
                    Ok(seen) => {
                        if me.timelines().on_seen(&seen) {
                            let _ = db_tx.send(DbWriteMessage::OrderSeen { order_no: seen.order_no, at_ms: seen.at_ms }).await;
                        }
                    }
                    // A poll that just started lists the whole day's orders at once.
                    Err(broadcast::error::RecvError::Lagged(n)) => tracing::warn!(missed = n, "Order timing fell behind the engine's poll tap"),
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = tick.tick() => {}
            }
            lines.extend(me.due());
            for line in lines {
                let text = line.get("message").and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
                tracing::info!("{text}");
                crate::routes::auth_kotak::send_log(&db_tx, &log_tx, "INFO", line).await;
            }
        }
    });
}

/// The session's login, cloned out of the Kotak mutex (never held across I/O).
#[derive(Debug, Clone, PartialEq)]
struct Login {
    base_url: String,
    auth_token: String,
    sid: String,
}

async fn current_login(kotak: &Mutex<Option<kotak_client::KotakClient>>) -> Option<Login> {
    let guard = kotak.lock().await;
    let s = guard.as_ref()?.session.as_ref()?;
    Some(Login { base_url: s.base_url.clone(), auth_token: s.auth_token.clone(), sid: s.sid.clone() })
}

/// Runs forever: holds Kotak's order stream open while there is a session,
/// records each update, and reconnects on a re-login or a drop. A login Kotak
/// doesn't acknowledge is tried the other documented way next time.
pub async fn run_push_stream(me: Arc<OrderTiming>, kotak: Arc<Mutex<Option<kotak_client::KotakClient>>>, db_tx: mpsc::Sender<DbWriteMessage>) {
    let mut style = AuthStyle::FIRST;
    let mut backoff = BACKOFF_START;
    loop {
        let Some(login) = current_login(&kotak).await else {
            {
                let mut p = me.push();
                p.connected = false;
                p.error = Some("no Kotak session".into());
            }
            tokio::time::sleep(SESSION_CHECK).await;
            continue;
        };

        let (tx, mut rx) = mpsc::channel::<FeedEvent>(256);
        let conn = {
            let login = login.clone();
            tokio::spawn(async move { stream_order_updates(&login.base_url, &login.auth_token, &login.sid, style, tx).await })
        };
        let mut check = tokio::time::interval(SESSION_CHECK);
        check.tick().await;
        let (mut acked, mut unknown, mut orders) = (false, 0usize, 0usize);

        let outcome: Result<(), String> = loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(FeedEvent::Connected) => {
                        acked = true;
                        backoff = BACKOFF_START;
                        let mut p = me.push();
                        p.connected = true;
                        p.style = Some(style.name());
                        p.error = None;
                        p.last_event_ms = Some(now_ms());
                        tracing::info!(style = style.name(), "Kotak order stream connected");
                    }
                    Some(FeedEvent::Order(update)) => {
                        {
                            let mut p = me.push();
                            p.updates += 1;
                            p.last_event_ms = Some(update.received_at_ms);
                        }
                        if orders < LOG_ORDERS {
                            orders += 1;
                            tracing::info!(?update, "Kotak order stream update");
                        }
                        me.timelines().on_update(&update);
                        if db_tx.send(DbWriteMessage::OrderUpdate(update)).await.is_err() {
                            conn.abort();
                            return;
                        }
                    }
                    Some(FeedEvent::Other(text)) => {
                        me.push().last_event_ms = Some(now_ms());
                        if unknown < LOG_UNKNOWN {
                            unknown += 1;
                            tracing::info!(message = %text, "Kotak order stream: message not read as an order");
                        }
                    }
                    None => break conn.await.unwrap_or_else(|e| Err(format!("stream task: {e}"))),
                },
                _ = check.tick() => {
                    if current_login(&kotak).await.as_ref() != Some(&login) {
                        conn.abort();
                        break Ok(());
                    }
                }
            }
        };

        {
            let mut p = me.push();
            p.connected = false;
            p.error = outcome.as_ref().err().cloned();
        }
        match outcome {
            Ok(()) => tracing::info!("Kotak order stream closed (re-login or socket closed); reconnecting"),
            Err(e) => {
                tracing::warn!(error = %e, style = style.name(), retry_in_secs = backoff.as_secs(), "Kotak order stream failed");
                if !acked {
                    style = style.other();
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TimingQuery {
    /// IST `YYYY-MM-DD`; today without it.
    day: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TimingAnswer {
    pub day: String,
    pub push: PushStatus,
    pub orders: Vec<crate::db::OrderTimingRow>,
}

/// `GET /api/admin/order-timings` (admin only).
pub async fn timings_handler(State(state): State<AppState>, Query(q): Query<TimingQuery>) -> Response {
    let day = q.day.filter(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()).unwrap_or_else(|| shared_domain::today_ist().format("%Y-%m-%d").to_string());
    match crate::db::load_order_timings(&state.db_pool, &day).await {
        Ok(orders) => Json(TimingAnswer { day, push: state.order_timing.status(), orders }).into_response(),
        Err(e) => {
            tracing::error!("order timings: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "could not read the order timings" }))).into_response()
        }
    }
}

/// `server order-feed-probe [seconds]`: connect to Kotak's order stream with
/// the session saved in the database, try both documented logins, and print
/// what comes back for `seconds` (default 60). Read-only: it sends the login
/// and heartbeats, nothing else, and never prints the token.
pub async fn probe(pool: &SqlitePool, seconds: u64) -> i32 {
    let Some(row) = crate::db::load_kotak_session(pool).await else {
        eprintln!("No Kotak session in the database: log in to Kotak first (it is cleared at 15:40 IST)");
        return 1;
    };
    println!("Session base URL {} (sid …{})", row.base_url, row.sid.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());
    for style in [AuthStyle::FIRST, AuthStyle::FIRST.other()] {
        println!("--- trying the {} login", style.name());
        let (tx, mut rx) = mpsc::channel::<FeedEvent>(256);
        let (base_url, token, sid) = (row.base_url.clone(), row.auth_token.clone(), row.sid.clone());
        let conn = tokio::spawn(async move { stream_order_updates(&base_url, &token, &sid, style, tx).await });
        let deadline = tokio::time::sleep(Duration::from_secs(seconds));
        tokio::pin!(deadline);
        let mut acked = false;
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    conn.abort();
                    println!("--- {} s up", seconds);
                    break;
                }
                event = rx.recv() => match event {
                    Some(FeedEvent::Connected) => {
                        acked = true;
                        println!("connected: Kotak acknowledged the {} login", style.name());
                    }
                    Some(FeedEvent::Order(u)) => println!("order update: {u:?}"),
                    Some(FeedEvent::Other(text)) => println!("other message: {text}"),
                    None => {
                        match conn.await {
                            Ok(Ok(())) => println!("stream closed"),
                            Ok(Err(e)) => println!("stream failed: {e}"),
                            Err(e) => println!("stream task: {e}"),
                        }
                        break;
                    }
                },
            }
        }
        if acked {
            println!("Use this login: it works. Set KOTAK_ORDER_STREAM=on to run the stream in the server.");
            return 0;
        }
    }
    println!("Neither login was acknowledged.");
    2
}
