//! `/api/v2/watchlists` — each signed-in person's watchlists
//! (`crate::watchlists`), priced from the quote hub (`crate::quotes`) and
//! labelled from the instrument catalog (`crate::instruments`), behind the
//! `watchlists` feature flag: while it is off every route answers 404.
//!
//! `Access::SignedIn` for every route: the lists are keyed by the phone
//! sign-in's user id, the admin's included (the admin is a `users` row, and
//! `Access::PhoneUser` would refuse it), and every read and write here is
//! scoped to the caller's id, so another person's list is a plain 404.
//! A key is accepted only when the catalog has a row for it, and the row's
//! own price-map key is what the list keeps; an unknown key is a 400 that
//! names it, and is never polled. Every key answered is marked wanted on
//! the hub, so the next call (the front end re-polls every 2 s) finds it
//! priced; a quote never says whether the feed carries its key
//! (`QuoteHub::get_at`). Nothing here reaches the feed task, the bridge,
//! the engine or anyone's positions.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::Identity;
use crate::instruments::{Catalog, SearchRow};
use crate::quotes::{eod, hub::Quote, QuoteHub};
use crate::watchlists::{self, Error, ItemRow, ListRow, MAX_ITEMS, MAX_LISTS, MAX_NAME_CHARS};
use crate::AppState;

/// The longest key a body or path may carry, in characters.
const MAX_KEY_CHARS: usize = 64;

/// What the answers are built from; the handlers take them from
/// `AppState`, the tests build their own.
pub struct Deps<'a> {
    pub pool: &'a SqlitePool,
    pub catalog: &'a Catalog,
    pub hub: &'a QuoteHub,
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

/// IST `YYYY-MM-DD HH:MM:SS` of `now`, as the user tables stamp rows.
fn stamp(now: DateTime<FixedOffset>) -> String {
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `lists` with `items` (every item of those lists, in order) grouped
/// under them, each item with the catalog's row and its quote as of `now`.
/// Every key is marked wanted on the hub.
async fn priced(deps: &Deps<'_>, lists: Vec<ListRow>, items: Vec<ItemRow>, now: DateTime<FixedOffset>) -> Vec<WatchlistOut> {
    let mut keys: Vec<String> = Vec::new();
    for item in &items {
        if !keys.contains(&item.key) {
            keys.push(item.key.clone());
        }
    }
    let rows = deps.catalog.rows_for_keys(&keys).await;
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
    deps.hub.want(keys);
    lists
        .into_iter()
        .map(|list| WatchlistOut {
            items: by_list.remove(&list.id).unwrap_or_default(),
            id: list.id,
            name: list.name,
            position: list.position,
            created_at: list.created_at,
        })
        .collect()
}

/// Every list of `user_id` with its items priced, as of `now`. A person
/// with no list yet gets the default one first.
pub async fn all_answer(deps: &Deps<'_>, user_id: i64, now: DateTime<FixedOffset>) -> Result<AllOut, Fail> {
    if watchlists::ensure_default(deps.pool, user_id, &stamp(now)).await.map_err(read_failed)? {
        tracing::info!(user_id, "Default watchlist created");
    }
    let lists = watchlists::lists(deps.pool, user_id).await.map_err(read_failed)?;
    let items = watchlists::items(deps.pool, user_id).await.map_err(read_failed)?;
    Ok(AllOut {
        at: now.timestamp_millis(),
        market_open: eod::open_at(now),
        max_lists: MAX_LISTS,
        max_items: MAX_ITEMS,
        max_name_chars: MAX_NAME_CHARS,
        watchlists: priced(deps, lists, items, now).await,
    })
}

/// List `id` of `user_id` with its items priced, as of `now`.
pub async fn one_answer(deps: &Deps<'_>, user_id: i64, id: i64, now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let list = watchlists::list(deps.pool, user_id, id).await.map_err(read_failed)?.ok_or(Fail::NotFound("no such watchlist"))?;
    let items = watchlists::items_of(deps.pool, user_id, id).await.map_err(read_failed)?;
    let mut lists = priced(deps, vec![list], items, now).await;
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

/// `keys` trimmed, blanks dropped, repeats kept once, in order; none too
/// long, and at most [`MAX_ITEMS`].
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
    if out.len() > MAX_ITEMS {
        return Err(Fail::Bad(format!("at most {MAX_ITEMS} instruments in a watchlist ({} given)", out.len())));
    }
    Ok(out)
}

/// The price-map key of each of `keys` (a price-map key or a catalog id,
/// as `clean_keys` left them), in order and distinct; [`Fail::UnknownKeys`]
/// naming every key the catalog has no row for.
async fn resolve(catalog: &Catalog, keys: &[String]) -> Result<Vec<String>, Fail> {
    let rows = catalog.rows_for_keys(keys).await;
    let unknown: Vec<String> = keys.iter().filter(|k| !rows.contains_key(*k)).cloned().collect();
    if !unknown.is_empty() {
        if catalog.status().day.is_none() {
            tracing::warn!(?unknown, "v2 watchlists: keys refused — the instrument catalog is not built (the `search` switch builds it)");
        }
        return Err(Fail::UnknownKeys(unknown));
    }
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        let ws_key = &rows[key].ws_key;
        if !out.contains(ws_key) {
            out.push(ws_key.clone());
        }
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

/// `keys`, resolved against the catalog, as the whole of list `id`.
pub async fn put_items_answer(deps: &Deps<'_>, user_id: i64, id: i64, keys: &[String], now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let keys = resolve(deps.catalog, &clean_keys(keys)?).await?;
    watchlists::replace_items(deps.pool, user_id, id, &keys, &stamp(now)).await?;
    one_answer(deps, user_id, id, now).await
}

/// `key` (a price-map key or a catalog id) added at the end of list `id`;
/// `false` when the list held it already.
pub async fn add_item_answer(deps: &Deps<'_>, user_id: i64, id: i64, key: &str, now: DateTime<FixedOffset>) -> Result<(bool, OneOut), Fail> {
    let keys = resolve(deps.catalog, &clean_keys(&[key.to_string()])?).await?;
    let Some(key) = keys.first() else { return Err(Fail::Bad("key is required".into())) };
    let added = watchlists::add_item(deps.pool, user_id, id, key, &stamp(now)).await?;
    Ok((added, one_answer(deps, user_id, id, now).await?))
}

/// `key` taken out of list `id`. A key the catalog knows is removed under
/// its price-map key, whichever form was given; one it no longer knows (an
/// expired contract) under the key as typed, so it can always be removed.
pub async fn remove_item_answer(deps: &Deps<'_>, user_id: i64, id: i64, key: &str, now: DateTime<FixedOffset>) -> Result<OneOut, Fail> {
    let cleaned = clean_keys(&[key.to_string()])?;
    let Some(typed) = cleaned.first() else { return Err(Fail::Bad("key is required".into())) };
    let key = match resolve(deps.catalog, &cleaned).await {
        Ok(keys) => keys.into_iter().next().unwrap_or_else(|| typed.clone()),
        Err(_) => typed.clone(),
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
async fn gate(state: &AppState) -> Option<Response> {
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

fn deps(state: &AppState) -> Deps<'_> {
    Deps { pool: &state.db_pool, catalog: &state.instruments, hub: &state.quotes }
}

/// `GET /api/v2/watchlists` — every list of the caller's with its items,
/// each with the catalog's row and its quote. A first call makes the
/// default list (`Watchlist 1`: NIFTY, BANKNIFTY, SENSEX).
pub async fn list_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>) -> Response {
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
pub async fn create_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Json(body): Json<NewList>) -> Response {
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
    State(state): State<AppState>,
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
pub async fn delete_handler(State(state): State<AppState>, Extension(identity): Extension<Identity>, Path(id): Path<i64>) -> Response {
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
    use axum::routing::post;
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
        hub: QuoteHub,
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
            let hub = QuoteHub::with_feed(prices, Box::new(|_: &str| None), Box::new(move |_: &str| Some(at)));
            World { _db: db, _scratch: scratch, pool, catalog, hub }
        }

        fn deps(&self) -> Deps<'_> {
            Deps { pool: &self.pool, catalog: &self.catalog, hub: &self.hub }
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
        // Only keys answered are polled, never a refused one.
        let watched = world.hub.watched_keys();
        for key in ["idx|SENSEX", "nse_cm|11536", "nse_cm|2885", "nse_fo|1", "nse_fo|41222", "idx|NIFTY 50"] {
            assert!(watched.iter().any(|k| k == key), "{key} is not wanted");
        }
        for key in ["nse_fo|999999", "junk", "nse_cm|Nifty 50"] {
            assert!(!watched.iter().any(|k| k == key), "{key} was refused, yet wanted");
        }
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

        // Items: fifty-one in one PUT is refused before anything is read.
        let too_many: Vec<String> = (1..=51).map(|i| format!("nse_cm|{i}")).collect();
        assert!(matches!(put_items_answer(&deps, 7, first, &too_many, noon).await.unwrap_err(), Fail::Bad(m) if m.contains("at most 50")));
        assert!(matches!(clean_keys(&[" x".repeat(40)]).unwrap_err(), Fail::Bad(_)));
        assert_eq!(clean_keys(&["a".into(), " ".into(), "a".into(), "b ".into()]).unwrap(), vec!["a", "b"]);

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
