//! `GET /api/v2/search`, `GET /api/v2/quotes`, `GET /api/v2/instruments/:id`
//! and `GET /api/v2/instruments/status` — the instrument catalog
//! (`crate::instruments`) and the quote hub (`crate::quotes`) over HTTP,
//! behind the `search` feature flag: while it is off every route answers
//! 404. The first three are for anyone signed in; the status page, with the
//! hub's error text and call times, is the admin's (`auth::policy`).
//! Read-only, and nothing here reaches the engine, the order path or
//! anyone's positions: a quote never says whether the feed carries its key
//! (`QuoteHub::get_at`), and an error is reported in general terms. Every
//! key a route answers is marked wanted on the hub, so its quote is polled
//! for the next 15 s and the client's next call (the front end re-polls
//! every 2 s) finds it priced.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::instruments::{Catalog, CatalogStatus, Instrument, SearchRow, PER_GROUP};
use crate::quotes::{eod, hub::Quote, HubStatus, QuoteHub};
use crate::AppState;

/// Keys one `/api/v2/quotes` call may ask for; a longer list is a 400.
pub const MAX_QUOTE_KEYS: usize = 60;
/// The longest query `/api/v2/search` reads, in characters.
const MAX_QUERY_CHARS: usize = 64;
/// The longest key `/api/v2/quotes` reads, in characters.
const MAX_KEY_CHARS: usize = 64;

/// The 404 every route answers while the `search` switch is off.
fn off_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "Search is off — the admin turns it on with the `search` switch (PUT /api/v2/flags)" })),
    )
        .into_response()
}

/// `Some(404)` while the `search` switch is off.
async fn gate(state: &AppState) -> Option<Response> {
    if state.flags.read().await.search {
        None
    } else {
        Some(off_response())
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message.into() }))).into_response()
}

/// A 500 with a general message: the cause is logged, never answered.
fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("v2 instruments: {what}: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("could not read {what}") }))).into_response()
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchQuery {
    /// A symbol, a name, an ISIN, `<SYM> [<MON>] <STRIKE> CE|PE` or `<SYM> FUT`.
    q: Option<String>,
    /// Rows per group, 1 to 8 (the default).
    limit: Option<String>,
}

/// A search row with its quote, when any source has one.
#[derive(Serialize)]
pub struct PricedRow {
    #[serde(flatten)]
    pub row: SearchRow,
    /// From the feed, Kotak's REST quotes or Upstox during hours; the
    /// 15:35 close (`source: close`, with `as_of`) after them. `None` until
    /// the poller has fetched it — ask again in 2 s.
    pub quote: Option<Quote>,
}

/// `GET /api/v2/search`'s answer: the catalog's groups, each row priced.
#[derive(Serialize)]
pub struct SearchOut {
    pub query: String,
    /// `plain`, `future` or `contract`: how the query was read.
    pub parsed: &'static str,
    /// IST `YYYY-MM-DD` the derivatives are listed from.
    pub today: String,
    /// The IST day of the CSVs the catalog was built from, for a "catalog
    /// from <date>" note when it is not today; `None` before the first build.
    pub catalog_day: Option<String>,
    pub market_open: bool,
    pub indices: Vec<PricedRow>,
    pub stocks: Vec<PricedRow>,
    pub futures: Vec<PricedRow>,
    pub options: Vec<PricedRow>,
}

/// `limit` as rows per group: [`PER_GROUP`] when left out, else the number
/// clamped to 1..=[`PER_GROUP`].
pub fn per_group(raw: Option<&str>) -> Result<usize, &'static str> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(PER_GROUP),
        Some(text) => text.parse::<usize>().map(|n| n.clamp(1, PER_GROUP)).map_err(|_| "limit must be a whole number"),
    }
}

/// Prices `rows` (at most `limit` of them) as of `now` and notes their keys
/// in `keys`, for the hub to poll.
fn priced(rows: Vec<SearchRow>, limit: usize, hub: &QuoteHub, now: DateTime<FixedOffset>, keys: &mut Vec<String>) -> Vec<PricedRow> {
    rows.into_iter()
        .take(limit)
        .map(|row| {
            keys.push(row.ws_key.clone());
            PricedRow { quote: hub.get_at(&row.ws_key, now), row }
        })
        .collect()
}

/// The search answer for `text`, as of `now`: the catalog's groups, each
/// row with its quote, the options listed around the underlying's spot
/// when the hub has one. Every row's key is marked wanted.
pub async fn search_answer(catalog: &Catalog, hub: &QuoteHub, text: &str, limit: usize, now: DateTime<FixedOffset>) -> SearchOut {
    let today = now.date_naive();
    let spot = |symbol: &str| catalog.spot_key(symbol).and_then(|key| hub.get_at(&key, now)).map(|q| q.ltp);
    let answer = catalog.search(text, today, &spot).await;
    let mut keys = Vec::new();
    let out = SearchOut {
        query: answer.query,
        parsed: answer.parsed,
        today: today.format("%Y-%m-%d").to_string(),
        catalog_day: catalog.status().day,
        market_open: eod::open_at(now),
        indices: priced(answer.indices, limit, hub, now, &mut keys),
        stocks: priced(answer.stocks, limit, hub, now, &mut keys),
        futures: priced(answer.futures, limit, hub, now, &mut keys),
        options: priced(answer.options, limit, hub, now, &mut keys),
    };
    hub.want(keys);
    out
}

/// `GET /api/v2/search?q=&limit=` — everything `q` matches, grouped
/// Indices / Stocks / Futures / Options and ranked, each row with its quote
/// when the hub has one.
pub async fn search_handler(State(state): State<AppState>, Query(q): Query<SearchQuery>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let text = q.q.as_deref().map(str::trim).unwrap_or("");
    if text.is_empty() {
        return bad_request("q (what to search for) is required");
    }
    if text.chars().count() > MAX_QUERY_CHARS {
        return bad_request(format!("q is too long ({MAX_QUERY_CHARS} characters at most)"));
    }
    let limit = match per_group(q.limit.as_deref()) {
        Ok(n) => n,
        Err(e) => return bad_request(e),
    };
    let now = shared_domain::now_ist();
    Json(search_answer(&state.instruments, &state.quotes, text, limit, now).await).into_response()
}

// ---------------------------------------------------------------------------
// Quotes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct QuotesQuery {
    /// Comma-separated price-map keys (`nse_cm|2885`, `nse_fo|51386`,
    /// `idx|NIFTY 50`), at most [`MAX_QUOTE_KEYS`].
    keys: Option<String>,
}

/// `GET /api/v2/quotes`'s answer.
#[derive(Serialize)]
pub struct QuotesOut {
    /// Unix ms the answer was made.
    pub at: i64,
    pub market_open: bool,
    /// One per key with a quote, in the order asked.
    pub quotes: Vec<Quote>,
    /// Known keys no source has a quote for yet. They are polled from now,
    /// so the next call may have them.
    pub missing: Vec<String>,
    /// Keys the catalog has no row for. Never sent to Kotak.
    pub unknown: Vec<String>,
}

/// The keys of a `keys=` parameter: trimmed, blanks dropped, repeats kept
/// once, in order. At most [`MAX_QUOTE_KEYS`], at least one.
pub fn parse_keys(raw: &str) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let key = part.trim();
        if key.is_empty() {
            continue;
        }
        if key.chars().count() > MAX_KEY_CHARS {
            return Err(format!("a key is too long ({MAX_KEY_CHARS} characters at most)"));
        }
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    if keys.is_empty() {
        return Err("keys (comma-separated price-map keys, e.g. nse_cm|2885) is required".into());
    }
    if keys.len() > MAX_QUOTE_KEYS {
        return Err(format!("at most {MAX_QUOTE_KEYS} keys a call"));
    }
    Ok(keys)
}

/// The quotes for `keys` as of `now`: a key the catalog knows
/// is answered from the hub's best source and marked wanted; one it does
/// not know is listed as unknown and never asked for.
pub async fn quotes_answer(catalog: &Catalog, hub: &QuoteHub, keys: Vec<String>, now: DateTime<FixedOffset>) -> QuotesOut {
    let known = catalog.known_keys(&keys).await;
    let mut quotes = Vec::new();
    let mut missing = Vec::new();
    let mut unknown = Vec::new();
    let mut wanted = Vec::new();
    for key in keys {
        if !known.contains(&key) {
            unknown.push(key);
            continue;
        }
        match hub.get_at(&key, now) {
            Some(quote) => quotes.push(quote),
            None => missing.push(key.clone()),
        }
        wanted.push(key);
    }
    hub.want(wanted);
    QuotesOut { at: now.timestamp_millis(), market_open: eod::open_at(now), quotes, missing, unknown }
}

/// `GET /api/v2/quotes?keys=` — the quotes for up to 60 price-map keys:
/// the feed's tick first for an equity or index, Kotak's REST quotes (a
/// contract's only live source), Upstox while Kotak is down, the day's
/// close after hours. The keys are marked wanted, so the
/// hub polls them for the next 15 s.
pub async fn quotes_handler(State(state): State<AppState>, Query(q): Query<QuotesQuery>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let keys = match parse_keys(q.keys.as_deref().unwrap_or("")) {
        Ok(keys) => keys,
        Err(e) => return bad_request(e),
    };
    let now = shared_domain::now_ist();
    Json(quotes_answer(&state.instruments, &state.quotes, keys, now).await).into_response()
}

// ---------------------------------------------------------------------------
// One instrument
// ---------------------------------------------------------------------------

/// `GET /api/v2/instruments/:id`'s answer: the instrument page.
#[derive(Serialize)]
pub struct InstrumentOut {
    #[serde(flatten)]
    pub instrument: Instrument,
    /// LTP, change, the day's open, high and low, the previous close, the
    /// volume and the 52-week range, from the hub's best source; the close
    /// after hours. `None` until the poller has fetched it.
    pub quote: Option<Quote>,
    /// A derivative's underlying, priced the same way.
    pub spot: Option<Quote>,
    pub catalog_day: Option<String>,
    pub market_open: bool,
}

/// The instrument page for `id` as of `now`; `Ok(None)` for an id the
/// catalog has no row for, `Err` when the catalog could not be read. Its
/// key (and its underlying's) are marked wanted.
pub async fn instrument_answer(catalog: &Catalog, hub: &QuoteHub, id: &str, now: DateTime<FixedOffset>) -> Result<Option<InstrumentOut>, sqlx::Error> {
    let Some(instrument) = catalog.instrument(id, now.date_naive()).await? else { return Ok(None) };
    let quote = hub.get_at(&instrument.row.ws_key, now);
    let spot_key = instrument.underlying_key.as_deref().filter(|k| *k != instrument.row.ws_key);
    let spot = spot_key.and_then(|key| hub.get_at(key, now));
    hub.want(std::iter::once(instrument.row.ws_key.clone()).chain(spot_key.map(str::to_string)));
    Ok(Some(InstrumentOut { instrument, quote, spot, catalog_day: catalog.status().day, market_open: eod::open_at(now) }))
}

/// `GET /api/v2/instruments/:id` — one instrument (`<segment>:<token>`,
/// as search rows carry it) with its lot and tick size, ISIN, exchange,
/// nearest expiry for an F&O underlying, and its quote. 404 for an id the
/// catalog has no row for; 500 when the catalog could not be read.
pub async fn instrument_handler(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    let now = shared_domain::now_ist();
    match instrument_answer(&state.instruments, &state.quotes, id.trim(), now).await {
        Ok(Some(out)) => Json(out).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no instrument {id}") }))).into_response(),
        Err(e) => server_error("the instrument catalog", e),
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// `GET /api/v2/instruments/status`'s answer.
#[derive(Serialize)]
pub struct StatusOut {
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub now: String,
    pub market_open: bool,
    /// The catalog: its data day, rows per segment and per kind, the last
    /// error.
    pub catalog: CatalogStatus,
    /// The hub: what it polls, its request rate, calls, refusals, pauses,
    /// the sweep and the close on file.
    pub quotes: HubStatus,
    /// The day of the close snapshot being served after hours, when one is
    /// on file.
    pub snapshot_day: Option<String>,
}

pub fn status_answer(catalog: &Catalog, hub: &QuoteHub, now: DateTime<FixedOffset>) -> StatusOut {
    let quotes = hub.status();
    StatusOut {
        now: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        market_open: eod::open_at(now),
        catalog: catalog.status(),
        snapshot_day: quotes.close.as_ref().map(|c| c.day.clone()),
        quotes,
    }
}

/// `GET /api/v2/instruments/status` — the catalog's day and row counts and
/// the quote hub's rates, errors and sweep, for the admin's System screen
/// (`Access::Admin`: the error text names Kotak's answers and the
/// catalog's paths).
pub async fn instruments_status_handler(State(state): State<AppState>) -> Response {
    if let Some(off) = gate(&state).await {
        return off;
    }
    Json(status_answer(&state.instruments, &state.quotes, shared_domain::now_ist())).into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use chrono::NaiveDate;
    use dashmap::DashMap;
    use tower::ServiceExt;

    use super::*;
    use crate::instruments::load::Kind;
    use crate::instruments::tests::Scratch;
    use crate::quotes::eod::{Row, Snapshot};
    use crate::quotes::hub::Source;

    fn ms(t: DateTime<FixedOffset>) -> i64 {
        t.timestamp_millis()
    }

    fn ist(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .single()
            .unwrap()
    }

    /// A hub over `prices` whose every feed tick is at `tick`.
    fn hub(prices: Arc<DashMap<String, f64>>, tick: DateTime<FixedOffset>) -> QuoteHub {
        let at = tick.timestamp_millis() as f64 / 1000.0;
        QuoteHub::with_feed(prices, Box::new(|_: &str| None), Box::new(move |_: &str| Some(at)))
    }

    async fn catalog(name: &str) -> (Scratch, Arc<Catalog>) {
        let scratch = Scratch::new(name);
        scratch.write_all();
        let catalog = scratch.catalog().await;
        catalog.load_if_changed("test").await.unwrap_or_else(|e| panic!("{e}"));
        (scratch, catalog)
    }

    fn symbols(rows: &[PricedRow]) -> Vec<&str> {
        rows.iter().map(|r| r.row.trading_symbol.as_str()).collect()
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn search_rows_carry_their_quotes_and_the_spot_centres_the_options() {
        let (_scratch, catalog) = catalog("route-search").await;
        let noon = ist(2026, 11, 4, 12, 0);
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        prices.insert("nse_cm|2885".into(), 1_381.0);
        prices.insert("nse_fo|41226".into(), 12.5);
        let hub = hub(Arc::clone(&prices), noon);

        let out = search_answer(&catalog, &hub, "reliance", PER_GROUP, noon).await;
        assert_eq!((out.query.as_str(), out.parsed, out.today.as_str(), out.market_open), ("reliance", "plain", "2026-11-04", true));
        assert_eq!(out.catalog_day, catalog.status().day);
        assert!(out.indices.is_empty());
        // The stock's row, priced from the feed.
        let stock = &out.stocks[0];
        assert_eq!((stock.row.id.as_str(), stock.row.kind, stock.row.lot_size), ("nse_cm:2885", Kind::Eq, 1));
        let quote = stock.quote.as_ref().unwrap();
        assert_eq!((quote.ltp, quote.source, quote.key.as_str()), (1_381.0, Source::Feed, "nse_cm|2885"));
        // The options come from the strikes around that spot, not the chain's middle.
        assert_eq!(
            symbols(&out.options),
            vec!["RELIANCE26NOV1380CE", "RELIANCE26NOV1380PE", "RELIANCE26NOV1400CE", "RELIANCE26NOV1400PE", "RELIANCE26NOV1420CE", "RELIANCE26NOV1420PE"]
        );
        // A contract on the feed is priced like any other, from REST alone:
        // not before the poller has read it, and then at REST's price and
        // time, so a held contract never stands out.
        assert!(out.options[0].quote.is_none(), "the 1380 CE is on the feed, but not polled yet");
        assert!(out.options[1].quote.is_none(), "no source has the 1380 PE yet");
        hub.put_rest(Quote::bare("nse_fo|41226", 12.4, ms(noon) - 1_000, Source::Rest));
        let out = search_answer(&catalog, &hub, "reliance", PER_GROUP, noon).await;
        let ce = out.options[0].quote.as_ref().unwrap();
        assert_eq!((ce.ltp, ce.source, ce.at), (12.4, Source::Rest, ms(noon) - 1_000), "polled once, REST's read gives the price, never the feed's fresher tick");
        assert_eq!(serde_json::to_value(ce).unwrap()["source"], "live");
        assert_eq!(symbols(&out.futures), vec!["RELIANCE26NOVFUT", "RELIANCE26DECFUT"]);
        assert!(out.futures.iter().all(|r| r.quote.is_none()));
        // Every row's key is wanted now, so the poller fetches it.
        let watched: std::collections::HashSet<String> = hub.watched_keys().into_iter().collect();
        for key in ["nse_cm|2885", "nse_fo|41226", "nse_fo|41227", "nse_fo|43001", "nse_fo|43002"] {
            assert!(watched.contains(key), "{key} is not wanted");
        }
        assert_eq!(watched.len(), 1 + 6 + 2);

        // The limit cuts every group; a typed contract resolves as the catalog does.
        let out = search_answer(&catalog, &hub, "reliance", 1, noon).await;
        assert_eq!((out.stocks.len(), out.futures.len(), out.options.len()), (1, 1, 1));
        let out = search_answer(&catalog, &hub, "RELIANCE NOV 1400 PE", PER_GROUP, noon).await;
        assert_eq!((out.parsed, symbols(&out.options)), ("contract", vec!["RELIANCE26NOV1400PE"]));
        // The JSON shape: the row's fields beside its quote.
        let json = serde_json::to_value(&out.options[0]).unwrap();
        assert_eq!((json["id"].as_str(), json["kind"].as_str(), json["strike"].as_f64()), (Some("nse_fo:41223"), Some("PE"), Some(1_400.0)));
        assert_eq!((json["expiry"].as_str(), json["lot_size"].as_i64(), json["ws_key"].as_str()), (Some("2026-11-24"), Some(500), Some("nse_fo|41223")));
        assert!(json["quote"].is_null());
        // Nothing matches nothing.
        let out = search_answer(&catalog, &hub, "zzz", PER_GROUP, noon).await;
        assert!(out.indices.is_empty() && out.stocks.is_empty() && out.futures.is_empty() && out.options.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_hours_the_close_prices_the_rows() {
        let (_scratch, catalog) = catalog("route-close").await;
        let evening = ist(2026, 11, 4, 18, 0);
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        let hub = hub(Arc::clone(&prices), evening);
        let mut rows = HashMap::new();
        rows.insert(
            "nse_cm|11536".to_string(),
            Row { key: "nse_cm|11536".into(), ltp: 3_010.0, open: Some(3_000.0), high: Some(3_020.0), low: Some(2_990.0), close: Some(2_995.0), volume: Some(1e6) },
        );
        hub.set_snapshot(Snapshot { day: NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(), taken_at: ist(2026, 11, 4, 15, 35), rows, asked: 1 });

        let out = search_answer(&catalog, &hub, "tcs", PER_GROUP, evening).await;
        assert!(!out.market_open);
        let quote = out.stocks[0].quote.as_ref().unwrap();
        assert_eq!((quote.ltp, quote.source, quote.as_of.as_deref()), (3_010.0, Source::Close, Some("2026-11-04 15:30")));
        assert_eq!((quote.close, quote.change), (Some(2_995.0), Some(15.0)));

        let out = quotes_answer(&catalog, &hub, vec!["nse_cm|11536".into(), "nse_cm|2885".into()], evening).await;
        assert_eq!((out.quotes.len(), out.quotes[0].source, out.market_open), (1, Source::Close, false));
        assert_eq!((out.missing, out.unknown), (vec!["nse_cm|2885".to_string()], Vec::<String>::new()));
        let page = instrument_answer(&catalog, &hub, "nse_cm:11536", evening).await.unwrap().unwrap();
        assert_eq!(page.quote.map(|q| q.source), Some(Source::Close));
        assert_eq!(status_answer(&catalog, &hub, evening).snapshot_day.as_deref(), Some("2026-11-04"));
    }

    #[test]
    fn quote_keys_are_trimmed_deduped_and_capped() {
        assert_eq!(parse_keys("nse_cm|2885, nse_fo|51386 ,,nse_cm|2885,").unwrap(), vec!["nse_cm|2885", "nse_fo|51386"]);
        assert_eq!(parse_keys("idx|NIFTY 50").unwrap(), vec!["idx|NIFTY 50"]);
        assert!(parse_keys("").is_err());
        assert!(parse_keys(" , ,").is_err());
        let sixty: Vec<String> = (1..=60).map(|i| format!("nse_fo|{i}")).collect();
        assert_eq!(parse_keys(&sixty.join(",")).unwrap().len(), 60);
        let too_many = (1..=61).map(|i| format!("nse_fo|{i}")).collect::<Vec<_>>().join(",");
        assert_eq!(parse_keys(&too_many).unwrap_err(), "at most 60 keys a call");
        // Repeats do not count toward the cap.
        let repeated = (1..=60).map(|i| format!("nse_fo|{}", i % 10)).collect::<Vec<_>>().join(",");
        assert_eq!(parse_keys(&repeated).unwrap().len(), 10);
        assert!(parse_keys(&"x".repeat(65)).is_err());

        assert_eq!(per_group(None), Ok(PER_GROUP));
        assert_eq!(per_group(Some("")), Ok(PER_GROUP));
        assert_eq!(per_group(Some("3")), Ok(3));
        assert_eq!(per_group(Some("0")), Ok(1));
        assert_eq!(per_group(Some("99")), Ok(PER_GROUP));
        assert!(per_group(Some("many")).is_err());
        assert!(per_group(Some("-1")).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quotes_answer_known_keys_and_want_only_those() {
        let (_scratch, catalog) = catalog("route-quotes").await;
        let noon = ist(2026, 11, 4, 12, 0);
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        prices.insert("nse_cm|2885".into(), 1_400.0);
        prices.insert("idx|NIFTY 50".into(), 25_000.0);
        prices.insert("idx_close|NIFTY 50".into(), 24_900.0);
        let hub = hub(Arc::clone(&prices), noon);

        let keys: Vec<String> = ["nse_cm|2885", "idx|NIFTY 50", "nse_fo|51386", "nse_fo|999999", "junk"].iter().map(|k| k.to_string()).collect();
        let out = quotes_answer(&catalog, &hub, keys, noon).await;
        assert_eq!((out.at, out.market_open), (noon.timestamp_millis(), true));
        assert_eq!(out.quotes.iter().map(|q| (q.key.as_str(), q.ltp)).collect::<Vec<_>>(), vec![("nse_cm|2885", 1_400.0), ("idx|NIFTY 50", 25_000.0)]);
        assert_eq!((out.quotes[1].close, out.quotes[1].change), (Some(24_900.0), Some(100.0)));
        assert_eq!(out.missing, vec!["nse_fo|51386"]);
        assert_eq!(out.unknown, vec!["nse_fo|999999", "junk"]);
        let mut watched = hub.watched_keys();
        watched.sort();
        assert_eq!(watched, vec!["idx|NIFTY 50", "nse_cm|2885", "nse_fo|51386"], "unknown keys are never polled");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_instrument_page_has_its_quote_and_its_spot() {
        let (_scratch, catalog) = catalog("route-instrument").await;
        let noon = ist(2026, 11, 4, 12, 0);
        let prices: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        prices.insert("idx|NIFTY 50".into(), 25_000.0);
        prices.insert("nse_fo|51386".into(), 150.0);
        let hub = hub(Arc::clone(&prices), noon);

        let page = instrument_answer(&catalog, &hub, "nse_fo:51386", noon).await.unwrap().unwrap();
        assert_eq!((page.instrument.row.kind, page.instrument.row.strike, page.instrument.row.lot_size), (Kind::Ce, Some(23_000.0), 65));
        assert!(page.quote.is_none(), "on the feed, not yet polled: unpriced like any contract");
        assert_eq!(page.spot.as_ref().map(|q| (q.key.as_str(), q.ltp)), Some(("idx|NIFTY 50", 25_000.0)));
        assert!(page.market_open);
        let mut watched = hub.watched_keys();
        watched.sort();
        assert_eq!(watched, vec!["idx|NIFTY 50", "nse_fo|51386"]);
        hub.put_rest(Quote::bare("nse_fo|51386", 149.0, ms(noon) - 1_000, Source::Rest));
        let page = instrument_answer(&catalog, &hub, "nse_fo:51386", noon).await.unwrap().unwrap();
        assert_eq!(page.quote.as_ref().map(|q| q.ltp), Some(149.0), "REST's read, not the feed's 150");
        // A cash row is its own spot: no second quote, one key wanted.
        let page = instrument_answer(&catalog, &hub, "nse_cm:2885", noon).await.unwrap().unwrap();
        assert!(page.spot.is_none() && page.quote.is_none());
        assert_eq!(page.instrument.nearest_expiry.as_deref(), Some("2026-11-24"));
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!((json["id"].as_str(), json["symbol"].as_str(), json["isin"].as_str()), (Some("nse_cm:2885"), Some("RELIANCE"), Some("INE002A01018")));
        assert_eq!((json["tick_size"].as_f64(), json["nearest_expiry"].as_str(), json["underlying_key"].as_str()), (Some(0.05), Some("2026-11-24"), Some("nse_cm|2885")));
        assert!(json["quote"].is_null() && json["spot"].is_null());
        assert!(instrument_answer(&catalog, &hub, "nse_cm:0", noon).await.unwrap().is_none());
        assert!(instrument_answer(&catalog, &hub, "status", noon).await.unwrap().is_none());

        let status = status_answer(&catalog, &hub, noon);
        assert_eq!((status.now.as_str(), status.market_open, status.snapshot_day), ("2026-11-04 12:00:00", true, None));
        assert_eq!(status.catalog.kinds.get("EQ"), Some(&18));
        assert_eq!(status.quotes.wanted, 3);
    }

    #[tokio::test]
    async fn the_search_switch_off_is_a_404_with_a_reason() {
        let response = off_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = body_json(response).await;
        assert!(json["error"].as_str().unwrap().contains("search"), "{json}");
        let response = bad_request("q is required");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"], "q is required");
        // A catalog failure is a 500 in general terms, never the cause.
        let response = server_error("the instrument catalog", "sqlite: disk I/O error at /srv/x.db");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_json(response).await["error"], "could not read the instrument catalog");
    }

    /// `/api/v2/instruments/status` is registered beside
    /// `/api/v2/instruments/:id`; the static path must win, and an id with a
    /// space (`nse_cm:Nifty 50`) must arrive decoded.
    #[tokio::test]
    async fn the_status_path_is_not_an_instrument_id() {
        let app = Router::new()
            .route("/api/v2/instruments/status", get(|| async { "status" }))
            .route("/api/v2/instruments/:id", get(|Path(id): Path<String>| async move { format!("id={id}") }));
        for (uri, want) in [
            ("/api/v2/instruments/status", "status"),
            ("/api/v2/instruments/nse_cm:2885", "id=nse_cm:2885"),
            ("/api/v2/instruments/nse_cm:Nifty%2050", "id=nse_cm:Nifty 50"),
            ("/api/v2/instruments/statusx", "id=statusx"),
        ] {
            let response = app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(String::from_utf8(bytes.to_vec()).unwrap(), want, "{uri}");
        }
    }
}
