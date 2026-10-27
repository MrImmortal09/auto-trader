//! `GET /api/v2/desk`, `/api/v2/feed` and `/api/v2/status` — what the v2
//! Desk and its status bar show (`crate::desk`). Read-only.

use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared_domain::{MonitoredPosition, TradeState, TradeTag, TradingConfig};

use crate::auth::Identity;
use crate::desk::{self, Context, DeskPosition, FeedItem, FeedRow};
use crate::history::{self, Call, GroupBy, Range};
use crate::AppState;

/// Most fills one status answer carries, for toasts.
const MAX_FILLS: usize = 10;

fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("v2 desk: {what}: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("could not read {what}") }))).into_response()
}

/// The current mode's calls, from every trade row.
async fn load_calls(state: &AppState, mode: &str) -> Result<Vec<Call>, sqlx::Error> {
    Ok(history::calls(history::load_legs(&state.db_pool, mode).await?))
}

fn ltp_of(state: &AppState, p: &MonitoredPosition) -> Option<f64> {
    p.ws_scrip_key.as_ref().and_then(|k| state.prices.get(k).map(|v| *v)).filter(|v| *v > 0.0)
}

fn index_ltp(state: &AppState, index: &str) -> Option<f64> {
    let (_, feed_name) = shared_domain::kotak_index_feed(index)?;
    state.prices.get(&shared_domain::index_price_key(feed_name)).map(|v| *v).filter(|v| *v > 0.0)
}

/// The open and the waiting positions, as Desk rows.
fn desk_rows(
    state: &AppState,
    positions: &[MonitoredPosition],
    cfg: &TradingConfig,
    calls: &[Call],
) -> (Vec<DeskPosition>, Vec<DeskPosition>) {
    // Keyed the way `history` keys a call: chat, signal id and index.
    let opened_at: HashMap<&str, &str> = calls.iter().map(|c| (c.key.as_str(), c.opened_at.as_str())).collect();
    let (mut open, mut waiting) = (Vec::new(), Vec::new());
    for p in positions.iter().filter(|p| !matches!(p.state, TradeState::Closed)) {
        let tag = TradeTag::of(&p.signal);
        let key = format!(
            "{}|{}|{}",
            tag.chat_id.map(|c| c.to_string()).unwrap_or_default(),
            tag.signal_id.as_deref().unwrap_or_default(),
            p.signal.instrument_name
        );
        let is_waiting = matches!(p.state, TradeState::WaitingForEntry);
        let cx = Context {
            ltp: ltp_of(state, p),
            index_ltp: index_ltp(state, &p.signal.instrument_name),
            default_lots: trading_engine::lots_for_instrument(cfg, &p.signal.instrument_name),
            first_buy_at: opened_at.get(key.as_str()).map(|t| t.to_string()),
            zone: if is_waiting { trading_engine::entry_buy_window(&p.signal, cfg) } else { None },
            valid_till: if is_waiting {
                shared_domain::entry_gate::call_expires_at(&p.created_at, cfg.call_validity_minutes)
            } else {
                None
            },
        };
        let row = DeskPosition::of(p, &cx);
        if is_waiting { waiting.push(row) } else { open.push(row) }
    }
    (open, waiting)
}

#[derive(Serialize)]
struct Wallet {
    /// `None` while the LIVE balance hasn't been read from Kotak yet.
    balance: Option<f64>,
    /// `PAPER`, `LIVE` or `LIVE_UNAVAILABLE`, as `/api/portfolio` says.
    source: &'static str,
    /// IST time of the Kotak read behind a LIVE balance.
    updated_at: Option<String>,
}

/// The wallet `/api/portfolio` shows: the paper wallet, or Kotak's cached
/// balance in LIVE — never a fresh Kotak call.
async fn wallet(state: &AppState, mode: &str) -> Wallet {
    if mode == "LIVE" {
        return match state.balance.latest().await {
            Some(b) => Wallet { balance: Some(b.net), source: "LIVE", updated_at: Some(b.fetched_at) },
            None => Wallet { balance: None, source: "LIVE_UNAVAILABLE", updated_at: None },
        };
    }
    let balance = sqlx::query_scalar::<_, f64>("SELECT balance FROM wallet WHERE id = 1")
        .fetch_one(&state.db_pool)
        .await
        .map_err(|e| tracing::error!("v2 desk: wallet: {e}"))
        .ok();
    Wallet { balance, source: "PAPER", updated_at: None }
}

#[derive(Serialize)]
struct ClosedCall {
    #[serde(flatten)]
    call: Call,
    /// When its Telegram message was sent, if the feed has it.
    message_at: Option<String>,
}

/// When each `(chat, message)` was sent, from the stored feed.
async fn message_times(state: &AppState, calls: &[Call]) -> HashMap<(i64, i64), String> {
    let mut times = HashMap::new();
    for c in calls {
        let (Some(chat), Some(msg)) = (c.chat_id, c.signal_id.as_deref().and_then(|s| s.parse::<i64>().ok())) else { continue };
        if let Ok(Some(sent_at)) = sqlx::query_scalar::<_, String>(
            "SELECT sent_at FROM telegram_messages WHERE chat_id = ? AND msg_id = ?",
        )
        .bind(chat)
        .bind(msg)
        .fetch_optional(&state.db_pool)
        .await
        {
            times.insert((chat, msg), sent_at);
        }
    }
    times
}

#[derive(Serialize)]
struct DeskAnswer {
    mode: String,
    kill_switch: bool,
    wallet: Wallet,
    kpis: desk::DeskKpis,
    open: Vec<DeskPosition>,
    waiting: Vec<DeskPosition>,
    /// Calls closed today, newest first.
    closed: Vec<ClosedCall>,
}

/// `GET /api/v2/desk` — Desk's Today: the figures (net, booked, open, calls
/// and wallet), the open and waiting positions, and the calls closed today
/// with their legs, original message and how each ended. The current mode.
pub async fn desk_handler(State(state): State<AppState>) -> Response {
    let cfg = state.trading_cfg.read().await.clone();
    let calls = match load_calls(&state, &cfg.mode).await {
        Ok(calls) => calls,
        Err(e) => return server_error("trades", e),
    };
    let positions = state.positions.read().await.clone();
    let today = shared_domain::today_ist();
    let (open, waiting) = desk_rows(&state, &positions, &cfg, &calls);
    let closed_calls = history::report(&calls, today, Range::Today, GroupBy::Channel).calls;
    let times = message_times(&state, &closed_calls).await;
    let closed: Vec<ClosedCall> = closed_calls
        .into_iter()
        .map(|call| {
            let message_at = call
                .chat_id
                .zip(call.signal_id.as_deref().and_then(|s| s.parse::<i64>().ok()))
                .and_then(|key| times.get(&key).cloned());
            ClosedCall { message_at, call }
        })
        .collect();
    let booked = desk::booked_on(&calls, &today.format("%Y-%m-%d").to_string());
    Json(DeskAnswer {
        kpis: desk::kpis(booked, &open, waiting.len(), closed.len()),
        wallet: wallet(&state, &cfg.mode).await,
        mode: cfg.mode,
        kill_switch: cfg.kill_switch_active,
        open,
        waiting,
        closed,
    })
    .into_response()
}

/// The messages the monitored chats posted on `day` (`YYYY-MM-DD`), newest
/// first and at most `limit`, each with what became of it.
pub(crate) async fn feed_items(state: &AppState, day: &str, limit: i64) -> Result<Vec<FeedItem>, Response> {
    let rows = sqlx::query_as::<_, FeedRow>(
        "SELECT id, chat_id, chat_title, msg_id, sent_at, text, kind, reply_to, value, edited_at
         FROM telegram_messages WHERE sent_at >= ? AND sent_at <= ?
         ORDER BY sent_at DESC, id DESC LIMIT ?",
    )
    .bind(format!("{day} 00:00:00"))
    .bind(format!("{day} 23:59:59"))
    .bind(limit)
    .fetch_all(&state.db_pool)
    .await
    .map_err(|e| server_error("telegram messages", e))?;
    let cfg = state.trading_cfg.read().await.clone();
    let calls = load_calls(state, &cfg.mode).await.map_err(|e| server_error("trades", e))?;
    let positions = state.positions.read().await.clone();
    let valid_till = |p: &MonitoredPosition| shared_domain::entry_gate::call_expires_at(&p.created_at, cfg.call_validity_minutes);
    Ok(rows
        .into_iter()
        .map(|row| {
            let status = desk::feed_status(&row, &positions, &calls, valid_till);
            FeedItem::of(row, status)
        })
        .collect())
}

#[derive(Deserialize)]
pub struct FeedQuery {
    day: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct Channel {
    chat_id: i64,
    title: String,
    messages: usize,
}

#[derive(Serialize)]
struct FeedAnswer {
    day: String,
    /// The chats that posted that day, for the channel filter.
    channels: Vec<Channel>,
    /// Newest first.
    items: Vec<FeedItem>,
}

/// `GET /api/v2/feed?day=YYYY-MM-DD&limit=` — the messages the monitored
/// chats posted that day (default today, newest 200), each with what became
/// of it: bought, waiting, closed with its P&L, skipped, or not a call.
pub async fn feed_handler(State(state): State<AppState>, Query(q): Query<FeedQuery>) -> Response {
    let day = match q.day.as_deref().map(str::trim) {
        None | Some("") => shared_domain::today_ist(),
        Some(raw) => match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            Ok(day) => day,
            Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "day must be YYYY-MM-DD" }))).into_response(),
        },
    };
    let day = day.format("%Y-%m-%d").to_string();
    let limit = q.limit.unwrap_or(200).clamp(1, 500);
    let items = match feed_items(&state, &day, limit).await {
        Ok(items) => items,
        Err(e) => return e,
    };
    let mut channels: BTreeMap<i64, Channel> = BTreeMap::new();
    for item in &items {
        channels
            .entry(item.chat_id)
            .or_insert_with(|| Channel { chat_id: item.chat_id, title: item.channel.clone(), messages: 0 })
            .messages += 1;
    }
    let mut channels: Vec<Channel> = channels.into_values().collect();
    channels.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Json(FeedAnswer { day, channels, items }).into_response()
}

#[derive(Deserialize)]
pub struct StatusQuery {
    /// The newest trade row the caller has seen: fills after it come back.
    after: Option<i64>,
}

#[derive(Serialize)]
struct StatusAnswer {
    /// The server's IST clock, `YYYY-MM-DD HH:MM:SS`.
    now: String,
    mode: String,
    kill_switch: bool,
    /// What the kill switch did, while it's on.
    stopped: Option<desk::Stopped>,
    market_open: bool,
    market: &'static str,
    /// `None` until the System screen's sampler has run once.
    health: Option<desk::HealthSet>,
    /// Seconds since the price feed's last tick, while it's connected.
    feed_age_secs: Option<f64>,
    /// Indices whose options are held open on their expiry day.
    expiring: Vec<desk::Expiring>,
    /// Today's booked plus open P&L.
    day_pnl: f64,
    /// Today's trade rows after `after`, oldest first.
    fills: Vec<desk::Fill>,
    last_fill_id: i64,
}

/// `GET /api/v2/status?after=` — the status bar: mode, kill switch, market
/// hours, the five health dots and the day's P&L, plus the fills after
/// `after` for toasts (none without it). Public, but the owner's figures are
/// the owner's and the admin's alone: everyone else gets the clock, the
/// market phase and the health dots, with `PAPER`, no kill switch and no P&L.
pub async fn status_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<StatusQuery>,
) -> Response {
    let now = shared_domain::now_ist();
    let today = now.date_naive();
    let day = today.format("%Y-%m-%d").to_string();
    let (market_open, market) = desk::market_phase(now, !telegram_ingester::parser::is_trading_holiday(today));
    let latest = state.pulse.latest();
    let feed_age_secs = latest.as_ref().filter(|s| s.feed_connected).and_then(|s| s.feed_last_tick_age_secs);
    let health = latest.map(|s| {
        desk::health_set(
            &desk::HealthInputs {
                engine_heartbeat_age_secs: s.engine_heartbeat_age_secs,
                feed_connected: s.feed_connected,
                feed_subscriptions: s.feed_subscriptions,
                feed_last_tick_age_secs: s.feed_last_tick_age_secs,
                kotak_connected: s.kotak_connected,
                kotak_session_healthy: s.kotak_session_healthy,
                telegram_connected: s.telegram_connected,
                upstox_configured: s.upstox_configured,
                upstox_last_success_age_secs: s.upstox_last_success_age_secs,
            },
            market_open,
        )
    });

    if !identity.can_read_owner_data() {
        return Json(StatusAnswer {
            now: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            mode: "PAPER".to_string(),
            kill_switch: false,
            stopped: None,
            market_open,
            market,
            health,
            feed_age_secs,
            expiring: Vec::new(),
            day_pnl: 0.0,
            fills: Vec::new(),
            last_fill_id: 0,
        })
        .into_response();
    }

    let cfg = state.trading_cfg.read().await.clone();
    let calls = match load_calls(&state, &cfg.mode).await {
        Ok(calls) => calls,
        Err(e) => return server_error("trades", e),
    };
    let positions = state.positions.read().await.clone();

    let (open, waiting) = desk_rows(&state, &positions, &cfg, &calls);
    let day_pnl = desk::kpis(desk::booked_on(&calls, &day), &open, waiting.len(), 0).net;
    let stopped = if cfg.kill_switch_active {
        let engaged = sqlx::query_as::<_, (String, String)>(
            "SELECT message, timestamp FROM system_logs
             WHERE message LIKE '%\"KILL_SWITCH_ENGAGED\"%' ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(&state.db_pool)
        .await
        .map_err(|e| tracing::error!("v2 status: kill switch log: {e}"))
        .ok()
        .flatten();
        // Only today's: a switch left on from an earlier day says just that.
        Some(match engaged {
            Some((message, ts)) if ts.starts_with(&day) => desk::stopped_from_log(&message, &ts),
            _ => desk::Stopped { at: None, positions_exited: 0, entries_cancelled: 0 },
        })
    } else {
        None
    };

    Json(StatusAnswer {
        now: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        mode: cfg.mode,
        kill_switch: cfg.kill_switch_active,
        stopped,
        market_open,
        market,
        health,
        feed_age_secs,
        expiring: desk::expiring_today(&positions, today),
        day_pnl,
        fills: q.after.map(|after| desk::fills_after(&calls, &day, after, MAX_FILLS)).unwrap_or_default(),
        last_fill_id: desk::last_leg_id(&calls),
    })
    .into_response()
}
