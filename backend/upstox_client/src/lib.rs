//! Read-only Upstox client for market data.
//!
//! Authenticates with an **Analytics Token** (generated once a year from the
//! Upstox Developer Apps page → Analytics tab). That token is read-only by
//! design — it cannot place, modify or cancel orders — so nothing in this crate
//! can ever touch money. Order execution stays on Kotak.
//!
//! Endpoints used (all documented at upstox.com/developer/api-documentation):
//! - `GET /v2/option/contract`      — list option contracts → nearest expiry
//! - `GET /v2/option/chain`         — full put/call chain with `oi` + `prev_oi`
//! - `GET /v2/market-quote/quotes`  — full quote (`last_price`, `average_price`)
//! - `GET /v3/historical-candle/…`  — OHLCV candles for past days
//! - `GET /v3/historical-candle/intraday/…` — today's OHLCV candles
//! - `GET /v3/feed/market-data-feed/authorize` — the live feed's WebSocket URL
//!   (see [`feed`])
//! - instruments JSON (gzip)        — futures and options by contract, for
//!   any underlying (the indices, or a stock such as `RELIANCE`)
//!
//! The instruments files are a few MB gzipped and tens of MB decompressed.
//! They are never materialised: every caller stream-filters the rows it
//! wants in `spawn_blocking`. With [`UpstoxClient::with_instruments_cache`]
//! set, the gzipped bytes are kept on disk per instruments day (see
//! [`instruments_day`]: it starts at 07:00 IST, after Upstox publishes the
//! day's file), so a second underlying asked for that day reads the file
//! instead of downloading it again.

pub mod feed;

use std::collections::HashMap;
use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::de::{DeserializeOwned, DeserializeSeed, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use shared_domain::telemetry;

pub use feed::LtpTick;

const API_BASE: &str = "https://api.upstox.com/v2";
const API_V3: &str = "https://api.upstox.com/v3";
/// Per-request budget for the small JSON API calls.
const API_TIMEOUT: Duration = Duration::from_secs(20);
/// Budget for downloading a multi-MB gzipped instruments file.
const INSTRUMENTS_TIMEOUT: Duration = Duration::from_secs(180);
/// The hour (IST) an instruments day starts: Upstox rebuilds its
/// instruments files early each morning (about 06:00 IST), so a file fetched
/// before this belongs to the day before and is never served after it.
pub const INSTRUMENTS_DAY_STARTS_AT_HOUR: i64 = 7;

/// The instruments day `now` falls in: the IST date, rolled at
/// [`INSTRUMENTS_DAY_STARTS_AT_HOUR`] instead of midnight. A file fetched
/// at 02:00 IST is the previous day's (Upstox has not refreshed it yet), so
/// it is cached, and kept in memory, under that day and fetched afresh from
/// 07:00 — contracts listed that morning (a new weekly series, a new
/// far-month future) are never missing all day.
pub fn instruments_day(now: DateTime<FixedOffset>) -> NaiveDate {
    (now.with_timezone(&shared_domain::ist_offset()) - chrono::Duration::hours(INSTRUMENTS_DAY_STARTS_AT_HOUR)).date_naive()
}
/// Sent on every request. `assets.upstox.com` (the instruments files) sits
/// behind Cloudflare and answers 403 to a request with no User-Agent, which is
/// what reqwest sends by default — that silently emptied the futures map and
/// left the VWAP columns blank while the api.upstox.com calls kept working.
const USER_AGENT: &str = concat!("auto-trader/", env!("CARGO_PKG_VERSION"));
pub const NSE_INSTRUMENTS_URL: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/NSE.json.gz";
pub const BSE_INSTRUMENTS_URL: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/BSE.json.gz";

#[derive(Debug, thiserror::Error)]
pub enum UpstoxError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstox api returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("decode error: {0}")]
    Decode(String),
    #[error("market-data feed: {0}")]
    Feed(String),
}

/// Fixed metric label for an API path (the paths are literals in this file).
fn endpoint_label(path: &str) -> &'static str {
    match path {
        "/option/contract" => "option_contract",
        "/option/chain" => "option_chain",
        "/market-quote/quotes" => "quotes",
        _ => "other",
    }
}

impl UpstoxError {
    /// 401/403 — the analytics token is missing, revoked, or expired.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, UpstoxError::Api { status: 401 | 403, .. })
    }
}

/// Fixed metric label for a call's outcome.
fn result_label<T>(res: &Result<T, UpstoxError>) -> &'static str {
    match res {
        Ok(_) => "ok",
        Err(UpstoxError::Api { status, .. }) => telemetry::status_class(*status),
        Err(UpstoxError::Decode(_)) => "decode_error",
        Err(_) => "transport_error",
    }
}

/// `key` as one URL path segment: `NSE_INDEX|Nifty 50` → `NSE_INDEX%7CNifty%2050`.
fn path_segment(key: &str) -> String {
    key.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Accepts a JSON number, a numeric string, or null (→ 0.0). Upstox returns
/// `null` for fields on illiquid strikes, which a plain `f64` would reject.
fn num<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    })
}

/// Like [`num`], but keeps "absent" distinct from zero: null, missing or
/// unparseable → `None`.
fn opt_num<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MarketData {
    #[serde(default, deserialize_with = "num")]
    pub ltp: f64,
    #[serde(default, deserialize_with = "num")]
    pub volume: f64,
    #[serde(default, deserialize_with = "num")]
    pub oi: f64,
    /// `None` when Upstox sends null. Callers must not treat that as zero, or
    /// the strike's entire OI would count as a same-day build-up.
    #[serde(default, deserialize_with = "opt_num")]
    pub prev_oi: Option<f64>,
    #[serde(default, deserialize_with = "num")]
    pub close_price: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OptionGreeks {
    #[serde(default, deserialize_with = "num")]
    pub iv: f64,
    #[serde(default, deserialize_with = "num")]
    pub delta: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OptionSide {
    #[serde(default)]
    pub instrument_key: String,
    #[serde(default)]
    pub market_data: Option<MarketData>,
    #[serde(default)]
    pub option_greeks: Option<OptionGreeks>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OptionChainStrike {
    #[serde(default)]
    pub expiry: String,
    #[serde(default, deserialize_with = "num")]
    pub strike_price: f64,
    #[serde(default, deserialize_with = "num")]
    pub underlying_spot_price: f64,
    #[serde(default)]
    pub call_options: Option<OptionSide>,
    #[serde(default)]
    pub put_options: Option<OptionSide>,
}

#[derive(Debug, Clone, Deserialize)]
struct OptionContract {
    #[serde(default)]
    expiry: String,
}

/// A quote's day: open, high, low and the previous close. `None` where
/// Upstox sends null or nothing.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Ohlc {
    #[serde(default, deserialize_with = "opt_num")]
    pub open: Option<f64>,
    #[serde(default, deserialize_with = "opt_num")]
    pub high: Option<f64>,
    #[serde(default, deserialize_with = "opt_num")]
    pub low: Option<f64>,
    #[serde(default, deserialize_with = "opt_num")]
    pub close: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Quote {
    /// Pipe form (`NSE_FO|12345`) — the response map itself is keyed by the
    /// colon/trading-symbol form, so match on this field instead.
    #[serde(default)]
    pub instrument_token: String,
    #[serde(default, deserialize_with = "num")]
    pub last_price: f64,
    /// Exchange average traded price for the day, i.e. VWAP.
    #[serde(default, deserialize_with = "num")]
    pub average_price: f64,
    #[serde(default, deserialize_with = "num")]
    pub volume: f64,
    #[serde(default, deserialize_with = "num")]
    pub oi: f64,
    /// The day's open, high and low and the previous close, when sent.
    #[serde(default)]
    pub ohlc: Option<Ohlc>,
    /// Points from the previous session's close to `last_price`.
    #[serde(default, deserialize_with = "num")]
    pub net_change: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FutureContract {
    pub underlying_symbol: String,
    pub instrument_key: String,
    pub trading_symbol: String,
}

/// One OHLCV candle from the V3 candle APIs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Candle {
    /// Unix ms of the candle's start (Upstox stamps a candle by its start).
    pub start_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Traded during the candle; 0 for an index.
    pub volume: f64,
    pub oi: f64,
}

#[derive(Deserialize)]
struct CandleData {
    #[serde(default)]
    candles: Vec<Vec<serde_json::Value>>,
}

/// `[timestamp, open, high, low, close, volume, oi]` rows, newest first as
/// Upstox sends them, into candles oldest first. A row that can't be read is
/// dropped, never guessed at.
fn parse_candles(rows: &[Vec<serde_json::Value>]) -> Vec<Candle> {
    let mut out: Vec<Candle> = rows
        .iter()
        .filter_map(|r| {
            let start_ms = DateTime::parse_from_rfc3339(r.first()?.as_str()?).ok()?.timestamp_millis();
            let n = |i: usize| r.get(i).and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()));
            Some(Candle {
                start_ms,
                open: n(1)?,
                high: n(2)?,
                low: n(3)?,
                close: n(4)?,
                volume: n(5).unwrap_or(0.0),
                oi: n(6).unwrap_or(0.0),
            })
        })
        .collect();
    out.sort_by_key(|c| c.start_ms);
    out.dedup_by_key(|c| c.start_ms);
    out
}

/// An index future or option, from the instruments files.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Derivative {
    pub underlying: String,
    /// `"FUT"`, `"CE"` or `"PE"`.
    pub kind: String,
    /// The IST date it expires on.
    pub expiry: NaiveDate,
    /// 0 for a future.
    pub strike: f64,
    pub instrument_key: String,
    pub lot_size: u32,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct UpstoxClient {
    http: reqwest::Client,
    token: String,
    /// Where the instruments files are kept for the IST day, when set —
    /// see [`Self::with_instruments_cache`].
    instruments_cache: Option<PathBuf>,
}

impl UpstoxClient {
    pub fn new(analytics_token: impl Into<String>) -> Result<Self, UpstoxError> {
        // No client-wide total timeout: API calls and the large instruments
        // download get separate per-request budgets.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self { http, token: analytics_token.into(), instruments_cache: None })
    }

    /// Keep each downloaded instruments file (its gzipped bytes, a few MB)
    /// under `dir/<instruments day>/<file>` and read it from there for the
    /// rest of that day ([`instruments_day`]: 07:00 IST to 07:00 IST); older
    /// days' directories are removed as a new one is written. Without this,
    /// every call downloads the file again.
    pub fn with_instruments_cache(mut self, dir: impl Into<PathBuf>) -> Self {
        self.instruments_cache = Some(dir.into());
        self
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T, UpstoxError> {
        let started = std::time::Instant::now();
        let res = self.get_json_inner(path, params).await;
        telemetry::upstox_request(endpoint_label(path), result_label(&res), started.elapsed().as_secs_f64());
        res
    }

    async fn get_json_inner<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T, UpstoxError> {
        let url = reqwest::Url::parse_with_params(&format!("{API_BASE}{path}"), params)
            .map_err(|e| UpstoxError::Decode(e.to_string()))?;
        self.fetch(url, path).await
    }

    /// GET a V3 `path` (its segments already percent-encoded), timed under
    /// `label`.
    async fn get_v3<T: DeserializeOwned>(&self, path: &str, label: &'static str) -> Result<T, UpstoxError> {
        let started = std::time::Instant::now();
        let res = match reqwest::Url::parse(&format!("{API_V3}{path}")) {
            Ok(url) => self.fetch(url, label).await,
            Err(e) => Err(UpstoxError::Decode(e.to_string())),
        };
        telemetry::upstox_request(label, result_label(&res), started.elapsed().as_secs_f64());
        res
    }

    /// GET `url` with the token and unwrap the response's `data`; `what`
    /// names the call in a decode error.
    async fn fetch<T: DeserializeOwned>(&self, url: reqwest::Url, what: &str) -> Result<T, UpstoxError> {
        let resp = self
            .http
            .get(url)
            .timeout(API_TIMEOUT)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(UpstoxError::Api {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }
        serde_json::from_str::<Envelope<T>>(&body)
            .map(|e| e.data)
            .map_err(|e| UpstoxError::Decode(format!("{what}: {e}")))
    }

    /// `interval_min`-minute candles for `key` (e.g. `NSE_INDEX|Nifty 50`) on
    /// the days `from..=to`, oldest first. Past days only: Upstox answers
    /// nothing for today here (see [`Self::intraday_candles`]), and allows at
    /// most a month per request for intervals up to 15 minutes.
    pub async fn historical_candles(
        &self,
        key: &str,
        interval_min: u32,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<Candle>, UpstoxError> {
        let path = format!(
            "/historical-candle/{}/minutes/{interval_min}/{}/{}",
            path_segment(key),
            to.format("%Y-%m-%d"),
            from.format("%Y-%m-%d"),
        );
        let data: CandleData = self.get_v3(&path, "historical_candle").await?;
        Ok(parse_candles(&data.candles))
    }

    /// Today's `interval_min`-minute candles for `key`, oldest first. During
    /// the session the newest one is still forming — Upstox sends it with
    /// only a last price — so callers must not take it as a finished bar.
    pub async fn intraday_candles(&self, key: &str, interval_min: u32) -> Result<Vec<Candle>, UpstoxError> {
        let path = format!("/historical-candle/intraday/{}/minutes/{interval_min}", path_segment(key));
        let data: CandleData = self.get_v3(&path, "intraday_candle").await?;
        Ok(parse_candles(&data.candles))
    }

    /// Nearest option expiry on or after `today` for an underlying such as
    /// `NSE_INDEX|Nifty 50`.
    pub async fn nearest_option_expiry(
        &self,
        underlying_key: &str,
        today: NaiveDate,
    ) -> Result<NaiveDate, UpstoxError> {
        let contracts: Vec<OptionContract> = self
            .get_json("/option/contract", &[("instrument_key", underlying_key)])
            .await?;
        contracts
            .iter()
            .filter_map(|c| NaiveDate::parse_from_str(&c.expiry, "%Y-%m-%d").ok())
            .filter(|d| *d >= today)
            .min()
            .ok_or_else(|| UpstoxError::Decode(format!("no live option expiry for {underlying_key}")))
    }

    pub async fn option_chain(
        &self,
        underlying_key: &str,
        expiry: NaiveDate,
    ) -> Result<Vec<OptionChainStrike>, UpstoxError> {
        let expiry = expiry.format("%Y-%m-%d").to_string();
        self.get_json(
            "/option/chain",
            &[("instrument_key", underlying_key), ("expiry_date", &expiry)],
        )
        .await
    }

    /// Full market quotes, returned keyed by pipe-form instrument key.
    pub async fn quotes(&self, keys: &[String]) -> Result<HashMap<String, Quote>, UpstoxError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let joined = keys.join(",");
        let raw: HashMap<String, Quote> = self
            .get_json("/market-quote/quotes", &[("instrument_key", &joined)])
            .await?;
        Ok(raw.into_values().map(|q| (q.instrument_token.clone(), q)).collect())
    }

    /// Downloads an instruments file (`NSE_INSTRUMENTS_URL` / `BSE_INSTRUMENTS_URL`)
    /// and returns the nearest still-trading future for each wanted underlying
    /// symbol (e.g. `NIFTY`, `BANKNIFTY`, `SENSEX`). A contract expiring on
    /// `today` (IST) still counts: it trades until the close.
    ///
    /// The decompressed file is tens of MB, and the production VM has under
    /// 1 GB of RAM, so it is stream-filtered row by row instead of being
    /// materialised.
    pub async fn nearest_index_futures(
        &self,
        instruments_url: &str,
        wanted: &[&str],
        today: NaiveDate,
    ) -> Result<HashMap<String, FutureContract>, UpstoxError> {
        let wanted: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();
        self.filter_instruments(instruments_url, move |gz| Ok(pick_nearest_futures(filter_futures(gz, &wanted)?, today)))
            .await
    }

    /// Every future and option on the `wanted` underlyings — an index
    /// (`NIFTY`, `SENSEX`) or a stock (`RELIANCE`) by its
    /// `underlying_symbol` — in an instruments file, stream-filtered the
    /// same way as [`Self::nearest_index_futures`] and read from the day's
    /// cached file when [`Self::with_instruments_cache`] is set. For the six
    /// indices that is about 8,000 contracts across the NSE and BSE files;
    /// a stock has a few hundred.
    pub async fn derivatives(&self, instruments_url: &str, wanted: &[&str]) -> Result<Vec<Derivative>, UpstoxError> {
        let wanted: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();
        self.filter_instruments(instruments_url, move |gz| filter_derivatives(gz, &wanted)).await
    }

    /// [`Self::derivatives`] under its older name, for the six indices.
    pub async fn index_derivatives(
        &self,
        instruments_url: &str,
        wanted: &[&str],
    ) -> Result<Vec<Derivative>, UpstoxError> {
        self.derivatives(instruments_url, wanted).await
    }

    /// Every future on the `wanted` underlyings in an instruments file — the
    /// near, next and far months — stream-filtered the same way as
    /// [`Self::nearest_index_futures`].
    pub async fn index_futures(&self, instruments_url: &str, wanted: &[&str]) -> Result<Vec<Derivative>, UpstoxError> {
        let wanted: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();
        self.filter_instruments(instruments_url, move |gz| Ok(filter_futures(gz, &wanted)?.iter().filter_map(to_derivative).collect()))
            .await
    }

    /// Run `filter` (a stream filter over the gzipped bytes of an
    /// instruments file, a few MB; the rows are decompressed and read one at
    /// a time, never materialised) on a blocking thread, against the
    /// instruments day's copy from the cache directory when one is set and
    /// holds it, else a download, which is then written there for the rest
    /// of that day ([`instruments_day`]).
    ///
    /// A cached file the filter cannot decode (a corrupt gzip or JSON) is
    /// removed and the file downloaded once more, so a bad write never
    /// serves "no derivatives" for a whole day. A decode error on the
    /// download itself is returned as it is.
    async fn filter_instruments<T, F>(&self, instruments_url: &str, filter: F) -> Result<T, UpstoxError>
    where
        T: Send + 'static,
        F: Fn(&[u8]) -> Result<T, UpstoxError> + Send + Sync + 'static,
    {
        self.filter_instruments_at(instruments_url, filter, shared_domain::now_ist()).await
    }

    /// [`Self::filter_instruments`] as of `now`.
    async fn filter_instruments_at<T, F>(&self, instruments_url: &str, filter: F, now: DateTime<FixedOffset>) -> Result<T, UpstoxError>
    where
        T: Send + 'static,
        F: Fn(&[u8]) -> Result<T, UpstoxError> + Send + Sync + 'static,
    {
        let filter = std::sync::Arc::new(filter);
        let today = instruments_day(now);
        let cached = self.instruments_cache.as_ref().map(|dir| instruments_cache_path(dir, today, instruments_url));
        if let Some(path) = &cached {
            if let Some(bytes) = read_cached_instruments(path).await {
                let run = std::sync::Arc::clone(&filter);
                match run_filter(move || run(&bytes)).await {
                    Err(UpstoxError::Decode(e)) => {
                        tracing::warn!(path = %path.display(), error = %e, "Upstox instruments cache is corrupt — removed, downloading");
                        if let Err(e) = tokio::fs::remove_file(path).await {
                            tracing::warn!(path = %path.display(), error = %e, "Corrupt Upstox instruments cache could not be removed");
                        }
                    }
                    other => return other,
                }
            }
        }
        let bytes = self.download_instruments(instruments_url).await?;
        if let (Some(dir), Some(path)) = (self.instruments_cache.clone(), cached) {
            let copy = bytes.clone();
            let target = path.clone();
            let stored = tokio::task::spawn_blocking(move || store_instruments(&dir, &target, &copy, today)).await;
            match stored {
                Ok(Ok(())) => tracing::info!(path = %path.display(), bytes = bytes.len(), "Upstox instruments file cached for the instruments day"),
                Ok(Err(e)) => tracing::warn!(path = %path.display(), error = %e, "Upstox instruments file could not be cached"),
                Err(e) => tracing::warn!(error = %e, "Upstox instruments cache write did not finish"),
            }
        }
        run_filter(move || filter(&bytes)).await
    }

    async fn download_instruments(&self, instruments_url: &str) -> Result<bytes::Bytes, UpstoxError> {
        let resp = self.http.get(instruments_url).timeout(INSTRUMENTS_TIMEOUT).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstoxError::Api { status: status.as_u16(), body: String::new() });
        }
        Ok(resp.bytes().await?)
    }
}

/// `filter` on a blocking thread; a thread that did not finish is a
/// `Decode` error, like a file it could not read.
async fn run_filter<T, F>(filter: F) -> Result<T, UpstoxError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, UpstoxError> + Send + 'static,
{
    tokio::task::spawn_blocking(filter).await.map_err(|e| UpstoxError::Decode(e.to_string()))?
}

// ---------------------------------------------------------------------------
// Instruments cache on disk
// ---------------------------------------------------------------------------

/// The day's cached instruments file, when `path` holds one: `None` for no
/// file, an empty one or one that cannot be read (logged), and the download
/// goes ahead.
async fn read_cached_instruments(path: &Path) -> Option<bytes::Bytes> {
    match tokio::fs::read(path).await {
        Ok(bytes) if !bytes.is_empty() => {
            tracing::debug!(path = %path.display(), "Upstox instruments file read from today's cache");
            Some(bytes.into())
        }
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Upstox instruments cache unreadable — downloading");
            None
        }
    }
}

/// `dir/<YYYY-MM-DD>/<file>` for an instruments URL: the file is the URL's
/// last path segment with anything but letters, digits, `.`, `-` and `_`
/// replaced, so a URL can never name a path outside the day's directory.
fn instruments_cache_path(dir: &Path, day: NaiveDate, instruments_url: &str) -> PathBuf {
    let raw = instruments_url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let mut name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    if name.trim_matches('.').is_empty() {
        name = "instruments.json.gz".to_string();
    }
    dir.join(day.format("%Y-%m-%d").to_string()).join(name)
}

/// Write `bytes` to `path` whole (a temp file of its own — the process id
/// and a counter, so two writes of the same file at once never share one —
/// renamed over it, so a reader never sees a half file) and drop every other
/// day's directory under `dir`.
fn store_instruments(dir: &Path, path: &Path, bytes: &[u8], today: NaiveDate) -> std::io::Result<()> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)?;
    prune_instruments_cache(dir, today);
    Ok(())
}

/// Remove every day directory under `dir` but `today`'s. Anything that is
/// not a `YYYY-MM-DD` directory is left alone.
fn prune_instruments_cache(dir: &Path, today: NaiveDate) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let keep = today.format("%Y-%m-%d").to_string();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_day = NaiveDate::parse_from_str(name, "%Y-%m-%d").is_ok();
        if is_day && name != keep && entry.path().is_dir() {
            if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                tracing::debug!(path = %entry.path().display(), error = %e, "Old Upstox instruments cache not removed");
            }
        }
    }
}

/// The futures on `wanted` in a gzipped instruments file, decompressed and
/// filtered as a stream.
fn filter_futures(gz: &[u8], wanted: &[String]) -> Result<Vec<InstrumentRow>, UpstoxError> {
    let reader = BufReader::new(flate2::read::GzDecoder::new(gz));
    let mut de = serde_json::Deserializer::from_reader(reader);
    FutFilter { wanted }
        .deserialize(&mut de)
        .map_err(|e| UpstoxError::Decode(format!("instruments: {e}")))
}

/// The futures and options on `wanted` in a gzipped instruments file,
/// decompressed and filtered as a stream.
fn filter_derivatives(gz: &[u8], wanted: &[String]) -> Result<Vec<Derivative>, UpstoxError> {
    let reader = BufReader::new(flate2::read::GzDecoder::new(gz));
    let mut de = serde_json::Deserializer::from_reader(reader);
    let keep = |r: &InstrumentRow| is_derivative_of(r, wanted);
    let rows = RowFilter { keep: &keep }
        .deserialize(&mut de)
        .map_err(|e| UpstoxError::Decode(format!("instruments: {e}")))?;
    Ok(rows.iter().filter_map(to_derivative).collect())
}

// ---------------------------------------------------------------------------
// Instruments stream filter
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InstrumentRow {
    #[serde(default)]
    segment: String,
    #[serde(default)]
    instrument_type: String,
    #[serde(default)]
    underlying_symbol: String,
    #[serde(default)]
    instrument_key: String,
    #[serde(default)]
    trading_symbol: String,
    #[serde(default, deserialize_with = "num")]
    expiry: f64,
    #[serde(default, deserialize_with = "num")]
    strike_price: f64,
    #[serde(default, deserialize_with = "num")]
    lot_size: f64,
}

/// A future or option on one of `wanted` (its `underlying_symbol`: `NIFTY`,
/// `RELIANCE`), in an F&O segment.
fn is_derivative_of(row: &InstrumentRow, wanted: &[String]) -> bool {
    matches!(row.instrument_type.as_str(), "FUT" | "CE" | "PE")
        && row.segment.ends_with("_FO")
        && wanted.iter().any(|w| *w == row.underlying_symbol)
}

/// Upstox stamps expiry as epoch millis on the expiry day itself, so the IST
/// date of that instant is the expiry date.
fn to_derivative(row: &InstrumentRow) -> Option<Derivative> {
    let expiry = DateTime::from_timestamp_millis(row.expiry as i64)?
        .with_timezone(&shared_domain::ist_offset())
        .date_naive();
    Some(Derivative {
        underlying: row.underlying_symbol.clone(),
        kind: row.instrument_type.clone(),
        expiry,
        strike: if row.instrument_type == "FUT" { 0.0 } else { row.strike_price },
        instrument_key: row.instrument_key.clone(),
        lot_size: row.lot_size.max(0.0) as u32,
    })
}

/// Keeps the rows `keep` accepts while the instruments array streams past.
struct RowFilter<'a, F: Fn(&InstrumentRow) -> bool> {
    keep: &'a F,
}

impl<'de, F: Fn(&InstrumentRow) -> bool> DeserializeSeed<'de> for RowFilter<'_, F> {
    type Value = Vec<InstrumentRow>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de, F: Fn(&InstrumentRow) -> bool> Visitor<'de> for RowFilter<'_, F> {
    type Value = Vec<InstrumentRow>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of instruments")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(row) = seq.next_element::<InstrumentRow>()? {
            if (self.keep)(&row) {
                out.push(row);
            }
        }
        Ok(out)
    }
}

struct FutFilter<'a> {
    wanted: &'a [String],
}

impl<'de> DeserializeSeed<'de> for FutFilter<'_> {
    type Value = Vec<InstrumentRow>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for FutFilter<'_> {
    type Value = Vec<InstrumentRow>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of instruments")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(row) = seq.next_element::<InstrumentRow>()? {
            if row.instrument_type == "FUT"
                && row.segment.ends_with("_FO")
                && self.wanted.iter().any(|w| *w == row.underlying_symbol)
            {
                out.push(row);
            }
        }
        Ok(out)
    }
}

/// Upstox expiry epochs are 00:00 IST of the expiry date, so compare IST
/// dates, not instants: an epoch-millis `exp <= now` check would drop the
/// near contract on its own expiry day while it is still trading.
fn pick_nearest_futures(rows: Vec<InstrumentRow>, today: NaiveDate) -> HashMap<String, FutureContract> {
    let mut best: HashMap<String, (i64, InstrumentRow)> = HashMap::new();
    for row in rows {
        let exp = row.expiry as i64;
        let Some(exp_date) = DateTime::from_timestamp_millis(exp)
            .map(|t| t.with_timezone(&shared_domain::ist_offset()).date_naive())
        else {
            continue;
        };
        if exp_date < today {
            continue;
        }
        match best.get(&row.underlying_symbol) {
            Some((cur, _)) if *cur <= exp => {}
            _ => {
                best.insert(row.underlying_symbol.clone(), (exp, row));
            }
        }
    }
    best.into_iter()
        .map(|(sym, (_, row))| {
            (
                sym,
                FutureContract {
                    underlying_symbol: row.underlying_symbol,
                    instrument_key: row.instrument_key,
                    trading_symbol: row.trading_symbol,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_option_chain_sample() {
        let body = r#"{"status":"success","data":[{"expiry":"2025-02-13","pcr":7515.3,"strike_price":21100,
            "underlying_key":"NSE_INDEX|Nifty 50","underlying_spot_price":22976.2,
            "call_options":{"instrument_key":"NSE_FO|51059","market_data":{"ltp":2449.9,"volume":0,"oi":750,
              "close_price":2449.9,"bid_price":1856.65,"bid_qty":1125,"ask_price":1941.65,"ask_qty":1125,"prev_oi":1500},
              "option_greeks":{"vega":4.1731,"theta":-472.8941,"gamma":0.0001,"delta":0.743,"iv":262.31,"pop":40.56}},
            "put_options":{"instrument_key":"NSE_FO|51060","market_data":{"ltp":0.3,"volume":22315725,"oi":5636475,
              "close_price":0.35,"bid_price":0.3,"bid_qty":1979400,"ask_price":0.35,"ask_qty":2152500,"prev_oi":null},
              "option_greeks":null}}]}"#;
        let env: Envelope<Vec<OptionChainStrike>> = serde_json::from_str(body).unwrap();
        let s = &env.data[0];
        assert_eq!(s.strike_price, 21100.0);
        let call = s.call_options.as_ref().unwrap().market_data.as_ref().unwrap();
        assert_eq!(call.oi, 750.0);
        assert_eq!(call.prev_oi, Some(1500.0));
        let put = s.put_options.as_ref().unwrap().market_data.as_ref().unwrap();
        assert_eq!(put.prev_oi, None);
    }

    /// Epoch millis of 00:00 IST on `y-m-d`, the form Upstox uses for expiry.
    fn ist_midnight_ms(y: i32, m: u32, d: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn filters_instruments_stream_to_nearest_future() {
        let sep = ist_midnight_ms(2026, 9, 24);
        let oct = ist_midnight_ms(2026, 10, 29);
        let nov = ist_midnight_ms(2026, 11, 26);
        let aug = ist_midnight_ms(2026, 8, 27);
        let json = format!(r#"[
            {{"segment":"NSE_INDEX","instrument_type":"INDEX","instrument_key":"NSE_INDEX|Nifty 50"}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|3","trading_symbol":"NIFTY FUT NOV","expiry":{nov}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|2","trading_symbol":"NIFTY FUT OCT","expiry":{oct}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|1","trading_symbol":"NIFTY FUT SEP","expiry":{sep}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|0","trading_symbol":"NIFTY FUT AUG","expiry":{aug}}},
            {{"segment":"NSE_FO","instrument_type":"CE","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|9","expiry":{sep}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"RELIANCE","instrument_key":"NSE_FO|7","expiry":{sep}}}
        ]"#);
        let wanted = vec!["NIFTY".to_string()];
        let rows = |json: &str| {
            let mut de = serde_json::Deserializer::from_str(json);
            FutFilter { wanted: &wanted }.deserialize(&mut de).unwrap()
        };
        assert_eq!(rows(&json).len(), 4);
        let date = |d| NaiveDate::from_ymd_opt(2026, 9, d).unwrap();

        // Before expiry day: the September contract.
        let picked = pick_nearest_futures(rows(&json), date(16));
        assert_eq!(picked["NIFTY"].instrument_key, "NSE_FO|1");
        assert!(!picked.contains_key("RELIANCE"));
        // On expiry day itself September still trades, so it is still picked.
        assert_eq!(pick_nearest_futures(rows(&json), date(24))["NIFTY"].instrument_key, "NSE_FO|1");
        // The day after, it rolls to October.
        assert_eq!(pick_nearest_futures(rows(&json), date(25))["NIFTY"].instrument_key, "NSE_FO|2");
    }

    fn ist_ms(d: u32, h: u32, m: u32) -> i64 {
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn candles_come_back_oldest_first_and_unreadable_rows_are_dropped() {
        // Shape of a real intraday answer: newest first, numbers or strings.
        let body = r#"{"status":"success","data":{"candles":[
            ["2026-10-21T09:20:00+05:30",23039.2,23045.0,23001.5,23010.0,0,0],
            ["2026-10-21T09:15:00+05:30",23016.0,23063.6,22976.15,23039.2,1200,"35"],
            ["not a time",1,1,1,1,0,0],
            ["2026-10-21T09:25:00+05:30",null,1,1,1,0,0]
        ]}}"#;
        let env: Envelope<CandleData> = serde_json::from_str(body).unwrap();
        let candles = parse_candles(&env.data.candles);
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].start_ms, ist_ms(21, 9, 15));
        assert_eq!((candles[0].open, candles[0].high, candles[0].volume, candles[0].oi), (23016.0, 23063.6, 1200.0, 35.0));
        assert_eq!(candles[1].start_ms, ist_ms(21, 9, 20));
    }

    #[test]
    fn an_instrument_key_is_one_encoded_path_segment() {
        assert_eq!(path_segment("NSE_INDEX|Nifty 50"), "NSE_INDEX%7CNifty%2050");
        assert_eq!(path_segment("BSE_FO|1101628"), "BSE_FO%7C1101628");
    }

    #[test]
    fn derivatives_keep_the_wanted_futures_and_options_with_their_ist_expiry() {
        // Rows as they appear in NSE.json.gz on 2026-10-21. Expiry is the
        // epoch millis of 15:29:59 IST on the expiry day.
        let json = r#"[
            {"segment":"NSE_INDEX","instrument_type":"INDEX","instrument_key":"NSE_INDEX|Nifty 50"},
            {"segment":"NSE_FO","instrument_type":"CE","underlying_symbol":"MIDCPNIFTY","instrument_key":"NSE_FO|50918","lot_size":120,"strike_price":15250.0,"expiry":1793125799000},
            {"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"BANKNIFTY","instrument_key":"NSE_FO|48699","lot_size":30,"strike_price":0,"expiry":1793125799000},
            {"segment":"NSE_FO","instrument_type":"PE","underlying_symbol":"RELIANCE","instrument_key":"NSE_FO|7","lot_size":500,"strike_price":3000,"expiry":1793125799000},
            {"segment":"NSE_EQ","instrument_type":"EQ","instrument_key":"NSE_EQ|INE002A01018"}
        ]"#;
        let wanted = vec!["MIDCPNIFTY".to_string(), "BANKNIFTY".to_string()];
        let keep = |r: &InstrumentRow| is_derivative_of(r, &wanted);
        let mut de = serde_json::Deserializer::from_str(json);
        let rows = RowFilter { keep: &keep }.deserialize(&mut de).unwrap();
        let found: Vec<Derivative> = rows.iter().filter_map(to_derivative).collect();

        let oct27 = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        assert_eq!(found, vec![
            Derivative {
                underlying: "MIDCPNIFTY".into(),
                kind: "CE".into(),
                expiry: oct27,
                strike: 15250.0,
                instrument_key: "NSE_FO|50918".into(),
                lot_size: 120,
            },
            Derivative {
                underlying: "BANKNIFTY".into(),
                kind: "FUT".into(),
                expiry: oct27,
                strike: 0.0,
                instrument_key: "NSE_FO|48699".into(),
                lot_size: 30,
            },
        ]);
    }

    /// Rows of NSE.json.gz as they appear for a stock, gzipped as the file is.
    fn stock_instruments_gz() -> Vec<u8> {
        use std::io::Write as _;
        let json = r#"[
            {"segment":"NSE_EQ","instrument_type":"EQ","instrument_key":"NSE_EQ|INE002A01018","trading_symbol":"RELIANCE","name":"RELIANCE INDUSTRIES LTD"},
            {"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"RELIANCE","instrument_key":"NSE_FO|35001","lot_size":500,"strike_price":0,"expiry":1795478399000},
            {"segment":"NSE_FO","instrument_type":"CE","underlying_symbol":"RELIANCE","instrument_key":"NSE_FO|35002","lot_size":500,"strike_price":1400,"expiry":1795478399000},
            {"segment":"NSE_FO","instrument_type":"PE","underlying_symbol":"RELIANCE","instrument_key":"NSE_FO|35003","lot_size":500,"strike_price":1400,"expiry":1795478399000},
            {"segment":"NSE_FO","instrument_type":"CE","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|50918","lot_size":65,"strike_price":25000,"expiry":1795478399000},
            {"segment":"NSE_FO","instrument_type":"CE","underlying_symbol":"RELIANCEX","instrument_key":"NSE_FO|9","lot_size":1,"strike_price":1,"expiry":1795478399000}
        ]"#;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(json.as_bytes()).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn derivatives_of_a_stock_come_from_the_same_stream_filter() {
        let gz = stock_instruments_gz();
        let found = filter_derivatives(&gz, &["RELIANCE".to_string()]).unwrap();
        let nov24 = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        let keys: Vec<&str> = found.iter().map(|d| d.instrument_key.as_str()).collect();
        assert_eq!(keys, ["NSE_FO|35001", "NSE_FO|35002", "NSE_FO|35003"]);
        assert!(found.iter().all(|d| d.underlying == "RELIANCE" && d.expiry == nov24 && d.lot_size == 500));
        assert_eq!(found[0].kind, "FUT");
        assert_eq!((found[1].kind.as_str(), found[1].strike), ("CE", 1400.0));
        // The equity row is not a derivative; another symbol's rows are not RELIANCE's.
        assert!(filter_derivatives(&gz, &["TCS".to_string()]).unwrap().is_empty());
        // A corrupt file is an error, never an empty list taken as "nothing listed".
        assert!(filter_derivatives(b"not gzip", &["RELIANCE".to_string()]).is_err());
    }

    #[test]
    fn the_cache_path_is_one_file_under_the_days_directory() {
        let dir = Path::new("/tmp/upstox");
        let day = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        assert_eq!(instruments_cache_path(dir, day, NSE_INSTRUMENTS_URL), Path::new("/tmp/upstox/2026-11-11/NSE.json.gz"));
        assert_eq!(instruments_cache_path(dir, day, BSE_INSTRUMENTS_URL), Path::new("/tmp/upstox/2026-11-11/BSE.json.gz"));
        // A URL can never climb out of the day's directory.
        assert_eq!(instruments_cache_path(dir, day, "https://x/../../etc/passwd"), Path::new("/tmp/upstox/2026-11-11/passwd"));
        assert_eq!(instruments_cache_path(dir, day, "https://x/a%2F..%2Fb"), Path::new("/tmp/upstox/2026-11-11/a_2F.._2Fb"));
        assert_eq!(instruments_cache_path(dir, day, "https://x/.."), Path::new("/tmp/upstox/2026-11-11/instruments.json.gz"));
        assert_eq!(instruments_cache_path(dir, day, ""), Path::new("/tmp/upstox/2026-11-11/instruments.json.gz"));
    }

    #[tokio::test]
    async fn todays_cached_file_is_read_instead_of_downloaded_and_old_days_are_pruned() {
        let dir = std::env::temp_dir().join(format!("upstox-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let today = instruments_day(shared_domain::now_ist());
        // A URL reqwest refuses before opening a socket (not http/https): a
        // download attempt fails without any network, so a result proves
        // the cache answered.
        let url = "nope://upstox.test/NSE.json.gz";
        let path = instruments_cache_path(&dir, today, url);
        let old_day = dir.join("2000-01-01");
        std::fs::create_dir_all(&old_day).unwrap();
        std::fs::write(old_day.join("NSE.json.gz"), b"stale").unwrap();
        std::fs::create_dir_all(dir.join("notes")).unwrap();

        store_instruments(&dir, &path, &stock_instruments_gz(), today).unwrap();
        assert!(path.is_file());
        assert!(!old_day.exists(), "yesterday's directory is pruned when today's is written");
        assert!(dir.join("notes").is_dir(), "only day directories are pruned");

        let client = UpstoxClient::new("test-token").unwrap().with_instruments_cache(&dir);
        let found = client.derivatives(url, &["RELIANCE"]).await.unwrap();
        assert_eq!(found.len(), 3);
        // Without the cache the same call has to download, and this URL answers nothing.
        let bare = UpstoxClient::new("test-token").unwrap();
        assert!(bare.derivatives(url, &["RELIANCE"]).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ist(m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        NaiveDate::from_ymd_opt(2026, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .single()
            .unwrap()
    }

    #[test]
    fn the_instruments_day_starts_at_seven_ist() {
        let (nov11, nov12) = (NaiveDate::from_ymd_opt(2026, 11, 11).unwrap(), NaiveDate::from_ymd_opt(2026, 11, 12).unwrap());
        // Before Upstox's morning refresh, the file is the day before's.
        assert_eq!(instruments_day(ist(11, 12, 0, 5)), nov11);
        assert_eq!(instruments_day(ist(11, 12, 6, 59)), nov11);
        assert_eq!(instruments_day(ist(11, 12, 7, 0)), nov12);
        assert_eq!(instruments_day(ist(11, 12, 15, 50)), nov12);
        assert_eq!(instruments_day(ist(11, 12, 23, 59)), nov12);
        // The same instant in UTC is read in IST: 01:00 UTC is 06:30 IST.
        let utc = ist(11, 12, 6, 30).with_timezone(&FixedOffset::east_opt(0).unwrap());
        assert_eq!(instruments_day(utc), nov11);
    }

    #[tokio::test]
    async fn a_file_cached_before_the_morning_refresh_is_not_served_after_it() {
        let dir = std::env::temp_dir().join(format!("upstox-cache-morning-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A URL no download can reach (see above): a result is the cache's.
        let url = "nope://upstox.test/NSE.json.gz";
        let client = UpstoxClient::new("test-token").unwrap().with_instruments_cache(&dir);
        let count = |gz: &[u8]| filter_derivatives(gz, &["RELIANCE".to_string()]).map(|d| d.len());

        // Fetched at 02:00 on the 12th: stored as the 11th's file.
        let night = ist(11, 12, 2, 0);
        let path = instruments_cache_path(&dir, instruments_day(night), url);
        assert!(path.to_string_lossy().contains("2026-11-11"));
        store_instruments(&dir, &path, &stock_instruments_gz(), instruments_day(night)).unwrap();
        assert_eq!(client.filter_instruments_at(url, count, night).await.unwrap(), 3);
        assert_eq!(client.filter_instruments_at(url, count, ist(11, 12, 6, 59)).await.unwrap(), 3);
        // From 07:00 the 12th's file is wanted: not on disk, so a download,
        // which here cannot happen — the night's file is never served.
        assert!(client.filter_instruments_at(url, count, ist(11, 12, 7, 0)).await.is_err());
        assert!(client.filter_instruments_at(url, count, ist(11, 12, 12, 0)).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_corrupt_cached_file_is_removed_and_downloaded_again() {
        let dir = std::env::temp_dir().join(format!("upstox-cache-corrupt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let today = instruments_day(shared_domain::now_ist());
        let url = "nope://upstox.test/NSE.json.gz";
        let path = instruments_cache_path(&dir, today, url);
        let client = UpstoxClient::new("test-token").unwrap().with_instruments_cache(&dir);

        // Today's file is there but not gzip: it is not served for the day.
        // The download it falls back to cannot happen here, so the call
        // fails — toward no derivatives, never an empty list — and the bad
        // file is gone, so the next call downloads afresh.
        store_instruments(&dir, &path, b"not gzip", today).unwrap();
        assert!(path.is_file());
        assert!(client.derivatives(url, &["RELIANCE"]).await.is_err());
        assert!(!path.exists(), "the corrupt file is removed");

        // Two writes of the same file at once never share a temp name, so a
        // good copy written next is read whole.
        let a = store_instruments(&dir, &path, &stock_instruments_gz(), today);
        let b = store_instruments(&dir, &path, &stock_instruments_gz(), today);
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp file is left behind");
        assert_eq!(client.derivatives(url, &["RELIANCE"]).await.unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
