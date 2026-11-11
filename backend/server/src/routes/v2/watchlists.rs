//! `/api/v2/watchlists` — each signed-in person's watchlists
//! (`crate::watchlists`), priced from the quote hub (`crate::quotes`) and
//! labelled from the instrument catalog (`crate::instruments`), behind the
//! `watchlists` feature flag: while it is off every route answers 404.
//!
//! `Access::SignedIn` for every route: the lists are keyed by the phone
//! sign-in's user id, the admin's included (the admin is a `users` row, and
//! `Access::PhoneUser` would refuse it), and every read and write here is
//! scoped to the caller's id, so another person's list is a plain 404.
//! A new key is accepted only when the catalog has a row for it, and the
//! row's own price-map key is what the list keeps; an unknown key is a 400
//! that names it, and is never polled. A key the list already holds is
//! kept as it is, without asking the catalog, so a list holding a contract
//! the catalog has dropped (an expired weekly) can still be reordered, and
//! that row removed by its key or its catalog id. Every key answered that
//! the catalog still has a row for is marked wanted on the hub, so the
//! next call (the front end re-polls every 2 s) finds it priced; a kept
//! key the catalog has dropped is shown without a row and not polled. A
//! catalog database that cannot be read is a 500, never "unknown keys" or
//! rows without their instrument. A quote never says whether the feed
//! carries its key (`QuoteHub::get_at`). Nothing here reaches the feed
//! task, the bridge, the engine or anyone's positions.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{FromRef, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared_domain::FeatureFlags;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::auth::Identity;
use crate::instruments::{Catalog, SearchRow};
use crate::quotes::{eod, hub::Quote, QuoteHub};
use crate::watchlists::{self, Error, ItemRow, ListRow, MAX_ITEMS, MAX_LISTS, MAX_NAME_CHARS};
use crate::AppState;

/// The longest key a body or path may carry, in characters.
const MAX_KEY_CHARS: usize = 64;

/// What the answers are built from; the handlers take them from
/// [`WatchlistState`], the tests build their own.
pub struct Deps<'a> {
    pub pool: &'a SqlitePool,
    pub catalog: &'a Catalog,
    pub hub: &'a QuoteHub,
}

/// The part of `AppState` the handlers read: the `watchlists` switch and
/// the three sources of [`Deps`]. Axum picks it out of the real state on
/// each request (`FromRef`), so the handlers can also be mounted on a
/// `Router` over this alone, as the tests do, without the rest of
/// `AppState`.
#[derive(Clone)]
pub(crate) struct WatchlistState {
    pub flags: Arc<RwLock<FeatureFlags>>,
    pub pool: SqlitePool,
    pub catalog: Arc<Catalog>,
    pub hub: Arc<QuoteHub>,
}

impl FromRef<AppState> for WatchlistState {
    fn from_ref(state: &AppState) -> Self {
        WatchlistState {
            flags: Arc::clone(&state.flags),
            pool: state.db_pool.clone(),
            catalog: Arc::clone(&state.instruments),
            hub: Arc::clone(&state.quotes),
        }
    }
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

/// One row of a watchlist: the key as kept, the catalog's row for it and
/// its quote.
#[derive(Debug, Serialize)]
pub struct ItemOut {
    /// The price-map key (`nse_cm|2885`, `nse_fo|51386`, `idx|NIFTY 50`).
    pub key: String,
    /// 0-based within the list.
    pub position: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub added_at: String,
    /// The catalog's row (`id`, `kind`, `symbol`, `name`, `exchange`,
    /// `trading_symbol`, `expiry`, `strike`, `lot_size`, …), as
    /// `/api/v2/search` lists it; `null` when the catalog no longer has
    /// one (an expired contract, or no catalog built yet).
    pub instrument: Option<SearchRow>,
    /// From the feed or Kotak's REST quotes during hours (`source: live`),
    /// Upstox while Kotak is down, the 15:35 close after hours
    /// (`source: close`, with `as_of`); `null` until the poller has fetched
    /// it — ask again in 2 s.
    pub quote: Option<Quote>,
    /// How old the quote's price is, by the server's clock; `null` without
    /// a quote.
    pub age_secs: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct WatchlistOut {
    pub id: i64,
    pub name: String,
    /// 0-based among the caller's lists.
    pub position: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub created_at: String,
    pub items: Vec<ItemOut>,
}

/// `GET /api/v2/watchlists`'s answer.
#[derive(Debug, Serialize)]
pub struct AllOut {
    /// Unix ms the answer was made, to age the rows against.
    pub at: i64,
    pub market_open: bool,
    pub max_lists: usize,
    pub max_items: usize,
    pub max_name_chars: usize,
    /// The caller's lists in order, each with its items in order.
    pub watchlists: Vec<WatchlistOut>,
}

/// Every other route's answer: the one list the call was about.
#[derive(Debug, Serialize)]
pub struct OneOut {
    pub at: i64,
    pub market_open: bool,
    pub watchlist: WatchlistOut,
}

/// Why a route refused, as the client sees it.
#[derive(Debug, PartialEq)]
pub enum Fail {
    /// 400.
    Bad(String),
    /// 400, naming the keys the catalog has no row for.
    UnknownKeys(Vec<String>),
    /// 404: no such list of the caller's, or no such key in it.
    NotFound(&'static str),
    /// 409.
    Conflict(String),
    /// 500, in general terms; the cause is logged.
    Server(&'static str),
}

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        match e {
            Error::NotFound => Fail::NotFound("no such watchlist"),
            Error::TooManyLists => Fail::Conflict(format!("You already have {MAX_LISTS} watchlists — delete one first")),
            Error::TooManyItems => Fail::Conflict(format!("This watchlist is full ({MAX_ITEMS} instruments) — remove one first")),
            Error::BadPosition => Fail::Bad("position is past your last watchlist".into()),
            Error::LastList => Fail::Conflict("Keep at least one watchlist — rename or empty this one instead".into()),
            Error::Db(e) => {
                tracing::error!("v2 watchlists: {e}");
                Fail::Server("could not save the watchlist")
            }
        }
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        match self {
            Fail::Bad(message) => (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response(),
            Fail::UnknownKeys(keys) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("unknown keys: {}", keys.join(", ")), "unknown": keys })),
            )
                .into_response(),
            Fail::NotFound(message) => (StatusCode::NOT_FOUND, Json(json!({ "error": message }))).into_response(),
            Fail::Conflict(message) => (StatusCode::CONFLICT, Json(json!({ "error": message }))).into_response(),
            Fail::Server(message) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": message }))).into_response(),
        }
    }
}

fn read_failed(e: sqlx::Error) -> Fail {
    tracing::error!("v2 watchlists: read: {e}");
    Fail::Server("could not read the watchlists")
}

/// The 500 for a catalog lookup that failed (logged by the catalog), as
/// `/api/v2/instruments/:id` answers it: never taken for keys the catalog
/// has no row for.
fn catalog_failed(_: sqlx::Error) -> Fail {
    Fail::Server("could not read the instrument catalog")
}

/// IST `YYYY-MM-DD HH:MM:SS` of `now`, as the user tables stamp rows.
fn stamp(now: DateTime<FixedOffset>) -> String {
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `lists` with `items` (every item of those lists, in order) grouped
/// under them, each item with the catalog's row and its quote as of `now`.
/// Every key the catalog resolved is marked wanted on the hub (the default
/// `idx|` keys resolve to the index rows); a kept key it has no row for
/// is answered without one and not wanted, so the poller never asks Kotak
/// for a contract that is gone. [`Fail::Server`] when the catalog could
/// not be read: every row would otherwise look expired.
async fn priced(deps: &Deps<'_>, lists: Vec<ListRow>, items: Vec<ItemRow>, now: DateTime<FixedOffset>) -> Result<Vec<WatchlistOut>, Fail> {
    let mut keys: Vec<String> = Vec::new();
    for item in &items {
        if !keys.contains(&item.key) {
            keys.push(item.key.clone());
        }
    }
    let rows = deps.catalog.rows_for_keys(&keys).await.map_err(catalog_failed)?;
    let now_ms = now.timestamp_millis();
    let mut by_list: HashMap<i64, Vec<ItemOut>> = HashMap::new();
    for item in items {
        let quote = deps.hub.get_at(&item.key, now);
        let age_secs = quote.as_ref().map(|q| ((now_ms - q.at) / 1000).max(0));
        by_list.entry(item.watchlist_id).or_default().push(ItemOut {
            instrument: rows.get(&item.key).cloned(),
            quote,
            age_secs,
            key: item.key,
            position: item.position,
            added_at: item.added_at,
        });
    }
    deps.hub.want(keys.into_iter().filter(|key| rows.contains_key(key)));
    Ok(lists
        .into_iter()
        .map(|list| WatchlistOut {
            items: by_list.remove(&list.id).unwrap_or_default(),
            id: list.id,
            name: list.name,
            position: list.position,
            created_at: list.created_at,
        })
        .collect())
}

/// Every list of `user_id` with its items priced, as of `now`. A person
/// with no list yet gets the default one first. Only then is trades.db
/// written: the front end asks every 2 s, and a plain read under WAL never
/// waits for the write lock, where [`watchlists::ensure_default`]'s
/// `BEGIN IMMEDIATE` would queue each poll behind the engine's
/// `db_writer` and the paper desk (and fail it after `busy_timeout`).
pub async fn all_answer(deps: &Deps<'_>, user_id: i64, now: DateTime<FixedOffset>) -> Result<AllOut, Fail> {
    let mut lists = watchlists::lists(deps.pool, user_id).await.map_err(read_failed)?;
    if lists.is_empty() {
        if watchlists::ensure_default(deps.pool, user_id, &stamp(now)).await.map_err(read_failed)? {
            tracing::info!(user_id, "Default watchlist created");
        }
        lists = watchlists::lists(deps.pool, user_id).await.map_err(read_failed)?;
    }
    let items = watchlists::items(deps.pool, user_id).await.map_err(read_failed)?;
    Ok(AllOut {
        at: now.timestamp_millis(),
        market_open: eod::open_at(now),
        max_lists: MAX_LISTS,
        max_items: MAX_ITEMS,
        max_name_chars: MAX_NAME_CHARS,
        watchlists: priced(deps, lists, items, now).await?,
    })
}

/// List `id` of `user_id` with its items priced, as of `now`.
pub async fn one_answer(deps: &Deps<'_>, user_id: i64, id: i64, now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let list = watchlists::list(deps.pool, user_id, id).await.map_err(read_failed)?.ok_or(Fail::NotFound("no such watchlist"))?;
    let items = watchlists::items_of(deps.pool, user_id, id).await.map_err(read_failed)?;
    let mut lists = priced(deps, vec![list], items, now).await?;
    Ok(OneOut { at: now.timestamp_millis(), market_open: eod::open_at(now), watchlist: lists.remove(0) })
}

/// `name` trimmed, 1 to [`MAX_NAME_CHARS`] characters.
pub fn clean_name(name: &str) -> Result<String, Fail> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Fail::Bad("name is required".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(Fail::Bad(format!("name is too long ({MAX_NAME_CHARS} characters at most)")));
    }
    Ok(name.to_string())
}

/// The most keys a body may name: a key can be typed as a catalog id or
/// as a price-map key, so one instrument may arrive twice before
/// [`resolve`] collapses the two forms; past this the body is refused
/// before anything is read.
const MAX_TYPED_KEYS: usize = 2 * MAX_ITEMS;

/// `keys` trimmed, blanks dropped, repeats kept once, in order; none too
/// long, and no more than [`MAX_TYPED_KEYS`]. The exact cap,
/// [`MAX_ITEMS`] instruments, is [`resolve`]'s to check, once an id and
/// its key have become one.
pub fn clean_keys(keys: &[String]) -> Result<Vec<String>, Fail> {
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        if key.chars().count() > MAX_KEY_CHARS {
            return Err(Fail::Bad(format!("a key is too long ({MAX_KEY_CHARS} characters at most)")));
        }
        if !out.iter().any(|k| k == key) {
            out.push(key.to_string());
        }
    }
    if out.len() > MAX_TYPED_KEYS {
        return Err(too_many(out.len()));
    }
    Ok(out)
}

/// The 400 for a body naming more than [`MAX_ITEMS`] instruments.
fn too_many(count: usize) -> Fail {
    Fail::Bad(format!("at most {MAX_ITEMS} instruments in a watchlist ({count} given)"))
}

/// The key of `held` (a list's keys, as stored) that `key` names, typed
/// as stored or as the catalog id of a cash row or contract
/// (`nse_fo:51386` for `nse_fo|51386`, the conversion `rows_for_keys`
/// makes), without asking the catalog: a contract it has dropped (an
/// expired weekly) is still found. An index's id (`nse_cm:Nifty 50`) is
/// not its key (`idx|NIFTY 50`), so it is left to the catalog.
fn held_key<'a>(held: &'a [String], key: &str) -> Option<&'a String> {
    held.iter().find(|h| *h == key).or_else(|| {
        let (segment, token) = key.split_once(':').filter(|(s, t)| !key.contains('|') && !s.is_empty() && !t.is_empty())?;
        let stored = format!("{segment}|{token}");
        held.iter().find(|h| **h == stored)
    })
}

/// The price-map key of each of `keys` (a price-map key or a catalog id,
/// as `clean_keys` left them), in order and distinct: a key the list
/// already holds (`held`, in either form, [`held_key`]) as it is stored,
/// every other one by the catalog's row for it. [`Fail::UnknownKeys`]
/// naming every key not held that the catalog has no row for, else
/// [`Fail::Bad`] past [`MAX_ITEMS`] distinct instruments — counted here,
/// after an id and its key have collapsed into one, so 51 typed keys
/// naming 50 instruments are 50. [`Fail::Server`] when the catalog could
/// not be read.
async fn resolve(catalog: &Catalog, keys: &[String], held: &[String]) -> Result<Vec<String>, Fail> {
    let fresh: Vec<String> = keys.iter().filter(|k| held_key(held, k).is_none()).cloned().collect();
    let rows = if fresh.is_empty() { HashMap::new() } else { catalog.rows_for_keys(&fresh).await.map_err(catalog_failed)? };
    let unknown: Vec<String> = fresh.iter().filter(|k| !rows.contains_key(*k)).cloned().collect();
    if !unknown.is_empty() {
        if catalog.status().day.is_none() {
            tracing::warn!(?unknown, "v2 watchlists: keys refused — the instrument catalog is not built (the `search` switch builds it)");
        }
        return Err(Fail::UnknownKeys(unknown));
    }
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        let kept = match held_key(held, key) {
            Some(stored) => stored,
            None => &rows[key].ws_key,
        };
        if !out.contains(kept) {
            out.push(kept.clone());
        }
    }
    if out.len() > MAX_ITEMS {
        return Err(too_many(out.len()));
    }
    Ok(out)
}

/// A new list named `name` at the end of `user_id`'s. The cap is checked
/// here for a plain answer and again by the insert itself.
pub async fn create_answer(deps: &Deps<'_>, user_id: i64, name: &str, now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let name = clean_name(name)?;
    if watchlists::lists(deps.pool, user_id).await.map_err(read_failed)?.len() >= MAX_LISTS {
        return Err(Error::TooManyLists.into());
    }
    let row = watchlists::create(deps.pool, user_id, &name, &stamp(now)).await?;
    one_answer(deps, user_id, row.id, now).await
}

/// `PATCH /api/v2/watchlists/:id`'s body: a new name and/or a new place
/// among the caller's lists. Left out: unchanged.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListPatch {
    pub name: Option<String>,
    pub position: Option<i64>,
}

pub async fn patch_answer(deps: &Deps<'_>, user_id: i64, id: i64, patch: &ListPatch, now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let name = match &patch.name {
        Some(name) => Some(clean_name(name)?),
        None => None,
    };
    let position = match patch.position {
        Some(p) if p < 0 => return Err(Fail::Bad("position must be 0 or more".into())),
        Some(p) => Some(p as usize),
        None => None,
    };
    watchlists::update(deps.pool, user_id, id, name.as_deref(), position).await?;
    one_answer(deps, user_id, id, now).await
}

pub async fn delete_answer(deps: &Deps<'_>, user_id: i64, id: i64) -> Result<(), Fail> {
    watchlists::delete(deps.pool, user_id, id).await?;
    Ok(())
}

/// The keys list `id` of `user_id` holds now, as stored; none for a list
/// that is not theirs (the store's write then says so).
async fn held(deps: &Deps<'_>, user_id: i64, id: i64) -> Result<Vec<String>, Fail> {
    Ok(watchlists::items_of(deps.pool, user_id, id).await.map_err(read_failed)?.into_iter().map(|item| item.key).collect())
}

/// `keys` as the whole of list `id`: those it holds already kept as they
/// are (so an expired contract in it never blocks a reorder), the new ones
/// resolved against the catalog.
pub async fn put_items_answer(deps: &Deps<'_>, user_id: i64, id: i64, keys: &[String], now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let keys = clean_keys(keys)?;
    let keys = resolve(deps.catalog, &keys, &held(deps, user_id, id).await?).await?;
    watchlists::replace_items(deps.pool, user_id, id, &keys, &stamp(now)).await?;
    one_answer(deps, user_id, id, now).await
}

/// `key` (a price-map key or a catalog id) added at the end of list `id`;
/// `false` when the list held it already.
pub async fn add_item_answer(deps: &Deps<'_>, user_id: i64, id: i64, key: &str, now: DateTime<FixedOffset>) -> Result<(bool, OneOut), Fail> {
    let keys = resolve(deps.catalog, &clean_keys(&[key.to_string()])?, &[]).await?;
    let Some(key) = keys.first() else { return Err(Fail::Bad("key is required".into())) };
    let added = watchlists::add_item(deps.pool, user_id, id, key, &stamp(now)).await?;
    Ok((added, one_answer(deps, user_id, id, now).await?))
}

/// `key` taken out of list `id`. A key the list holds is removed as it is
/// stored, typed as its price-map key or as its catalog id, without asking
/// the catalog, so an expired contract can always be removed; any other is
/// removed under the price-map key the catalog gives it (an index's id),
/// and one the catalog does not know either is not in the list.
pub async fn remove_item_answer(deps: &Deps<'_>, user_id: i64, id: i64, key: &str, now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let cleaned = clean_keys(&[key.to_string()])?;
    let Some(typed) = cleaned.first() else { return Err(Fail::Bad("key is required".into())) };
    let key = match resolve(deps.catalog, &cleaned, &held(deps, user_id, id).await?).await {
        Ok(keys) => keys.into_iter().next().unwrap_or_else(|| typed.clone()),
        // Not held and unknown: the store's answer says whose list it is.
        Err(Fail::UnknownKeys(_)) => typed.clone(),
        Err(e) => return Err(e),
    };
    if !watchlists::remove_item(deps.pool, user_id, id, &key).await? {
        return Err(Fail::NotFound("that instrument is not in this watchlist"));
    }
    one_answer(deps, user_id, id, now).await
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// The 404 every route answers while the `watchlists` switch is off.
fn off_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "Watchlists are off — the admin turns them on with the `watchlists` switch (PUT /api/v2/flags)" })),
    )
        .into_response()
}

/// `Some(404)` while the `watchlists` switch is off.
async fn gate(state: &WatchlistState) -> Option<Response> {
    if state.flags.read().await.watchlists {
        None
    } else {
        Some(off_response())
    }
}

/// The caller's user id: the admin's or a user's. The policy lets only a
/// sign-in through; anyone else is refused here too.
fn caller(identity: &Identity) -> Result<i64, Response> {
    identity
        .user_id()
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Sign in to keep watchlists" }))).into_response())
}

fn deps(state: &WatchlistState) -> Deps<'_> {
    Deps { pool: &state.pool, catalog: &state.catalog, hub: &state.hub }
}

/// `GET /api/v2/watchlists` — every list of the caller's with its items,
/// each with the catalog's row and its quote. A first call makes the
/// default list (`Watchlist 1`: NIFTY, BANKNIFTY, SENSEX).
pub async fn list_handler(State(state): State<WatchlistState>, Extension(identity): Extension<Identity>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match all_answer(&deps(&state), user_id, shared_domain::now_ist()).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /api/v2/watchlists`'s body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewList {
    pub name: String,
}

/// `POST /api/v2/watchlists {name}` — a new, empty list at the end. 201;
/// 409 at five lists.
pub async fn create_handler(State(state): State<WatchlistState>, Extension(identity): Extension<Identity>, Json(body): Json<NewList>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match create_answer(&deps(&state), user_id, &body.name, shared_domain::now_ist()).await {
        Ok(out) => (StatusCode::CREATED, Json(out)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `PATCH /api/v2/watchlists/:id {name?, position?}` — rename and/or
/// move a list.
pub async fn patch_handler(
    State(state): State<WatchlistState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Json(patch): Json<ListPatch>,
) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match patch_answer(&deps(&state), user_id, id, &patch, shared_domain::now_ist()).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /api/v2/watchlists/:id` — a list and its items; 409 for the
/// caller's only list.
pub async fn delete_handler(State(state): State<WatchlistState>, Extension(identity): Extension<Identity>, Path(id): Path<i64>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match delete_answer(&deps(&state), user_id, id).await {
        Ok(()) => Json(json!({ "deleted": true, "id": id })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `PUT /api/v2/watchlists/:id/items`'s body: the whole list, in order.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemsBody {
    pub keys: Vec<String>,
}

/// `PUT /api/v2/watchlists/:id/items {keys}` — the whole ordered list:
/// what is left out goes, what is new is added, the rest is reordered.
pub async fn put_items_handler(
    State(state): State<WatchlistState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Json(body): Json<ItemsBody>,
) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match put_items_answer(&deps(&state), user_id, id, &body.keys, shared_domain::now_ist()).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /api/v2/watchlists/:id/items/:key` — one key added at the end.
/// 201 when added, 200 when the list held it already; 409 at fifty.
pub async fn add_item_handler(
    State(state): State<WatchlistState>,
    Extension(identity): Extension<Identity>,
    Path((id, key)): Path<(i64, String)>,
) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match add_item_answer(&deps(&state), user_id, id, &key, shared_domain::now_ist()).await {
        Ok((true, out)) => (StatusCode::CREATED, Json(out)).into_response(),
        Ok((false, out)) => Json(out).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /api/v2/watchlists/:id/items/:key` — one key taken out; 404
/// when the list does not hold it.
pub async fn remove_item_handler(
    State(state): State<WatchlistState>,
    Extension(identity): Extension<Identity>,
    Path((id, key)): Path<(i64, String)>,
) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let user_id = match caller(&identity) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match remove_item_answer(&deps(&state), user_id, id, &key, shared_domain::now_ist()).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use axum::Router;
    use chrono::NaiveDate;
    use dashmap::DashMap;
    use tower::ServiceExt;

    use super::*;
    use crate::db::{init_db, test_support::TempDb};
    use crate::instruments::load::Kind;
    use crate::instruments::tests::Scratch;
    use crate::quotes::hub::Source;

    fn ist(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .single()
            .unwrap()
    }

    /// A trades.db, the fixture catalog and a hub whose every feed tick is
    /// at `tick`, over `prices`.
    struct World {
        _db: TempDb,
        _scratch: Scratch,
        pool: SqlitePool,
        catalog: Arc<Catalog>,
        hub: Arc<QuoteHub>,
    }

    impl World {
        async fn new(name: &str, prices: Arc<DashMap<String, f64>>, tick: DateTime<FixedOffset>) -> World {
            let db = TempDb::new(&format!("watchlist-routes-{name}"));
            let pool = init_db(&db.url()).await;
            let scratch = Scratch::new(&format!("watchlists-{name}"));
            scratch.write_all();
            let catalog = scratch.catalog().await;
            catalog.load_if_changed("test").await.unwrap_or_else(|e| panic!("{e}"));
            let at = tick.timestamp_millis() as f64 / 1000.0;
            let hub = Arc::new(QuoteHub::with_feed(prices, Box::new(|_: &str| None), Box::new(move |_: &str| Some(at))));
            World { _db: db, _scratch: scratch, pool, catalog, hub }
        }

        fn deps(&self) -> Deps<'_> {
            Deps { pool: &self.pool, catalog: &self.catalog, hub: &self.hub }
        }

        /// The real handlers on a `Router`, at the paths `main` mounts them
        /// on, over this world and `flags`; every request is `identity`'s,
        /// as the auth middleware would have marked it.
        fn app(&self, flags: Arc<RwLock<FeatureFlags>>, identity: Identity) -> Router {
            Router::new()
                .route("/api/v2/watchlists", get(list_handler).post(create_handler))
                .route("/api/v2/watchlists/:id", axum::routing::patch(patch_handler).delete(delete_handler))
                .route("/api/v2/watchlists/:id/items", axum::routing::put(put_items_handler))
                .route("/api/v2/watchlists/:id/items/:key", post(add_item_handler).delete(remove_item_handler))
                .layer(Extension(identity))
                .with_state(WatchlistState { flags, pool: self.pool.clone(), catalog: Arc::clone(&self.catalog), hub: Arc::clone(&self.hub) })
        }

        /// `n` distinct instruments the catalog has rows for, as price-map
        /// keys, `nse_cm|2885` first: the fixture's cash rows and indices,
        /// then its contracts that are live on 2026-11-04.
        async fn known_keys(&self, n: usize) -> Vec<String> {
            let mut candidates = vec!["nse_cm|2885".to_string()];
            candidates.extend(self.catalog.cash_keys());
            for token in [
                52001, 52002, 52003, 52004, 52005, 35013, 35014, 44001, 44002, 41222, 41223, 41224, 41225, 41226, 41227, 41228, 41229, 41230, 43001, 43002,
                45001, 45002, 46001, 46002, 47001, 47002, 48001, 48002, 49001, 50001,
            ] {
                candidates.push(format!("nse_fo|{token}"));
            }
            for token in [874001, 874002, 874003, 875001] {
                candidates.push(format!("bse_fo|{token}"));
            }
            let rows = self.catalog.rows_for_keys(&candidates).await.unwrap();
            let mut known: Vec<String> = Vec::new();
            for key in &candidates {
                if let Some(row) = rows.get(key) {
                    if !known.contains(&row.ws_key) {
                        known.push(row.ws_key.clone());
                    }
                }
            }
            assert!(known.len() >= n, "the fixture catalog resolves {} instruments; {n} are needed", known.len());
            known.truncate(n);
            known
        }
    }

    fn keys(list: &WatchlistOut) -> Vec<&str> {
        list.items.iter().map(|i| i.key.as_str()).collect()
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_first_call_makes_the_default_list_and_prices_its_rows() {
        let noon = ist(2026, 11, 4, 12, 0);
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        prices.insert("idx|NIFTY 50".into(), 25_000.0);
        prices.insert("idx_close|NIFTY 50".into(), 24_900.0);
        let world = World::new("default", Arc::clone(&prices), noon).await;
        let deps = world.deps();

        let out = all_answer(&deps, 7, noon).await.unwrap();
        assert_eq!((out.at, out.market_open, out.max_lists, out.max_items, out.max_name_chars), (noon.timestamp_millis(), true, 5, 50, 40));
        assert_eq!(out.watchlists.len(), 1);
        let list = &out.watchlists[0];
        assert_eq!((list.name.as_str(), list.position, list.created_at.as_str()), ("Watchlist 1", 0, "2026-11-04 12:00:00"));
        assert_eq!(keys(list), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX"]);
        assert_eq!(list.items.iter().map(|i| i.position).collect::<Vec<_>>(), vec![0, 1, 2]);
        // The catalog labels each row; the feed prices the index at once.
        let nifty = &list.items[0];
        let row = nifty.instrument.as_ref().unwrap();
        assert_eq!((row.id.as_str(), row.symbol.as_str(), row.exchange.as_str(), row.kind), ("nse_cm:Nifty 50", "NIFTY", "NSE", Kind::Index));
        let quote = nifty.quote.as_ref().unwrap();
        assert_eq!((quote.ltp, quote.change, quote.source), (25_000.0, Some(100.0), Source::Feed));
        assert_eq!(nifty.age_secs, Some(0));
        let banknifty = &list.items[1];
        assert_eq!(banknifty.instrument.as_ref().map(|r| r.symbol.as_str()), Some("BANKNIFTY"));
        assert!(banknifty.quote.is_none() && banknifty.age_secs.is_none(), "no source has it yet");
        assert_eq!(list.items[2].instrument.as_ref().map(|r| r.exchange.as_str()), Some("BSE"));
        // Every key is wanted now, so the poller fetches it.
        let mut watched = world.hub.watched_keys();
        watched.sort();
        assert_eq!(watched, vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX"]);
        // A second call finds the same one list, not another default.
        let again = all_answer(&deps, 7, ist(2026, 11, 4, 12, 1)).await.unwrap();
        assert_eq!(again.watchlists.len(), 1);
        assert_eq!(again.watchlists[0].id, list.id);
        // An older tick ages; after hours the close, with its day, prices the row.
        let later = ist(2026, 11, 4, 12, 0) + chrono::Duration::seconds(42);
        let again = all_answer(&deps, 7, later).await.unwrap();
        assert_eq!(again.watchlists[0].items[0].age_secs, Some(42));

        // The JSON the front end reads.
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["watchlists"][0]["items"][0]["key"], "idx|NIFTY 50");
        assert_eq!(json["watchlists"][0]["items"][0]["instrument"]["id"], "nse_cm:Nifty 50");
        assert_eq!(json["watchlists"][0]["items"][0]["quote"]["source"], "live");
        assert_eq!(json["watchlists"][0]["items"][0]["quote"]["per_change"].as_f64().map(|p| (p * 1000.0).round() / 1000.0), Some(0.402));
        assert!(json["watchlists"][0]["items"][1]["quote"].is_null());
        assert!(json["watchlists"][0]["items"][1]["age_secs"].is_null());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn keys_are_checked_against_the_catalog_and_kept_as_price_map_keys() {
        let noon = ist(2026, 11, 4, 12, 0);
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        prices.insert("nse_cm|2885".into(), 1_400.0);
        let world = World::new("keys", Arc::clone(&prices), noon).await;
        let deps = world.deps();
        let id = all_answer(&deps, 7, noon).await.unwrap().watchlists[0].id;

        // An id and its key name the same row; a contract comes with its details.
        let body: Vec<String> = ["nse_cm:2885", "nse_cm|2885", " nse_fo|41222 ", "idx|SENSEX", ""].iter().map(|k| k.to_string()).collect();
        let out = put_items_answer(&deps, 7, id, &body, noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["nse_cm|2885", "nse_fo|41222", "idx|SENSEX"]);
        let reliance = &out.watchlist.items[0];
        assert_eq!(reliance.instrument.as_ref().map(|r| (r.symbol.as_str(), r.kind, r.has_fo)), Some(("RELIANCE", Kind::Eq, true)));
        assert_eq!(reliance.quote.as_ref().map(|q| q.ltp), Some(1_400.0));
        let ce = out.watchlist.items[1].instrument.as_ref().unwrap();
        assert_eq!((ce.trading_symbol.as_str(), ce.strike, ce.expiry.as_deref(), ce.lot_size), ("RELIANCE26NOV1400CE", Some(1_400.0), Some("2026-11-24"), 500));
        assert!(out.watchlist.items[1].quote.is_none(), "a contract is never priced before the poller's first read");

        // Unknown keys are named, and nothing changes. The index's REST
        // spelling is not its key, as in `/api/v2/quotes`.
        let bad: Vec<String> = ["nse_cm|2885", "nse_fo|999999", "junk", "nse_cm|Nifty 50"].iter().map(|k| k.to_string()).collect();
        assert_eq!(
            put_items_answer(&deps, 7, id, &bad, noon).await.unwrap_err(),
            Fail::UnknownKeys(vec!["nse_fo|999999".into(), "junk".into(), "nse_cm|Nifty 50".into()])
        );
        assert_eq!(keys(&one_answer(&deps, 7, id, noon).await.unwrap().watchlist), vec!["nse_cm|2885", "nse_fo|41222", "idx|SENSEX"]);
        assert_eq!(add_item_answer(&deps, 7, id, "nse_fo|999999", noon).await.unwrap_err(), Fail::UnknownKeys(vec!["nse_fo|999999".into()]));

        // One at a time, by id or key; a repeat is not a change.
        let (added, out) = add_item_answer(&deps, 7, id, "nse_cm:11536", noon).await.unwrap();
        assert!(added);
        assert_eq!(keys(&out.watchlist), vec!["nse_cm|2885", "nse_fo|41222", "idx|SENSEX", "nse_cm|11536"]);
        let (added, out) = add_item_answer(&deps, 7, id, "nse_cm|11536", noon).await.unwrap();
        assert!(!added);
        assert_eq!(out.watchlist.items.len(), 4);
        let out = remove_item_answer(&deps, 7, id, "nse_cm:2885", noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["nse_fo|41222", "idx|SENSEX", "nse_cm|11536"]);
        assert_eq!(out.watchlist.items.iter().map(|i| i.position).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(remove_item_answer(&deps, 7, id, "nse_cm|2885", noon).await.unwrap_err(), Fail::NotFound("that instrument is not in this watchlist"));
        // A key the catalog has dropped (an expired contract) still comes out.
        sqlx::query("INSERT INTO user_watchlist_items (watchlist_id, key, position, added_at) VALUES (?, 'nse_fo|1', 3, 'x')")
            .bind(id)
            .execute(&world.pool)
            .await
            .unwrap();
        let out = one_answer(&deps, 7, id, noon).await.unwrap();
        assert!(out.watchlist.items[3].instrument.is_none());
        let out = remove_item_answer(&deps, 7, id, "nse_fo|1", noon).await.unwrap();
        assert_eq!(out.watchlist.items.len(), 3);
        // Only keys answered with a catalog row are polled: never a refused
        // one, nor a kept key the catalog has dropped (`nse_fo|1` above).
        let watched = world.hub.watched_keys();
        for key in ["idx|SENSEX", "nse_cm|11536", "nse_cm|2885", "nse_fo|41222", "idx|NIFTY 50"] {
            assert!(watched.iter().any(|k| k == key), "{key} is not wanted");
        }
        for key in ["nse_fo|999999", "junk", "nse_cm|Nifty 50", "nse_fo|1"] {
            assert!(!watched.iter().any(|k| k == key), "{key} has no catalog row, yet is wanted");
        }
    }

    /// Puts `key` at the end of list `id` straight into the table, as a key
    /// the catalog resolved once and has since dropped (an expired weekly).
    async fn hold_expired(pool: &SqlitePool, id: i64, key: &str, added_at: &str) {
        sqlx::query(
            "INSERT INTO user_watchlist_items (watchlist_id, key, position, added_at)
             SELECT ?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM user_watchlist_items WHERE watchlist_id = ?1), 0), ?3",
        )
        .bind(id)
        .bind(key)
        .bind(added_at)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A list holding a contract the catalog has dropped can still be
    /// reordered (the held key is kept as it is, typed as its key or its
    /// catalog id) and that row removed by either form; a new unknown key
    /// is still refused, named alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_list_holding_an_expired_contract_can_be_reordered_and_emptied() {
        let noon = ist(2026, 11, 4, 12, 0);
        let world = World::new("expired", Arc::new(DashMap::new()), noon).await;
        let deps = world.deps();
        let id = all_answer(&deps, 7, noon).await.unwrap().watchlists[0].id;
        hold_expired(&world.pool, id, "nse_fo|1", "2026-10-27 10:00:00").await;
        assert_eq!(keys(&one_answer(&deps, 7, id, noon).await.unwrap().watchlist), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX", "nse_fo|1"]);

        // A Move: every key of the list, the expired one among them, in a new order.
        let moved: Vec<String> = ["nse_fo|1", "idx|SENSEX", "idx|NIFTY 50", "idx|NIFTY BANK"].iter().map(|k| k.to_string()).collect();
        let out = put_items_answer(&deps, 7, id, &moved, noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), moved.iter().map(String::as_str).collect::<Vec<_>>());
        let expired = &out.watchlist.items[0];
        assert!(expired.instrument.is_none() && expired.quote.is_none());
        assert_eq!((expired.position, expired.added_at.as_str()), (0, "2026-10-27 10:00:00"), "kept as it was");

        // Held keys typed as catalog ids name the held rows; a new one by id is resolved.
        let typed: Vec<String> = ["idx|NIFTY 50", "nse_fo:1", "nse_cm:2885", "idx|SENSEX"].iter().map(|k| k.to_string()).collect();
        let out = put_items_answer(&deps, 7, id, &typed, noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["idx|NIFTY 50", "nse_fo|1", "nse_cm|2885", "idx|SENSEX"]);
        assert_eq!(out.watchlist.items[2].instrument.as_ref().map(|r| r.symbol.as_str()), Some("RELIANCE"));

        // A new key the catalog has no row for is still refused, named
        // alone (never the held one), and nothing changes.
        let with_new: Vec<String> = ["nse_fo|1", "nse_fo|999999", "idx|NIFTY 50", "nse_cm|2885", "idx|SENSEX"].iter().map(|k| k.to_string()).collect();
        assert_eq!(put_items_answer(&deps, 7, id, &with_new, noon).await.unwrap_err(), Fail::UnknownKeys(vec!["nse_fo|999999".into()]));
        assert_eq!(keys(&one_answer(&deps, 7, id, noon).await.unwrap().watchlist), vec!["idx|NIFTY 50", "nse_fo|1", "nse_cm|2885", "idx|SENSEX"]);
        // Someone else's list does not hold it: there it is unknown.
        assert_eq!(put_items_answer(&deps, 8, id, &["nse_fo|1".to_string()], noon).await.unwrap_err(), Fail::UnknownKeys(vec!["nse_fo|1".into()]));

        // Once left out, a dropped contract cannot come back.
        let out = put_items_answer(&deps, 7, id, &["idx|NIFTY 50".to_string(), "nse_cm|2885".to_string()], noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["idx|NIFTY 50", "nse_cm|2885"]);
        let back: Vec<String> = ["idx|NIFTY 50", "nse_cm|2885", "nse_fo|1"].iter().map(|k| k.to_string()).collect();
        assert_eq!(put_items_answer(&deps, 7, id, &back, noon).await.unwrap_err(), Fail::UnknownKeys(vec!["nse_fo|1".into()]));

        // Removed by its catalog id: the row is kept as `nse_fo|51999`, the
        // id typed is `nse_fo:51999`, and the catalog has neither.
        hold_expired(&world.pool, id, "nse_fo|51999", "2026-10-27 10:00:00").await;
        let out = remove_item_answer(&deps, 7, id, "nse_fo:51999", noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["idx|NIFTY 50", "nse_cm|2885"]);
        assert_eq!(remove_item_answer(&deps, 7, id, "nse_fo:51999", noon).await.unwrap_err(), Fail::NotFound("that instrument is not in this watchlist"));
        // A live instrument by its id, and an index by its (non-key) id.
        let out = remove_item_answer(&deps, 7, id, "nse_cm:2885", noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["idx|NIFTY 50"]);
        let out = remove_item_answer(&deps, 7, id, "nse_cm:Nifty 50", noon).await.unwrap();
        assert!(out.watchlist.items.is_empty());
        // Through the route, with the id in the path.
        hold_expired(&world.pool, id, "nse_fo|51999", "2026-10-27 10:00:00").await;
        let flags = Arc::new(RwLock::new(FeatureFlags { watchlists: true, ..Default::default() }));
        let app = world.app(flags, Identity::User { user_id: 7 });
        let uri = format!("/api/v2/watchlists/{id}/items/nse_fo:51999");
        let response = app.oneshot(Request::builder().method("DELETE").uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_json(response).await["watchlist"]["items"].as_array().unwrap().is_empty());

        // The dropped contracts were never polled.
        let watched = world.hub.watched_keys();
        for key in ["nse_fo|1", "nse_fo|51999"] {
            assert!(!watched.iter().any(|k| k == key), "{key} has no catalog row, yet is wanted");
        }
    }

    /// The front end asks for every list every 2 s: once a person has a
    /// list, that read never waits for trades.db's write lock, which the
    /// engine's `db_writer` or the paper desk may hold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_poll_never_waits_for_the_write_lock() {
        let noon = ist(2026, 11, 4, 12, 0);
        let world = World::new("poll-lock", Arc::new(DashMap::new()), noon).await;
        let deps = world.deps();
        let id = all_answer(&deps, 7, noon).await.unwrap().watchlists[0].id;

        // Another connection is mid-write (the busy timeout is 5 s).
        let mut writer = world.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE wallet SET balance = balance WHERE id = 1").execute(&mut *writer).await.unwrap();
        let polled = tokio::time::timeout(std::time::Duration::from_secs(2), all_answer(&deps, 7, noon)).await;
        let out = polled.expect("the poll waited for the write lock").unwrap();
        assert_eq!((out.watchlists.len(), out.watchlists[0].id), (1, id));
        assert_eq!(keys(&out.watchlists[0]), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX"]);
        writer.rollback().await.unwrap();

        // A first call still makes the default list, once.
        let first = all_answer(&deps, 8, noon).await.unwrap();
        assert_eq!((first.watchlists.len(), first.watchlists[0].name.as_str()), (1, "Watchlist 1"));
        assert_eq!(all_answer(&deps, 8, noon).await.unwrap().watchlists[0].id, first.watchlists[0].id);
    }

    /// A catalog database that cannot be read is a 500 in general terms:
    /// never "unknown keys" for instruments it has, nor rows shown as if
    /// their instrument had expired. A held key still comes out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_broken_catalog_is_a_500_never_unknown_keys() {
        let noon = ist(2026, 11, 4, 12, 0);
        let world = World::new("broken-catalog", Arc::new(DashMap::new()), noon).await;
        let deps = world.deps();
        let id = all_answer(&deps, 7, noon).await.unwrap().watchlists[0].id;
        add_item_answer(&deps, 7, id, "nse_fo|41222", noon).await.unwrap();

        world.catalog.close_db_for_tests().await;
        let broken = Fail::Server("could not read the instrument catalog");
        assert_eq!(put_items_answer(&deps, 7, id, &["nse_fo|41223".to_string()], noon).await.unwrap_err(), broken);
        assert_eq!(add_item_answer(&deps, 7, id, "nse_fo:41223", noon).await.unwrap_err(), broken);
        assert_eq!(remove_item_answer(&deps, 7, id, "nse_fo:41223", noon).await.unwrap_err(), broken, "not held: only the catalog can say");
        assert_eq!(all_answer(&deps, 7, noon).await.unwrap_err(), broken);
        assert_eq!(one_answer(&deps, 7, id, noon).await.unwrap_err(), broken);
        let held: Vec<String> = watchlists::items_of(&world.pool, 7, id).await.unwrap().into_iter().map(|i| i.key).collect();
        assert_eq!(held, vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX", "nse_fo|41222"], "nothing changed");

        // Through the route: a 500 with the general message.
        let flags = Arc::new(RwLock::new(FeatureFlags { watchlists: true, ..Default::default() }));
        let app = world.app(flags, Identity::User { user_id: 7 });
        let response = app.clone().oneshot(Request::builder().method("GET").uri("/api/v2/watchlists").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_json(response).await["error"], "could not read the instrument catalog");
        let put = Request::builder()
            .method("PUT")
            .uri(format!("/api/v2/watchlists/{id}/items"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"keys": ["nse_fo|41223"]}"#))
            .unwrap();
        let response = app.clone().oneshot(put).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body_json(response).await.get("unknown").is_none());

        // The held contract comes out without the catalog; what is left
        // (indices, from the search list in memory) is answered.
        let out = remove_item_answer(&deps, 7, id, "nse_fo|41222", noon).await.unwrap();
        assert_eq!(keys(&out.watchlist), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_caps_and_the_name_rules_hold_in_the_route() {
        let noon = ist(2026, 11, 4, 12, 0);
        let world = World::new("caps", Arc::new(DashMap::new()), noon).await;
        let deps = world.deps();
        let first = all_answer(&deps, 7, noon).await.unwrap().watchlists[0].id;

        // Names: trimmed, not blank, not too long.
        assert_eq!(create_answer(&deps, 7, "   ", noon).await.unwrap_err(), Fail::Bad("name is required".into()));
        assert!(matches!(create_answer(&deps, 7, &"x".repeat(41), noon).await.unwrap_err(), Fail::Bad(_)));
        let out = create_answer(&deps, 7, "  Banks ", noon).await.unwrap();
        assert_eq!((out.watchlist.name.as_str(), out.watchlist.position, out.watchlist.items.len()), ("Banks", 1, 0));
        for n in 3..=5 {
            create_answer(&deps, 7, &format!("List {n}"), noon).await.unwrap();
        }
        assert!(matches!(create_answer(&deps, 7, "Sixth", noon).await.unwrap_err(), Fail::Conflict(_)));
        assert_eq!(all_answer(&deps, 7, noon).await.unwrap().watchlists.len(), 5);

        // Items: a hundred and one typed keys are refused before anything is
        // read. Fifty-one typed keys naming fifty instruments (one as its
        // id and as its key) are fifty, and kept; fifty-one instruments are
        // too many, and nothing changes.
        let banks = out.watchlist.id;
        let too_many: Vec<String> = (1..=101).map(|i| format!("nse_cm|{i}")).collect();
        assert!(matches!(put_items_answer(&deps, 7, banks, &too_many, noon).await.unwrap_err(), Fail::Bad(m) if m.contains("at most 50") && m.contains("101 given")));
        assert!(matches!(clean_keys(&too_many).unwrap_err(), Fail::Bad(m) if m.contains("101 given")));
        assert!(matches!(clean_keys(&[" x".repeat(40)]).unwrap_err(), Fail::Bad(_)));
        assert_eq!(clean_keys(&["a".into(), " ".into(), "a".into(), "b ".into()]).unwrap(), vec!["a", "b"]);
        let known = world.known_keys(51).await;
        assert_eq!(known[0], "nse_cm|2885");
        let mut fifty_one_typed = vec!["nse_cm:2885".to_string()];
        fifty_one_typed.extend(known[..50].iter().cloned());
        assert_eq!((fifty_one_typed.len(), clean_keys(&fifty_one_typed).unwrap().len()), (51, 51), "two forms of one instrument pass the cheap check");
        let full = put_items_answer(&deps, 7, banks, &fifty_one_typed, noon).await.unwrap();
        assert_eq!(keys(&full.watchlist), known[..50].iter().map(String::as_str).collect::<Vec<_>>());
        assert!(matches!(put_items_answer(&deps, 7, banks, &known, noon).await.unwrap_err(), Fail::Bad(m) if m.contains("at most 50") && m.contains("51 given")));
        assert_eq!(one_answer(&deps, 7, banks, noon).await.unwrap().watchlist.items.len(), 50, "a refused replace changes nothing");
        assert_eq!(add_item_answer(&deps, 7, banks, &known[50], noon).await.unwrap_err(), Fail::from(Error::TooManyItems), "the fifty-first one at a time is the store's 409");

        // Positions: 0 to the last list.
        let out = patch_answer(&deps, 7, out.watchlist.id, &ListPatch { name: None, position: Some(0) }, noon).await.unwrap();
        assert_eq!((out.watchlist.name.as_str(), out.watchlist.position), ("Banks", 0));
        assert_eq!(all_answer(&deps, 7, noon).await.unwrap().watchlists.iter().map(|l| l.position).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
        assert!(matches!(patch_answer(&deps, 7, first, &ListPatch { name: None, position: Some(-1) }, noon).await.unwrap_err(), Fail::Bad(_)));
        assert!(matches!(patch_answer(&deps, 7, first, &ListPatch { name: None, position: Some(5) }, noon).await.unwrap_err(), Fail::Bad(_)));
        assert!(matches!(patch_answer(&deps, 7, first, &ListPatch { name: Some("".into()), position: None }, noon).await.unwrap_err(), Fail::Bad(_)));
        let renamed = patch_answer(&deps, 7, first, &ListPatch { name: Some("Indices".into()), position: None }, noon).await.unwrap();
        assert_eq!((renamed.watchlist.name.as_str(), renamed.watchlist.position, renamed.watchlist.items.len()), ("Indices", 1, 3));
        // Deleting down to one, and never the last.
        for list in all_answer(&deps, 7, noon).await.unwrap().watchlists.iter().skip(1) {
            delete_answer(&deps, 7, list.id).await.unwrap();
        }
        let left = all_answer(&deps, 7, noon).await.unwrap().watchlists;
        assert_eq!((left.len(), left[0].name.as_str(), left[0].position), (1, "Banks", 0));
        assert!(matches!(delete_answer(&deps, 7, left[0].id).await.unwrap_err(), Fail::Conflict(_)));
        assert_eq!(delete_answer(&deps, 7, first).await.unwrap_err(), Fail::NotFound("no such watchlist"));
    }

    /// The real handlers over a `Router`, as `main` mounts them: while the
    /// `watchlists` switch is off every route is a 404 naming the switch,
    /// and nothing is read or made; flipping it needs no restart.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_switch_gates_every_route_with_a_404() {
        let noon = ist(2026, 11, 4, 12, 0);
        let world = World::new("switch", Arc::new(DashMap::new()), noon).await;
        let flags = Arc::new(RwLock::new(FeatureFlags::default()));
        assert!(!flags.read().await.watchlists, "off by default");
        let app = world.app(Arc::clone(&flags), Identity::User { user_id: 7 });
        let request = |method: &str, uri: &str, body: Option<&str>| {
            let builder = Request::builder().method(method).uri(uri);
            match body {
                Some(json) => builder.header("content-type", "application/json").body(Body::from(json.to_string())).unwrap(),
                None => builder.body(Body::empty()).unwrap(),
            }
        };
        let get_all = || request("GET", "/api/v2/watchlists", None);
        let post_one = || request("POST", "/api/v2/watchlists", Some(r#"{"name": "Banks"}"#));

        // Off: 404 with the reason on every route, and no list is made.
        for (method, uri, body) in [
            ("GET", "/api/v2/watchlists", None),
            ("POST", "/api/v2/watchlists", Some(r#"{"name": "Banks"}"#)),
            ("PATCH", "/api/v2/watchlists/3", Some(r#"{"name": "Banks"}"#)),
            ("DELETE", "/api/v2/watchlists/3", None),
            ("PUT", "/api/v2/watchlists/3/items", Some(r#"{"keys": ["nse_cm|2885"]}"#)),
            ("POST", "/api/v2/watchlists/3/items/nse_cm%7C2885", None),
            ("DELETE", "/api/v2/watchlists/3/items/nse_cm%7C2885", None),
        ] {
            let response = app.clone().oneshot(request(method, uri, body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
            let json = body_json(response).await;
            assert!(json["error"].as_str().unwrap().contains("`watchlists` switch"), "{method} {uri}: {json}");
        }
        assert!(watchlists::lists(&world.pool, 7).await.unwrap().is_empty(), "the gate answered before the default list was made");

        // On: the same requests are answered.
        flags.write().await.watchlists = true;
        let response = app.clone().oneshot(get_all()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["watchlists"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["watchlists"][0]["name"], "Watchlist 1");
        assert_eq!(json["watchlists"][0]["items"][0]["key"], "idx|NIFTY 50");
        let response = app.clone().oneshot(post_one()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let json = body_json(response).await;
        assert_eq!((json["watchlist"]["name"].as_str(), json["watchlist"]["position"].as_i64()), (Some("Banks"), Some(1)));
        assert_eq!(watchlists::lists(&world.pool, 7).await.unwrap().len(), 2);

        // Off again: gone again, and the lists stay as they are.
        flags.write().await.watchlists = false;
        assert_eq!(app.clone().oneshot(get_all()).await.unwrap().status(), StatusCode::NOT_FOUND);
        assert_eq!(app.clone().oneshot(post_one()).await.unwrap().status(), StatusCode::NOT_FOUND);
        assert_eq!(watchlists::lists(&world.pool, 7).await.unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_person_cannot_read_or_edit_anothers_lists() {
        let noon = ist(2026, 11, 4, 12, 0);
        let world = World::new("isolation", Arc::new(DashMap::new()), noon).await;
        let deps = world.deps();
        // The admin (the owner's user row, id 1) and two users.
        let admin = Identity::Admin { user_id: 1 };
        let alice = Identity::User { user_id: 2 };
        let bob = Identity::User { user_id: 3 };
        let (admin_id, alice_id, bob_id) = (caller(&admin).unwrap(), caller(&alice).unwrap(), caller(&bob).unwrap());
        assert!(caller(&Identity::Anonymous).is_err());

        let theirs = all_answer(&deps, alice_id, noon).await.unwrap().watchlists[0].id;
        add_item_answer(&deps, alice_id, theirs, "nse_cm|2885", noon).await.unwrap();
        let mine = all_answer(&deps, bob_id, noon).await.unwrap();
        assert_eq!(mine.watchlists.len(), 1);
        assert_ne!(mine.watchlists[0].id, theirs);
        assert_eq!(keys(&mine.watchlists[0]), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX"]);

        let not_found = Fail::NotFound("no such watchlist");
        assert_eq!(one_answer(&deps, bob_id, theirs, noon).await.unwrap_err(), not_found);
        assert_eq!(patch_answer(&deps, bob_id, theirs, &ListPatch { name: Some("Mine now".into()), position: None }, noon).await.unwrap_err(), not_found);
        assert_eq!(patch_answer(&deps, bob_id, theirs, &ListPatch { name: None, position: Some(0) }, noon).await.unwrap_err(), not_found);
        assert_eq!(delete_answer(&deps, bob_id, theirs).await.unwrap_err(), not_found);
        assert_eq!(put_items_answer(&deps, bob_id, theirs, &["nse_cm|11536".to_string()], noon).await.unwrap_err(), not_found);
        assert_eq!(add_item_answer(&deps, bob_id, theirs, "nse_cm|11536", noon).await.unwrap_err(), not_found);
        assert_eq!(remove_item_answer(&deps, bob_id, theirs, "nse_cm|2885", noon).await.unwrap_err(), not_found);
        // Alice's list is as she left it; the admin's lists are its own.
        let hers = one_answer(&deps, alice_id, theirs, noon).await.unwrap();
        assert_eq!((hers.watchlist.name.as_str(), keys(&hers.watchlist)), ("Watchlist 1", vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX", "nse_cm|2885"]));
        let owners = all_answer(&deps, admin_id, noon).await.unwrap();
        assert_eq!(owners.watchlists.len(), 1);
        assert!(owners.watchlists.iter().all(|l| l.id != theirs && l.id != mine.watchlists[0].id));
        assert_eq!(one_answer(&deps, admin_id, theirs, noon).await.unwrap_err(), not_found);
    }

    #[tokio::test]
    async fn refusals_have_their_status_and_a_reason() {
        let response = off_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(body_json(response).await["error"].as_str().unwrap().contains("watchlists"));
        let response = Fail::UnknownKeys(vec!["junk".into(), "nse_fo|1".into()]).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert_eq!(json["error"], "unknown keys: junk, nse_fo|1");
        assert_eq!(json["unknown"], serde_json::json!(["junk", "nse_fo|1"]));
        assert_eq!(Fail::NotFound("no such watchlist").into_response().status(), StatusCode::NOT_FOUND);
        assert_eq!(Fail::Bad("x".into()).into_response().status(), StatusCode::BAD_REQUEST);
        assert_eq!(Fail::from(Error::TooManyLists).into_response().status(), StatusCode::CONFLICT);
        assert_eq!(Fail::from(Error::TooManyItems).into_response().status(), StatusCode::CONFLICT);
        assert_eq!(Fail::from(Error::LastList).into_response().status(), StatusCode::CONFLICT);
        assert_eq!(Fail::from(Error::BadPosition).into_response().status(), StatusCode::BAD_REQUEST);
        assert_eq!(Fail::from(Error::NotFound), Fail::NotFound("no such watchlist"));
        let response = Fail::from(Error::Db(sqlx::Error::RowNotFound)).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_json(response).await["error"], "could not save the watchlist");
        // Bodies: an unknown field is refused, a known one read.
        assert!(serde_json::from_str::<ListPatch>(r#"{"nmae": "x"}"#).is_err());
        let patch: ListPatch = serde_json::from_str(r#"{"position": 2}"#).unwrap();
        assert_eq!((patch.name, patch.position), (None, Some(2)));
        assert!(serde_json::from_str::<ItemsBody>(r#"{"keys": "nse_cm|2885"}"#).is_err());
    }

    /// The item routes carry the key in the path: `|` and a space arrive
    /// decoded, and the list id as a number.
    #[tokio::test]
    async fn an_item_key_arrives_decoded_from_the_path() {
        let app = Router::new()
            .route("/api/v2/watchlists/:id/items", axum::routing::put(|Path(id): Path<i64>| async move { format!("put {id}") }))
            .route(
                "/api/v2/watchlists/:id/items/:key",
                post(|Path((id, key)): Path<(i64, String)>| async move { format!("post {id} {key}") })
                    .delete(|Path((id, key)): Path<(i64, String)>| async move { format!("delete {id} {key}") }),
            );
        for (method, uri, want) in [
            ("PUT", "/api/v2/watchlists/3/items", "put 3"),
            ("POST", "/api/v2/watchlists/3/items/idx%7CNIFTY%2050", "post 3 idx|NIFTY 50"),
            ("POST", "/api/v2/watchlists/3/items/nse_cm:2885", "post 3 nse_cm:2885"),
            ("DELETE", "/api/v2/watchlists/12/items/nse_fo%7C51386", "delete 12 nse_fo|51386"),
        ] {
            let response = app.clone().oneshot(Request::builder().method(method).uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
            let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(String::from_utf8(bytes.to_vec()).unwrap(), want, "{method} {uri}");
        }
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/v2/watchlists/abc/items/x").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "a list id is a number");
    }
}
