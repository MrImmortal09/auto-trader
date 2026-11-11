//! The instrument catalog: every NSE equity and ETF, every NSE F&O contract,
//! the SENSEX and BANKEX options bse_fo lists, and the six indices the app
//! trades — searchable by symbol or name, each with its exchange, ISIN, lot
//! size and feed key. It is built from the scrip-master CSVs `scrip_files`
//! keeps on disk (`load`) into its own SQLite file, `data/instruments.db`:
//! apart from trades.db because a rebuild writes 150-200k rows in one
//! transaction and SQLite serialises writers, so in trades.db it would hold
//! the engine's `db_writer` behind the busy timeout. Nothing here is read by
//! the engine, which keeps `ScripStore` as its one source of contracts, and
//! nothing here writes the price map.
//!
//! Search (`search`) ranks a small in-memory list of underlyings (equities,
//! the indices, F&O underlyings), rebuilt at each load, and expands
//! derivatives for the top matches by indexed lookups: nearest expiries
//! first, strikes nearest the spot when the caller knows it. The contract
//! rows are never scanned per query. Name words go through FTS5 when the
//! SQLite build has it (the bundled one does), else the same tier is
//! computed in memory with a warning logged once.

pub mod load;
pub mod search;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;
use shared_domain::FeatureFlags;
use sqlx::SqlitePool;

use crate::scrip_files::{ScripFiles, SEGMENTS};
use load::{Kind, LoadReport, Source};
use search::{parse_query, Group, Parsed, SearchIndex, Side, Tier, Underlying, UnderlyingKind};

pub use search::MonthHint;

/// Rows per group in a search answer.
pub const PER_GROUP: usize = 8;

/// F&O underlyings whose derivatives a plain search expands, from the top.
const EXPAND_UNDERLYINGS: usize = 3;

/// Strikes below and above the centre a plain search lists options at.
const STRIKES_EACH_SIDE: usize = 2;

/// What the catalog holds, for the System screen and the log.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CatalogStatus {
    /// IST date of the CSVs the catalog was built from.
    pub day: Option<String>,
    /// IST `YYYY-MM-DD HH:MM:SS` of the last successful build.
    pub loaded_at: Option<String>,
    /// Rows per segment (`nse_cm`, `nse_fo`, `bse_fo`) and the six `index` rows.
    pub rows: BTreeMap<String, i64>,
    /// Rows read per kind (`INDEX`, `EQ`, `FUT`, `CE`, `PE`), counted as the
    /// files were parsed — never by counting the table. A token repeated in
    /// a file counts at each row here and once in `rows`.
    pub kinds: BTreeMap<String, i64>,
    /// Symbols in the search list.
    pub underlyings: usize,
    /// Name words are searched with FTS5; else in memory.
    pub fts: bool,
    pub loading: bool,
    pub last_error: Option<String>,
}

impl CatalogStatus {
    pub fn total_rows(&self) -> i64 {
        self.rows.values().sum()
    }
}

/// Why a build did not happen.
#[derive(Debug)]
pub enum LoadError {
    /// The catalog database could not be opened at startup.
    Unavailable(String),
    /// No scrip-master CSV is on disk.
    NoFiles,
    Io { segment: &'static str, error: std::io::Error },
    Db(sqlx::Error),
    /// The build thread did not finish.
    Join(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Unavailable(why) => write!(f, "catalog database unavailable: {why}"),
            LoadError::NoFiles => write!(f, "no scrip-master CSV on disk"),
            LoadError::Io { segment, error } => write!(f, "{segment}.csv: {error}"),
            LoadError::Db(e) => write!(f, "catalog database: {e}"),
            LoadError::Join(e) => write!(f, "catalog build thread: {e}"),
        }
    }
}

impl From<sqlx::Error> for LoadError {
    fn from(e: sqlx::Error) -> Self {
        LoadError::Db(e)
    }
}

/// What a build attempt (`load_if_changed_at`) did.
pub enum LoadOutcome {
    /// The CSVs on disk are the ones the catalog was last built from.
    Unchanged,
    /// The market is open and the CSVs on disk are newer files of the same
    /// day, and the same segments, the catalog was last built from (a
    /// relogin's download): rebuilding would change nothing and take both
    /// of the VM's cores for seconds while the engine ticks, so it is left
    /// for a trigger outside market hours (`load_if_changed_at`); the next
    /// day's first download builds that day's files anyway.
    SameDay,
    Loaded(LoadReport),
}

/// One row of a search answer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchRow {
    /// `<segment>:<token>`.
    pub id: String,
    pub kind: Kind,
    pub symbol: String,
    pub name: String,
    /// `NSE` or `BSE`.
    pub exchange: String,
    pub trading_symbol: String,
    pub underlying: String,
    /// `YYYY-MM-DD`, derivatives only.
    pub expiry: Option<String>,
    pub strike: Option<f64>,
    pub lot_size: i64,
    pub tick_size: f64,
    /// The price-map key.
    pub ws_key: String,
    /// The underlying has F&O listed.
    pub has_fo: bool,
    /// How the query matched, for a row from the search list; `None` for a
    /// derivative listed under a match.
    pub tier: Option<Tier>,
}

/// A search's results, grouped.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SearchAnswer {
    pub query: String,
    /// `plain`, `future` or `contract`: how the query was read.
    pub parsed: &'static str,
    pub indices: Vec<SearchRow>,
    pub stocks: Vec<SearchRow>,
    pub futures: Vec<SearchRow>,
    pub options: Vec<SearchRow>,
}

/// One instrument as its page shows it (`GET /api/v2/instruments/:id`): its
/// row, and what the page needs beside it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Instrument {
    #[serde(flatten)]
    pub row: SearchRow,
    pub segment: String,
    /// `pSymbol`: the token the feed and the quotes API take.
    pub token: String,
    pub isin: Option<String>,
    /// The price-map key of the underlying's cash or index row: a
    /// derivative's spot, a cash row's own key. `None` for an underlying
    /// with no cash or index row (`NIFTYNXT50`).
    pub underlying_key: Option<String>,
    /// The nearest expiry listed under the underlying from today, for an
    /// F&O underlying's cash or index row; `None` on a contract or a stock
    /// with no F&O.
    pub nearest_expiry: Option<String>,
}

/// The catalog handle in `AppState`. Cheap to share; one build at a time.
pub struct Catalog {
    /// `None` when the database could not be opened: every lookup answers
    /// empty and every build fails, and the status says why.
    pool: Option<SqlitePool>,
    files: ScripFiles,
    fts: bool,
    /// The v2 switches as loaded: `search` gates the background rebuilds,
    /// read at the moment of each one so flipping it needs no restart.
    flags: Arc<tokio::sync::RwLock<FeatureFlags>>,
    index: RwLock<Arc<SearchIndex>>,
    status: Mutex<CatalogStatus>,
    loading: tokio::sync::Mutex<()>,
}

impl Catalog {
    /// Opens (or creates) the catalog database at `db_path` and restores the
    /// search list from the last build, so search works right after a
    /// restart. Never fails: a database that cannot be opened leaves an
    /// empty catalog whose status carries the error. `flags` is the loaded
    /// `FeatureFlags`: a background rebuild runs only while `search` is on.
    pub async fn open(files: ScripFiles, db_path: &Path, flags: Arc<tokio::sync::RwLock<FeatureFlags>>) -> Arc<Catalog> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (pool, fts, error) = match crate::db::init_catalog_db(db_path).await {
            Ok((pool, fts)) => (Some(pool), fts, None),
            Err(e) => {
                tracing::error!(path = %db_path.display(), error = %e, "Instrument catalog database cannot be opened — search stays empty");
                (None, false, Some(format!("cannot open {}: {e}", db_path.display())))
            }
        };
        if pool.is_some() && !fts {
            tracing::warn!("FTS5 is not in this SQLite build — name search falls back to in-memory LIKE-style matching");
        }
        let catalog = Catalog {
            pool,
            files,
            fts,
            flags,
            index: RwLock::new(Arc::new(SearchIndex::empty())),
            status: Mutex::new(CatalogStatus { fts, last_error: error, ..Default::default() }),
            loading: tokio::sync::Mutex::new(()),
        };
        if let Some(pool) = catalog.pool.clone() {
            catalog.restore(&pool).await;
        }
        Arc::new(catalog)
    }

    pub fn status(&self) -> CatalogStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The catalog database's pool, which the quote hub's close snapshot
    /// (`quote_eod`) shares; `None` when the database could not be opened.
    pub fn pool(&self) -> Option<SqlitePool> {
        self.pool.clone()
    }

    fn index(&self) -> Arc<SearchIndex> {
        Arc::clone(&self.index.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// The price-map key of every equity and index in the catalog: what
    /// the quote hub's close sweep asks Kotak for. From the search list in
    /// memory; the contract rows are not touched.
    pub fn cash_keys(&self) -> Vec<String> {
        self.index()
            .entries()
            .iter()
            .filter(|u| matches!(u.kind, UnderlyingKind::Index | UnderlyingKind::Equity))
            .filter_map(|u| u.ws_key.clone())
            .collect()
    }

    /// Upstox's instrument key for a cash or index row's price-map key: an
    /// equity by its ISIN (`NSE_EQ|INE002A01018`), one of the six indices by
    /// the name Upstox gives it. `None` for a contract: whether Kotak's and
    /// Upstox's F&O tokens are the same number is not verified, and a wrong
    /// price is worse than none.
    pub fn upstox_key(&self, ws_key: &str) -> Option<String> {
        let index = self.index();
        let u = index.by_ws_key(ws_key)?;
        match u.kind {
            UnderlyingKind::Index => crate::candles::symbols::upstox_index_key(&u.symbol).map(str::to_string),
            UnderlyingKind::Equity => {
                let isin = u.isin.as_deref().map(str::trim).filter(|i| !i.is_empty())?;
                Some(format!("{}_EQ|{isin}", u.exchange.as_deref().unwrap_or("NSE")))
            }
            UnderlyingKind::FoOnly => None,
        }
    }

    fn set_index(&self, index: Arc<SearchIndex>) {
        *self.index.write().unwrap_or_else(|e| e.into_inner()) = index;
    }

    fn update_status(&self, f: impl FnOnce(&mut CatalogStatus)) {
        f(&mut self.status.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// The segment files on disk, in [`SEGMENTS`] order, as sources.
    fn sources(&self) -> Vec<Source> {
        SEGMENTS
            .into_iter()
            .filter_map(|segment| {
                let path = self.files.path(segment);
                let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
                let mtime = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
                Some(Source { segment, path, mtime: i64::try_from(mtime).unwrap_or(i64::MAX) })
            })
            .collect()
    }

    /// The last build's `instrument_load` rows, one per segment, with the
    /// day and time of that build.
    async fn last_load(pool: &SqlitePool) -> Result<Option<(String, String, Vec<LoadRow>)>, sqlx::Error> {
        let rows = sqlx::query_as::<_, (String, String, String, i64, Option<i64>, Option<String>)>(
            "SELECT day, loaded_at, segment, rows, source_mtime, kinds FROM instrument_load
             WHERE loaded_at = (SELECT MAX(loaded_at) FROM instrument_load)",
        )
        .fetch_all(pool)
        .await?;
        let Some((day, loaded_at, ..)) = rows.first().cloned() else { return Ok(None) };
        let segments = rows
            .into_iter()
            .map(|(_, _, segment, rows, mtime, kinds)| LoadRow {
                segment,
                rows,
                mtime,
                kinds: kinds.and_then(|k| serde_json::from_str(&k).ok()).unwrap_or_default(),
            })
            .collect();
        Ok(Some((day, loaded_at, segments)))
    }

    /// Puts the search list and the status back from the database.
    async fn restore(&self, pool: &SqlitePool) {
        match Self::last_load(pool).await {
            Ok(Some((day, loaded_at, segments))) => {
                let mut rows: BTreeMap<String, i64> = BTreeMap::new();
                let mut kinds: BTreeMap<String, i64> = BTreeMap::new();
                for s in segments {
                    rows.insert(s.segment, s.rows);
                    for (kind, n) in s.kinds {
                        *kinds.entry(kind).or_insert(0) += n;
                    }
                }
                rows.insert("index".into(), shared_domain::INDEX_NAMES.len() as i64);
                kinds.insert("INDEX".into(), shared_domain::INDEX_NAMES.len() as i64);
                self.update_status(|s| {
                    s.day = Some(day);
                    s.loaded_at = Some(loaded_at);
                    s.rows = rows;
                    s.kinds = kinds;
                });
                let status = self.status();
                tracing::info!(
                    day = status.day.as_deref().unwrap_or("?"), loaded_at = status.loaded_at.as_deref().unwrap_or("?"),
                    rows = status.total_rows(), "Instrument catalog restored from the last build"
                );
            }
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "Instrument catalog: cannot read the last load"),
        }
        match Self::load_underlyings(pool).await {
            Ok(entries) => {
                let index = SearchIndex::new(entries);
                self.update_status(|s| s.underlyings = index.len());
                self.set_index(Arc::new(index));
            }
            Err(e) => tracing::error!(error = %e, "Instrument catalog: cannot read the underlyings"),
        }
    }

    async fn load_underlyings(pool: &SqlitePool) -> Result<Vec<Underlying>, sqlx::Error> {
        let rows = sqlx::query_as::<_, UnderlyingRow>(
            "SELECT symbol, name, kind, id, has_fo, fo_index, fo_segment, isin, exchange, ws_key, trading_symbol, lot_size, tick_size
             FROM instrument_underlyings",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().filter_map(UnderlyingRow::into_underlying).collect())
    }

    /// Rebuilds the catalog from the CSVs on disk unless they are the files
    /// the last build read (same segments, same modification times),
    /// whatever the time: the tests' build. The server's goes through
    /// [`Catalog::load_if_changed_at`].
    #[cfg(test)]
    pub async fn load_if_changed(&self, reason: &str) -> Result<LoadOutcome, LoadError> {
        self.load(reason, None).await
    }

    /// Rebuilds the catalog from the CSVs on disk as of `now`, unless they
    /// are the files the last build read (same segments, same modification
    /// times) — or, while the market is open, newer files of the same day
    /// and the same segments as the last build ([`LoadOutcome::SameDay`]):
    /// the session watchdog's relogin downloads the day's scrip master
    /// again, and a rebuild of 150-200k rows during hours would only
    /// compete with the engine's tick for the VM's two cores. The first
    /// download of a new day, a segment the last build lacked, and any
    /// build outside market hours go ahead. One build at a time; a second
    /// caller waits and then finds nothing to do.
    pub async fn load_if_changed_at(&self, reason: &str, now: DateTime<FixedOffset>) -> Result<LoadOutcome, LoadError> {
        self.load(reason, Some(now)).await
    }

    async fn load(&self, reason: &str, now: Option<DateTime<FixedOffset>>) -> Result<LoadOutcome, LoadError> {
        let _one_at_a_time = self.loading.lock().await;
        let pool = match &self.pool {
            Some(pool) => pool.clone(),
            None => {
                let why = self.status().last_error.unwrap_or_else(|| "not opened".into());
                return Err(LoadError::Unavailable(why));
            }
        };
        let sources = self.sources();
        if sources.is_empty() {
            return Err(LoadError::NoFiles);
        }
        if let Ok(Some((built_day, _, last))) = Self::last_load(&pool).await {
            let same_segments = last.len() == sources.len() && sources.iter().all(|s| last.iter().any(|l| l.segment == s.segment));
            let same = same_segments && sources.iter().all(|s| last.iter().any(|l| l.segment == s.segment && l.mtime == Some(s.mtime)));
            if same {
                return Ok(LoadOutcome::Unchanged);
            }
            let open = now.is_some_and(crate::quotes::eod::open_at);
            let files_day = load::files_day(&sources).map(|d| d.format("%Y-%m-%d").to_string());
            if open && same_segments && files_day.as_deref() == Some(built_day.as_str()) {
                return Ok(LoadOutcome::SameDay);
            }
        }

        self.update_status(|s| s.loading = true);
        let segments: Vec<&str> = sources.iter().map(|s| s.segment).collect();
        tracing::info!(reason, ?segments, "Instrument catalog build starting");
        let result = load::rebuild(&pool, sources, self.fts, shared_domain::now_ist()).await;
        match result {
            Ok(report) => {
                self.set_index(Arc::clone(&report.index));
                let mut rows: BTreeMap<String, i64> = report.segments.iter().map(|r| (r.segment.to_string(), r.stored() as i64)).collect();
                rows.insert("index".into(), report.index_rows as i64);
                let mut kinds: BTreeMap<String, i64> = BTreeMap::new();
                for r in &report.segments {
                    for (kind, n) in &r.kinds {
                        *kinds.entry(kind.to_string()).or_insert(0) += *n as i64;
                    }
                }
                kinds.insert("INDEX".into(), report.index_rows as i64);
                self.update_status(|s| {
                    s.day = Some(report.day.clone());
                    s.loaded_at = Some(report.loaded_at.clone());
                    s.rows = rows.clone();
                    s.kinds = kinds;
                    s.underlyings = report.underlyings;
                    s.loading = false;
                    s.last_error = None;
                });
                for r in &report.segments {
                    tracing::info!(
                        segment = r.segment, kept = r.kept, skipped = r.skipped, unresolved_strikes = r.unresolved_strikes,
                        duplicates = r.duplicates, skip_reasons = ?r.skip_reasons, first_skip = r.first_skip.as_deref().unwrap_or(""),
                        "Instrument catalog segment loaded"
                    );
                }
                tracing::info!(
                    reason, day = %report.day, ?rows, underlyings = report.underlyings,
                    secs = format!("{:.1}", report.secs), "Instrument catalog rebuilt"
                );
                Ok(LoadOutcome::Loaded(report))
            }
            Err(e) => {
                self.update_status(|s| {
                    s.loading = false;
                    s.last_error = Some(e.to_string());
                });
                tracing::error!(reason, error = %e, "Instrument catalog build failed — the previous catalog stays");
                Err(e)
            }
        }
    }

    /// [`Catalog::load_if_changed_at`] now, on its own task, its outcome logged — while
    /// the `search` switch is on. Off, nothing is built: the CSVs on disk
    /// are still written by every download, and the build they are owed
    /// runs when the switch is turned on (`PUT /api/v2/flags`) or at the
    /// first download after that. During market hours a download of the
    /// same day's files the catalog already holds is not built again.
    pub fn rebuild_in_background(self: &Arc<Self>, reason: &'static str) {
        let catalog = Arc::clone(self);
        tokio::spawn(async move {
            if !catalog.flags.read().await.search {
                tracing::info!(reason, "Instrument catalog rebuild skipped — the `search` switch is off");
                return;
            }
            match catalog.load_if_changed_at(reason, shared_domain::now_ist()).await {
                Ok(LoadOutcome::Unchanged) => tracing::info!(reason, "Instrument catalog is already built from the CSVs on disk"),
                Ok(LoadOutcome::SameDay) => tracing::info!(
                    reason,
                    "Instrument catalog rebuild skipped during market hours — it is already built from the same day's scrip master"
                ),
                Ok(LoadOutcome::Loaded(report)) => tracing::debug!(reason, day = %report.day, "Instrument catalog build finished"),
                Err(LoadError::NoFiles) => tracing::info!(reason, "No scrip-master CSV on disk yet — the instrument catalog waits for a download"),
                // Logged where it failed.
                Err(_) => {}
            }
        });
    }

    // -----------------------------------------------------------------------
    // Lookups for the quote and instrument routes
    // -----------------------------------------------------------------------

    /// The price-map key an underlying's spot ticks under: its cash or
    /// index row's. From the search list in memory.
    pub fn spot_key(&self, symbol: &str) -> Option<String> {
        self.index().get(symbol).and_then(|u| u.ws_key.clone())
    }

    /// Those of `keys` that are a catalog row's price-map key: equities and
    /// indices from the search list in memory, contracts by one primary-key
    /// lookup of their `<segment>:<token>` ids, each accepted only when the
    /// row's `ws_key` is the key asked for (an index's REST spelling,
    /// `nse_cm|Nifty 50`, names its virtual row's id but is not its key).
    /// What `GET /api/v2/quotes` accepts, so nothing a caller types goes to
    /// Kotak's quotes API unchecked.
    pub async fn known_keys(&self, keys: &[String]) -> HashSet<String> {
        let index = self.index();
        let mut known = HashSet::new();
        let mut ids: Vec<String> = Vec::new();
        for key in keys {
            if index.by_ws_key(key).is_some() {
                known.insert(key.clone());
            } else if let Some((segment, token)) = key.split_once('|') {
                if !segment.is_empty() && !token.is_empty() {
                    ids.push(format!("{segment}:{token}"));
                }
            }
        }
        let Some(pool) = &self.pool else { return known };
        if ids.is_empty() {
            return known;
        }
        let sql = format!("SELECT ws_key FROM instruments WHERE id IN ({})", vec!["?"; ids.len()].join(","));
        let mut query = sqlx::query_scalar::<_, String>(&sql);
        for id in &ids {
            query = query.bind(id);
        }
        match query.fetch_all(pool).await {
            Ok(found) => {
                for key in keys {
                    if found.contains(key) {
                        known.insert(key.clone());
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "Instrument catalog: key lookup failed"),
        }
        known
    }

    /// The catalog's row for each of `keys` that names one, by the key as
    /// given: a price-map key (`nse_cm|2885`, `idx|NIFTY 50`) or a row's id
    /// (`nse_cm:2885`, `nse_cm:Nifty 50`). Equities and indices come from
    /// the search list in memory, contracts by one primary-key lookup of
    /// their ids; a price-map key is accepted only when it is the row's own
    /// (`nse_cm|Nifty 50` names an index's virtual row but is not its key,
    /// as in `known_keys`). A key with no row is simply absent from the
    /// answer. What the watchlists check keys against and label rows with;
    /// the row's `ws_key` is what they keep. `Err` when the contracts'
    /// lookup itself failed, so a broken catalog database is never taken
    /// for keys it has no row for (an unopened catalog has none, `Ok`).
    pub async fn rows_for_keys(&self, keys: &[String]) -> Result<HashMap<String, SearchRow>, sqlx::Error> {
        let index = self.index();
        let mut found: HashMap<String, SearchRow> = HashMap::new();
        let mut ids: Vec<String> = Vec::new();
        for key in keys {
            if found.contains_key(key) {
                continue;
            }
            if let Some(row) = index.by_ws_key(key).and_then(|u| cash_row(u, Tier::Exact)) {
                found.insert(key.clone(), SearchRow { tier: None, ..row });
            } else if let Some((segment, token)) = key.split_once('|') {
                if !segment.is_empty() && !token.is_empty() {
                    ids.push(format!("{segment}:{token}"));
                }
            } else if key.split_once(':').is_some_and(|(segment, token)| !segment.is_empty() && !token.is_empty()) {
                ids.push(key.clone());
            }
        }
        let Some(pool) = &self.pool else { return Ok(found) };
        if ids.is_empty() {
            return Ok(found);
        }
        ids.sort();
        ids.dedup();
        let sql = format!("{SELECT_ROW} WHERE id IN ({})", vec!["?"; ids.len()].join(","));
        let mut query = sqlx::query_as::<_, DbRow>(&sql);
        for id in &ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(pool).await.map_err(|e| {
            tracing::error!(error = %e, "Instrument catalog: rows lookup failed");
            e
        })?;
        let rows: Vec<SearchRow> = rows.into_iter().map(|r| r.into_row(&index)).collect();
        for key in keys {
            if found.contains_key(key) {
                continue;
            }
            if let Some(row) = rows.iter().find(|r| r.ws_key == *key || r.id == *key) {
                found.insert(key.clone(), row.clone());
            }
        }
        Ok(found)
    }

    /// Closes the catalog database's pool, so every lookup that reaches it
    /// fails from then on: the tests' broken catalog.
    #[cfg(test)]
    pub(crate) async fn close_db_for_tests(&self) {
        if let Some(pool) = &self.pool {
            pool.close().await;
        }
    }

    /// The instrument with `id` (`<segment>:<token>`), with its underlying's
    /// key and, for an F&O underlying's own row, the nearest expiry listed
    /// from `today`. One primary-key lookup, and seeks for the expiry.
    /// `Ok(None)` for an id the catalog has no row for (an unopened catalog
    /// has none); `Err` when the lookup itself failed, which the route
    /// reports as a server error rather than "no instrument".
    pub async fn instrument(&self, id: &str, today: NaiveDate) -> Result<Option<Instrument>, sqlx::Error> {
        let Some(pool) = self.pool.as_ref() else { return Ok(None) };
        let row = sqlx::query_as::<_, DetailRow>(SELECT_DETAIL).bind(id).fetch_optional(pool).await.map_err(|e| {
            tracing::error!(error = %e, id, "Instrument catalog: instrument lookup failed");
            e
        })?;
        let Some(row) = row else { return Ok(None) };
        let index = self.index();
        let underlying = index.get(&row.row.underlying);
        let underlying_key = underlying.and_then(|u| u.ws_key.clone());
        let row_kind = Kind::parse(&row.row.kind).unwrap_or(Kind::Eq);
        let nearest_expiry = match underlying {
            Some(u) if u.has_fo && matches!(row_kind, Kind::Eq | Kind::Index) => self.nearest_expiry(&u.symbol, today).await,
            _ => None,
        };
        Ok(Some(Instrument {
            row: row.row.into_row(&index),
            segment: row.segment,
            token: row.token,
            isin: row.isin,
            underlying_key,
            nearest_expiry: nearest_expiry.map(day_text),
        }))
    }

    /// The nearest expiry any contract under `underlying` is listed for
    /// from `today`: one `MIN(expiry)` seek per kind on the chain index.
    async fn nearest_expiry(&self, underlying: &str, today: NaiveDate) -> Option<NaiveDate> {
        let pool = self.pool.as_ref()?;
        sqlx::query_scalar::<_, Option<String>>(NEAREST_EXPIRY)
            .bind(underlying)
            .bind(day_text(today))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Instrument catalog: nearest expiry lookup failed");
                None
            })
            .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Everything `raw` matches, grouped and ranked, with derivatives listed
    /// from `today`. `spot` gives an underlying's current price when known,
    /// so the options listed are the strikes around it; without one, the
    /// middle of the chain. Behind `GET /api/v2/search`.
    pub async fn search(&self, raw: &str, today: NaiveDate, spot: &(dyn Fn(&str) -> Option<f64> + Sync)) -> SearchAnswer {
        let parsed = parse_query(raw);
        let mut answer = SearchAnswer { query: raw.trim().to_string(), parsed: parsed.label(), ..Default::default() };
        let text = parsed.text();
        if text.is_empty() {
            return answer;
        }
        let index = self.index();
        let word_hits = if self.fts { self.fts_symbols(text).await } else { None };
        let hits = index.rank(text, word_hits.as_ref());

        for hit in &hits {
            let rows = match hit.entry.group() {
                Group::Indices => &mut answer.indices,
                Group::Stocks => &mut answer.stocks,
            };
            if rows.len() < PER_GROUP {
                if let Some(row) = cash_row(hit.entry, hit.tier) {
                    rows.push(row);
                }
            }
        }

        let fo: Vec<&Underlying> = hits.iter().map(|h| h.entry).filter(|e| e.has_fo).take(EXPAND_UNDERLYINGS).collect();
        match &parsed {
            Parsed::Plain(_) => {
                for u in &fo {
                    let room = PER_GROUP - answer.futures.len();
                    if room == 0 {
                        break;
                    }
                    answer.futures.extend(self.futures_of(&u.symbol, today, room).await);
                }
                if let Some(u) = fo.first() {
                    answer.options = self.options_around(u, today, spot(&u.symbol), None).await;
                }
            }
            Parsed::Future { .. } => {
                for u in &fo {
                    let room = PER_GROUP - answer.futures.len();
                    if room == 0 {
                        break;
                    }
                    answer.futures.extend(self.futures_of(&u.symbol, today, room).await);
                }
            }
            Parsed::Contract { month, strike, side, .. } => {
                // The best-ranked underlying with options listed answers; the
                // next is tried only when it has none at all.
                for u in &fo {
                    let rows = self.contract_rows(u, today, month.as_ref(), *strike, *side).await;
                    if !rows.is_empty() {
                        answer.options = rows;
                        break;
                    }
                }
            }
        }
        answer
    }

    /// The symbols FTS5 matches for the word-prefix tier; `None` when the
    /// text has no word or the query failed (the tier is then computed in
    /// memory).
    async fn fts_symbols(&self, text: &str) -> Option<HashSet<String>> {
        let pool = self.pool.as_ref()?;
        let query = search::fts_query(text)?;
        match sqlx::query_scalar::<_, String>("SELECT symbol FROM instruments_fts WHERE instruments_fts MATCH ? LIMIT 200")
            .bind(&query)
            .fetch_all(pool)
            .await
        {
            Ok(symbols) => Some(symbols.into_iter().collect()),
            Err(e) => {
                tracing::debug!(query = %query, error = %e, "Instrument catalog: FTS query failed — word tier computed in memory");
                None
            }
        }
    }

    /// The underlying's futures listed from `today`, nearest expiry first.
    async fn futures_of(&self, underlying: &str, today: NaiveDate, limit: usize) -> Vec<SearchRow> {
        let Some(pool) = &self.pool else { return Vec::new() };
        let rows = sqlx::query_as::<_, DbRow>(&format!(
            "{SELECT_ROW} WHERE underlying = ? AND kind = 'FUT' AND expiry >= ? ORDER BY expiry LIMIT ?"
        ))
        .bind(underlying)
        .bind(day_text(today))
        .bind(limit as i64)
        .fetch_all(pool)
        .await;
        self.rows(rows, "futures")
    }

    /// The expiries the underlying's options are listed for from `today`,
    /// nearest first: a seek per expiry (`OPTION_EXPIRIES`), never a walk
    /// of the chain.
    async fn option_expiries(&self, underlying: &str, today: NaiveDate) -> Vec<NaiveDate> {
        let Some(pool) = &self.pool else { return Vec::new() };
        match sqlx::query_scalar::<_, String>(OPTION_EXPIRIES).bind(underlying).bind(day_text(today)).fetch_all(pool).await {
            Ok(days) => days.iter().filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).collect(),
            Err(e) => {
                tracing::error!(error = %e, "Instrument catalog: expiries lookup failed");
                Vec::new()
            }
        }
    }

    /// The options at one strike of one expiry: one side, or both (CE first).
    async fn options_at(&self, underlying: &str, expiry: NaiveDate, strike: f64, side: Option<Side>) -> Vec<SearchRow> {
        let Some(pool) = &self.pool else { return Vec::new() };
        let kinds = match side {
            Some(Side::Ce) => "('CE')",
            Some(Side::Pe) => "('PE')",
            None => "('CE', 'PE')",
        };
        let rows = sqlx::query_as::<_, DbRow>(&format!(
            "{SELECT_ROW} WHERE underlying = ? AND expiry = ? AND strike BETWEEN ? AND ? AND kind IN {kinds} ORDER BY kind"
        ))
        .bind(underlying)
        .bind(day_text(expiry))
        .bind(strike - 0.001)
        .bind(strike + 0.001)
        .fetch_all(pool)
        .await;
        self.rows(rows, "options")
    }

    /// The listed strikes of one expiry nearest `centre`: up to `below` at
    /// or under it and `above` over it, ascending.
    async fn strikes_near(&self, underlying: &str, expiry: NaiveDate, centre: f64, below: usize, above: usize) -> Vec<f64> {
        let Some(pool) = &self.pool else { return Vec::new() };
        let fetch = |sql: &'static str, limit: usize| {
            sqlx::query_scalar::<_, f64>(sql)
                .bind(underlying)
                .bind(day_text(expiry))
                .bind(centre)
                .bind(limit as i64)
                .fetch_all(pool)
        };
        let under = fetch(
            "SELECT DISTINCT strike FROM instruments WHERE underlying = ? AND expiry = ? AND strike <= ? AND kind IN ('CE', 'PE') ORDER BY strike DESC LIMIT ?",
            below,
        )
        .await;
        let over = fetch(
            "SELECT DISTINCT strike FROM instruments WHERE underlying = ? AND expiry = ? AND strike > ? AND kind IN ('CE', 'PE') ORDER BY strike LIMIT ?",
            above,
        )
        .await;
        let mut strikes: Vec<f64> = Vec::new();
        for result in [under, over] {
            match result {
                Ok(found) => strikes.extend(found),
                Err(e) => tracing::error!(error = %e, "Instrument catalog: strikes lookup failed"),
            }
        }
        strikes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        strikes
    }

    /// The middle strike of one expiry's chain, for a search with no spot.
    async fn median_strike(&self, underlying: &str, expiry: NaiveDate) -> Option<f64> {
        let pool = self.pool.as_ref()?;
        sqlx::query_scalar::<_, f64>(
            "SELECT strike FROM instruments WHERE underlying = ?1 AND expiry = ?2 AND kind = 'CE' AND strike IS NOT NULL
             ORDER BY strike LIMIT 1 OFFSET (SELECT COUNT(*) / 2 FROM instruments WHERE underlying = ?1 AND expiry = ?2 AND kind = 'CE' AND strike IS NOT NULL)",
        )
        .bind(underlying)
        .bind(day_text(expiry))
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "Instrument catalog: median strike lookup failed");
            None
        })
    }

    /// Up to [`PER_GROUP`] options of the underlying's nearest expiry from
    /// `today` (or the first expiry `month` names), at the strikes nearest
    /// `centre` (the spot, else the middle of the chain), CE and PE at each.
    async fn options_around(&self, u: &Underlying, today: NaiveDate, centre: Option<f64>, month: Option<&MonthHint>) -> Vec<SearchRow> {
        let expiries = self.option_expiries(&u.symbol, today).await;
        let Some(expiry) = pick_expiry(&expiries, month) else { return Vec::new() };
        let centre = match centre {
            Some(spot) => spot,
            None => match self.median_strike(&u.symbol, expiry).await {
                Some(middle) => middle,
                None => return Vec::new(),
            },
        };
        let mut rows = Vec::new();
        for strike in self.strikes_near(&u.symbol, expiry, centre, STRIKES_EACH_SIDE, STRIKES_EACH_SIDE).await {
            rows.extend(self.options_at(&u.symbol, expiry, strike, None).await);
        }
        rows.truncate(PER_GROUP);
        rows
    }

    /// The contracts a typed `<SYM> [<MON>] <STRIKE> CE|PE` names: that
    /// strike at each listed expiry from `today` (those `month` names when
    /// it names any), nearest first; when no expiry lists the strike, the
    /// nearest strikes of the nearest expiry instead.
    async fn contract_rows(&self, u: &Underlying, today: NaiveDate, month: Option<&MonthHint>, strike: f64, side: Side) -> Vec<SearchRow> {
        let all = self.option_expiries(&u.symbol, today).await;
        let mut expiries: Vec<NaiveDate> = match month {
            Some(hint) => all.iter().copied().filter(|d| hint.matches(*d)).collect(),
            None => Vec::new(),
        };
        if expiries.is_empty() {
            expiries = all;
        }
        let mut rows = Vec::new();
        for expiry in &expiries {
            if rows.len() >= PER_GROUP {
                break;
            }
            rows.extend(self.options_at(&u.symbol, *expiry, strike, Some(side)).await);
        }
        if rows.is_empty() {
            if let Some(expiry) = expiries.first() {
                for near in self.strikes_near(&u.symbol, *expiry, strike, STRIKES_EACH_SIDE, STRIKES_EACH_SIDE).await {
                    rows.extend(self.options_at(&u.symbol, *expiry, near, Some(side)).await);
                }
            }
        }
        rows.truncate(PER_GROUP);
        rows
    }

    fn rows(&self, result: Result<Vec<DbRow>, sqlx::Error>, what: &str) -> Vec<SearchRow> {
        match result {
            Ok(rows) => {
                let index = self.index();
                rows.into_iter().map(|r| r.into_row(&index)).collect()
            }
            Err(e) => {
                tracing::error!(error = %e, "Instrument catalog: {what} lookup failed");
                Vec::new()
            }
        }
    }
}

/// The first expiry `month` names, else the nearest.
fn pick_expiry(expiries: &[NaiveDate], month: Option<&MonthHint>) -> Option<NaiveDate> {
    month
        .and_then(|hint| expiries.iter().copied().find(|d| hint.matches(*d)))
        .or_else(|| expiries.first().copied())
}

fn day_text(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

/// An underlying's own row (its cash or index listing), from memory.
fn cash_row(u: &Underlying, tier: Tier) -> Option<SearchRow> {
    Some(SearchRow {
        id: u.id.clone()?,
        kind: match u.kind {
            UnderlyingKind::Index => Kind::Index,
            UnderlyingKind::Equity => Kind::Eq,
            UnderlyingKind::FoOnly => return None,
        },
        symbol: u.symbol.clone(),
        name: u.name.clone(),
        exchange: u.exchange.clone()?,
        trading_symbol: u.trading_symbol.clone()?,
        underlying: u.symbol.clone(),
        expiry: None,
        strike: None,
        lot_size: u.lot_size,
        tick_size: u.tick_size,
        ws_key: u.ws_key.clone()?,
        has_fo: u.has_fo,
        tier: Some(tier),
    })
}

/// One segment's row of the last build, as `instrument_load` keeps it.
struct LoadRow {
    segment: String,
    rows: i64,
    mtime: Option<i64>,
    kinds: BTreeMap<String, i64>,
}

const SELECT_ROW: &str =
    "SELECT id, kind, symbol, name, exchange, trading_symbol, underlying, expiry, strike, lot_size, tick_size, ws_key FROM instruments";

/// The nearest expiry from `?2` under underlying `?1`: the least of one
/// `MIN(expiry)` per kind, each a seek on the chain index
/// (`underlying, kind, expiry`), so NIFTY's 17,000 contracts are never
/// walked. `NULL` when nothing is listed from that day.
const NEAREST_EXPIRY: &str = "SELECT MIN(e) FROM (
    SELECT MIN(expiry) AS e FROM instruments WHERE underlying = ?1 AND kind = 'FUT' AND expiry >= ?2
    UNION ALL
    SELECT MIN(expiry) FROM instruments WHERE underlying = ?1 AND kind = 'CE' AND expiry >= ?2
    UNION ALL
    SELECT MIN(expiry) FROM instruments WHERE underlying = ?1 AND kind = 'PE' AND expiry >= ?2
)";

/// The option expiries from `?2` under underlying `?1`, ascending: each row
/// is the next expiry after the last, found by a `MIN(expiry)` seek on the
/// chain index per side (CE, then PE), so the cost is the number of
/// expiries, not of contracts. `DISTINCT` is implied: a `MIN` strictly
/// greater than the last never repeats one.
const OPTION_EXPIRIES: &str = "WITH RECURSIVE next(expiry) AS (
    SELECT MIN(e) FROM (
        SELECT MIN(expiry) AS e FROM instruments WHERE underlying = ?1 AND kind = 'CE' AND expiry >= ?2
        UNION ALL
        SELECT MIN(expiry) FROM instruments WHERE underlying = ?1 AND kind = 'PE' AND expiry >= ?2
    )
    UNION ALL
    SELECT (
        SELECT MIN(e) FROM (
            SELECT MIN(expiry) AS e FROM instruments WHERE underlying = ?1 AND kind = 'CE' AND expiry > next.expiry
            UNION ALL
            SELECT MIN(expiry) FROM instruments WHERE underlying = ?1 AND kind = 'PE' AND expiry > next.expiry
        )
    ) FROM next WHERE next.expiry IS NOT NULL
)
SELECT expiry FROM next WHERE expiry IS NOT NULL";

/// `SELECT_ROW`'s columns and the instrument page's, by primary key.
const SELECT_DETAIL: &str =
    "SELECT id, kind, symbol, name, exchange, trading_symbol, underlying, expiry, strike, lot_size, tick_size, ws_key, segment, token, isin FROM instruments WHERE id = ?";

#[derive(sqlx::FromRow)]
struct DbRow {
    id: String,
    kind: String,
    symbol: String,
    name: String,
    exchange: String,
    trading_symbol: String,
    underlying: String,
    expiry: Option<String>,
    strike: Option<f64>,
    lot_size: i64,
    tick_size: f64,
    ws_key: String,
}

impl DbRow {
    fn into_row(self, index: &SearchIndex) -> SearchRow {
        let has_fo = index.get(&self.underlying).is_some_and(|u| u.has_fo) || Kind::parse(&self.kind).is_some_and(|k| k != Kind::Eq && k != Kind::Index);
        SearchRow {
            id: self.id,
            kind: Kind::parse(&self.kind).unwrap_or(Kind::Eq),
            symbol: self.symbol,
            name: self.name,
            exchange: self.exchange,
            trading_symbol: self.trading_symbol,
            underlying: self.underlying,
            expiry: self.expiry,
            strike: self.strike,
            lot_size: self.lot_size,
            tick_size: self.tick_size,
            ws_key: self.ws_key,
            has_fo,
            tier: None,
        }
    }
}

/// A row with what the instrument page shows beyond a search row.
#[derive(sqlx::FromRow)]
struct DetailRow {
    #[sqlx(flatten)]
    row: DbRow,
    segment: String,
    token: String,
    isin: Option<String>,
}

#[derive(sqlx::FromRow)]
struct UnderlyingRow {
    symbol: String,
    name: String,
    kind: String,
    id: Option<String>,
    has_fo: bool,
    fo_index: bool,
    fo_segment: Option<String>,
    isin: Option<String>,
    exchange: Option<String>,
    ws_key: Option<String>,
    trading_symbol: Option<String>,
    lot_size: i64,
    tick_size: f64,
}

impl UnderlyingRow {
    fn into_underlying(self) -> Option<Underlying> {
        Some(Underlying {
            symbol: self.symbol,
            name: self.name,
            kind: UnderlyingKind::parse(&self.kind)?,
            id: self.id,
            has_fo: self.has_fo,
            fo_index: self.fo_index,
            fo_segment: self.fo_segment,
            isin: self.isin,
            exchange: self.exchange,
            ws_key: self.ws_key,
            trading_symbol: self.trading_symbol,
            lot_size: self.lot_size,
            tick_size: self.tick_size,
        })
    }
}
