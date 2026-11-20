//! Admin → Hyperliquid markets: every perp Hyperliquid lists, and the ones
//! the app trades (`hyperliquid::markets`). The admin's alone (neither route
//! is named in the auth policy), behind the `hyperliquid` switch.
//!
//! - `GET /api/v2/hl/admin/markets` — Hyperliquid's live perps by dex, each
//!   with its rules, 24 h volume and whether the app trades it
//! - `PUT /api/v2/hl/admin/markets` `{add?: [coin], remove?: [coin],
//!   header?: [{coin, on}]}` — all of it or none; answers the list again

use axum::{extract::State, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::hyperliquid::markets::{self, Change, DexListing, MAX_CHANGE, MAX_HEADER};
use crate::routes::auth_kotak::send_log;
use crate::AppState;

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

async fn switched_on(state: &AppState) -> Result<(), Response> {
    if state.flags.read().await.hyperliquid {
        Ok(())
    } else {
        Err(refuse(StatusCode::NOT_FOUND, "Hyperliquid is switched off"))
    }
}

/// One perp on the admin's page.
#[derive(Serialize)]
struct PerpRow {
    coin: String,
    /// The app's name for it (`GOLD` for trade.xyz's gold).
    key: String,
    base: String,
    sz_decimals: u32,
    max_leverage: u32,
    /// The most the app takes on it: Hyperliquid's own limit.
    leverage_cap: u32,
    taker_fee: f64,
    maker_fee: f64,
    growth_mode: bool,
    only_isolated: bool,
    /// The mid (the mark when the book has one side), the 24 h change in
    /// percent, the 24 h volume in USD.
    price: Option<f64>,
    change_pct: Option<f64>,
    day_volume: Option<f64>,
    /// `added`, `close_only` (removed, still held) or `off`.
    state: &'static str,
    header: bool,
    /// A paper book holds, rests or arms something in it.
    held: bool,
}

#[derive(Serialize)]
struct DexRow {
    /// `null`: the core perps.
    dex: Option<String>,
    venue: String,
    perps: Vec<PerpRow>,
}

/// A perp the app keeps that Hyperliquid no longer lists.
#[derive(Serialize)]
struct UnlistedRow {
    coin: &'static str,
    key: &'static str,
    label: &'static str,
    state: &'static str,
    held: bool,
}

#[derive(Serialize)]
struct View {
    rev: u64,
    max_header: usize,
    dexes: Vec<DexRow>,
    unlisted: Vec<UnlistedRow>,
}

fn view(listings: &[DexListing], held: &std::collections::HashSet<&'static str>) -> View {
    let ours = markets::entries();
    let state_of = |coin: &str| -> (&'static str, bool, bool) {
        match ours.iter().find(|e| e.market.coin == coin) {
            Some(e) if e.added => ("added", e.header, held.contains(e.market.key)),
            Some(e) if held.contains(e.market.key) => ("close_only", false, true),
            _ => ("off", false, false),
        }
    };
    let dexes = listings
        .iter()
        .map(|d| {
            let mut perps: Vec<PerpRow> = d
                .perps
                .iter()
                .map(|p| {
                    let r = &p.rules;
                    let (taker_fee, maker_fee) = r.fees();
                    let (state, header, held) = state_of(&r.coin);
                    let price = p.ctx.as_ref().map(|c| c.mid.unwrap_or(c.mark));
                    let prev = p.ctx.as_ref().map(|c| c.prev_day).filter(|v| *v > 0.0);
                    PerpRow {
                        coin: r.coin.clone(),
                        key: markets::key_for(&r.coin).to_string(),
                        base: r.coin.split_once(':').map_or(r.coin.as_str(), |(_, b)| b).to_string(),
                        sz_decimals: r.sz_decimals,
                        max_leverage: r.max_leverage,
                        leverage_cap: r.max_leverage,
                        taker_fee,
                        maker_fee,
                        growth_mode: r.growth_mode,
                        only_isolated: r.only_isolated,
                        price,
                        change_pct: price.zip(prev).map(|(p, prev)| (p - prev) / prev * 100.0),
                        day_volume: p.ctx.as_ref().map(|c| c.day_volume),
                        state,
                        header,
                        held,
                    }
                })
                .collect();
            perps.sort_by(|a, b| b.day_volume.unwrap_or(0.0).total_cmp(&a.day_volume.unwrap_or(0.0)));
            DexRow { dex: d.dex.clone(), venue: d.venue.clone(), perps }
        })
        .collect();
    let listed = |coin: &str| listings.iter().any(|d| d.perps.iter().any(|p| p.rules.coin == coin));
    let unlisted = ours
        .iter()
        .filter(|e| !listed(e.market.coin) && (e.added || held.contains(e.market.key)))
        .map(|e| UnlistedRow { coin: e.market.coin, key: e.market.key, label: e.market.label, state: if e.added { "added" } else { "close_only" }, held: held.contains(e.market.key) })
        .collect();
    View { rev: markets::rev(), max_header: MAX_HEADER, dexes, unlisted }
}

pub async fn view_handler(State(state): State<AppState>) -> Response {
    if let Err(e) = switched_on(&state).await {
        return e;
    }
    match state.hl_markets.listings().await {
        Ok(l) => Json(view(&l, &state.perps.in_use().await)).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderBody {
    coin: String,
    on: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    header: Vec<HeaderBody>,
}

pub async fn change_handler(State(state): State<AppState>, Json(b): Json<ChangeBody>) -> Response {
    if let Err(e) = switched_on(&state).await {
        return e;
    }
    if b.add.len() + b.remove.len() > MAX_CHANGE || b.header.len() > MAX_CHANGE {
        return refuse(StatusCode::BAD_REQUEST, format!("At most {MAX_CHANGE} perps a change"));
    }
    if b.add.iter().any(|c| b.remove.contains(c)) {
        return refuse(StatusCode::BAD_REQUEST, "A perp can't be added and removed at once");
    }
    let change = Change { add: b.add, remove: b.remove, header: b.header.into_iter().map(|h| (h.coin, h.on)).collect() };
    if let Err(r) = state.hl_markets.change(&change).await {
        return refuse(r.status, r.message);
    }
    // The owner's LIVE orders that could open on a perp just removed are
    // cancelled, as paper's are at its next tick; its stops and targets stay.
    if !change.remove.is_empty() {
        if let Some(network) = state.hl_live.live_network().await {
            let problems = state.hl_live.cancel_opening(network, &change.remove).await;
            if !problems.is_empty() {
                let message = format!("Hyperliquid markets: LIVE orders on removed perps not all cancelled — {} — cancel them on Hyperliquid", problems.join("; "));
                tracing::error!("{message}");
                send_log(&state.db_tx, &state.log_tx, "ERROR", json!({ "event": "HL_MARKETS", "level": "ERROR", "message": message })).await;
            }
        }
    }
    let words = |what: &str, coins: &[String]| match coins.len() {
        0 => None,
        1..=6 => Some(format!("{what} {}", coins.join(", "))),
        n => Some(format!("{what} {n} perps")),
    };
    let message = [words("added", &change.add), words("removed (close-only)", &change.remove)].into_iter().flatten().collect::<Vec<_>>().join("; ");
    if !message.is_empty() {
        send_log(&state.db_tx, &state.log_tx, "WARN", json!({ "event": "HL_MARKETS", "level": "WARN", "message": format!("Hyperliquid markets: {message}") })).await;
    }
    match state.hl_markets.listings().await {
        Ok(l) => Json(view(&l, &state.perps.in_use().await)).into_response(),
        Err(r) => refuse(r.status, r.message),
    }
}
