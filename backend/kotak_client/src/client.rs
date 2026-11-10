use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use shared_domain::{telemetry, ExecutionResult, OrderRequest};

use crate::{KotakCredentials, KotakError, AUTH_BASE_URL, NEO_FIN_KEY};

// ---------------------------------------------------------------------------
// Internal session state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Session {
    pub auth_token: String,
    pub sid: String,
    pub base_url: String,
    /// Market-data WebSocket endpoint for this account (`feedUrl` in the
    /// login response). `None` on a restored session — the caller keeps it.
    pub feed_url: Option<String>,
    #[allow(dead_code)]
    pub data_center: Option<String>,
}

// ---------------------------------------------------------------------------
// Private serde types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct TotpLoginPayload<'a> {
    #[serde(rename = "mobileNumber")]
    mobile_number: &'a str,
    ucc: &'a str,
    totp: &'a str,
}

#[derive(Serialize)]
struct MpinPayload<'a> {
    mpin: &'a str,
}

#[derive(Deserialize)]
struct AuthData {
    token: String,
    sid: String,
    #[serde(rename = "baseUrl", default)]
    base_url: Option<String>,
    #[serde(rename = "feedUrl", default)]
    feed_url: Option<String>,
    #[serde(rename = "dataCenter", default)]
    data_center: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LoginApiResponse {
    Success { data: AuthData },
    Error { message: String },
}

/// Raw broker response from the Place Order endpoint.
#[derive(Deserialize)]
struct KotakOrderResponse {
    stat: String,
    #[serde(rename = "stCode")]
    st_code: i32,
    #[serde(rename = "nOrdNo", default)]
    n_ord_no: Option<String>,
    #[serde(rename = "emsg", default)]
    emsg: Option<String>,
}

/// Deserialise a field Kotak may send as either a JSON string or a number.
///
/// The Positions API documents every numeric field as a string, but the Order
/// Book is inconsistent — `qty`/`fldQty`/`avgPrc` come back as numbers on some
/// responses. Normalising to `String` here keeps the parsing helpers uniform.
fn de_loose_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// Raw broker response from the Order Book endpoint (`GET /quick/user/orders`).
#[derive(Deserialize)]
struct KotakOrderBookResponse {
    stat: String,
    #[serde(rename = "stCode", default)]
    st_code: i32,
    #[serde(default)]
    data: Vec<KotakOrder>,
    #[serde(rename = "emsg", alias = "errMsg", alias = "errmsg", default)]
    emsg: Option<String>,
}

/// One row from the Kotak Order Book — the authoritative per-order status,
/// filled quantity and average fill price.
///
/// Reference: kotak-api-docs/getting-started.md §Step 4 (Check Order Status).
#[derive(Debug, Clone, Deserialize)]
pub struct KotakOrder {
    #[serde(rename = "nOrdNo", default, deserialize_with = "de_loose_string")]
    pub order_no: String,
    /// `open`, `complete`, `rejected`, `cancelled`, `trigger pending`, …
    #[serde(rename = "ordSt", default, deserialize_with = "de_loose_string")]
    pub status: String,
    #[serde(rename = "trdSym", default, deserialize_with = "de_loose_string")]
    pub trading_symbol: String,
    /// `B` / `S`.
    #[serde(rename = "trnsTp", default, deserialize_with = "de_loose_string")]
    pub transaction_type: String,
    /// `L` / `MKT` / `SL` / `SL-M`.
    #[serde(rename = "prcTp", default, deserialize_with = "de_loose_string")]
    pub order_type: String,
    #[serde(rename = "qty", default, deserialize_with = "de_loose_string")]
    pub quantity: String,
    #[serde(rename = "fldQty", default, deserialize_with = "de_loose_string")]
    pub filled_quantity: String,
    #[serde(rename = "avgPrc", default, deserialize_with = "de_loose_string")]
    pub average_price: String,
    /// Stop trigger for `SL` / `SL-M` rows.
    #[serde(rename = "trgPrc", default, deserialize_with = "de_loose_string")]
    pub trigger_price: String,
    #[serde(rename = "rejRsn", default, deserialize_with = "de_loose_string")]
    pub reject_reason: String,
    /// The limit price asked; `0` for a market order.
    #[serde(rename = "prc", default, deserialize_with = "de_loose_string")]
    pub price: String,
    /// When it was placed, as Kotak writes it (e.g. `05-Nov-2026 10:15:30`);
    /// empty when not sent. Display only.
    #[serde(rename = "ordDtTm", default, deserialize_with = "de_loose_string")]
    pub order_time: String,
    /// When Kotak last updated it (e.g. `2026/11/05 10:15:31`); empty when
    /// not sent. Display only.
    #[serde(rename = "hsUpTm", default, deserialize_with = "de_loose_string")]
    pub updated_time: String,
}

impl KotakOrder {
    fn status_lower(&self) -> String {
        self.status.trim().to_lowercase()
    }

    /// Quantity the order was placed for.
    pub fn ordered_qty(&self) -> i32 {
        KotakPosition::parse_i32(&self.quantity)
    }
    /// Quantity filled so far.
    pub fn filled_qty(&self) -> i32 {
        KotakPosition::parse_i32(&self.filled_quantity)
    }
    /// Average fill price (`0.0` until something fills).
    pub fn avg_price(&self) -> f64 {
        KotakPosition::parse_f64(&self.average_price)
    }
    /// Stop trigger price (`0.0` for order types that have none).
    pub fn trigger(&self) -> f64 {
        KotakPosition::parse_f64(&self.trigger_price)
    }
    /// The limit price asked; `0.0` for a market order.
    pub fn limit_price(&self) -> f64 {
        KotakPosition::parse_f64(&self.price)
    }
    /// `true` for a sell order.
    pub fn is_sell(&self) -> bool {
        self.transaction_type.trim().eq_ignore_ascii_case("S")
    }

    pub fn is_complete(&self) -> bool {
        self.status_lower() == "complete"
    }
    pub fn is_rejected(&self) -> bool {
        self.status_lower() == "rejected"
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self.status_lower().as_str(), "cancelled" | "canceled")
    }
    /// `true` once the broker will not act on this order again — safe to settle
    /// against `filled_qty()` without double-counting a later fill.
    pub fn is_terminal(&self) -> bool {
        self.is_complete() || self.is_rejected() || self.is_cancelled()
    }
}

/// Account limits — how much we can actually spend right now.
///
/// Reference: kotak-api-docs/limits.md.
#[derive(Debug, Clone)]
pub struct KotakLimits {
    /// `Net` — net available margin / cash. This is the spendable figure.
    pub net: f64,
    /// `MarginUsed` — already consumed.
    pub margin_used: f64,
    /// `CollateralValue` — pledged securities + cash.
    pub collateral_value: f64,
}

/// Raw broker response from the Limits endpoint (`POST /quick/user/limits`).
///
/// Note this one is **flat** — the figures sit at the top level, not under `data`.
#[derive(Deserialize)]
struct KotakLimitsResponse {
    stat: String,
    #[serde(rename = "stCode", default)]
    st_code: i32,
    #[serde(rename = "Net", default, deserialize_with = "de_loose_string")]
    net: String,
    #[serde(rename = "MarginUsed", default, deserialize_with = "de_loose_string")]
    margin_used: String,
    #[serde(rename = "CollateralValue", default, deserialize_with = "de_loose_string")]
    collateral_value: String,
    #[serde(rename = "emsg", alias = "errMsg", alias = "errmsg", default)]
    emsg: Option<String>,
}

/// Raw broker response from the Positions endpoint (`GET /quick/user/positions`).
#[derive(Deserialize)]
struct KotakPositionsResponse {
    stat: String,
    #[serde(rename = "stCode", default)]
    st_code: i32,
    #[serde(default)]
    data: Vec<KotakPosition>,
    #[serde(rename = "emsg", default)]
    emsg: Option<String>,
}

/// One row from the Kotak Positions API. Numeric fields arrive as strings and
/// are parsed lazily via the helper methods.
///
/// Reference: kotak-api-docs/positions.md
#[derive(Debug, Clone, Deserialize)]
pub struct KotakPosition {
    #[serde(rename = "trdSym", default)]
    pub trading_symbol: String,
    #[serde(rename = "sym", default)]
    pub symbol: String,
    #[serde(rename = "prod", default)]
    pub product: String,
    #[serde(rename = "exSeg", default)]
    pub exchange_segment: String,
    #[serde(rename = "flBuyQty", default)]
    pub filled_buy_qty: String,
    #[serde(rename = "flSellQty", default)]
    pub filled_sell_qty: String,
    /// Quantity bought on a prior day and carried forward (NRML positions
    /// held past one day, which is the normal case here — see `buy_qty`).
    #[serde(rename = "cfBuyQty", default)]
    pub cf_buy_qty: String,
    /// Quantity sold on a prior day and carried forward.
    #[serde(rename = "cfSellQty", default)]
    pub cf_sell_qty: String,
    #[serde(rename = "buyAmt", default)]
    pub buy_amount: String,
    #[serde(rename = "sellAmt", default)]
    pub sell_amount: String,
    #[serde(rename = "cfBuyAmt", default)]
    pub cf_buy_amount: String,
    #[serde(rename = "cfSellAmt", default)]
    pub cf_sell_amount: String,
    #[serde(rename = "optTp", default)]
    pub option_type: String,
    #[serde(rename = "expDt", default)]
    pub expiry_date: String,
}

impl KotakPosition {
    fn parse_f64(s: &str) -> f64 { s.trim().parse().unwrap_or(0.0) }
    fn parse_i32(s: &str) -> i32 { s.trim().parse().unwrap_or(0) }

    /// Total bought quantity: today's fills plus anything carried forward
    /// from a prior day. `flBuyQty` alone silently undercounts every NRML
    /// position that has been held past one trading day — which, in this
    /// account, is the common case, not the exception.
    pub fn buy_qty(&self) -> i32 {
        Self::parse_i32(&self.filled_buy_qty) + Self::parse_i32(&self.cf_buy_qty)
    }
    /// Total sold quantity: today's fills plus anything carried forward.
    pub fn sell_qty(&self) -> i32 {
        Self::parse_i32(&self.filled_sell_qty) + Self::parse_i32(&self.cf_sell_qty)
    }
    /// Net open quantity (total buys − total sells, across today and carry-forward).
    pub fn net_qty(&self) -> i32 { self.buy_qty() - self.sell_qty() }

    /// Volume-weighted average buy fill price (`0.0` if nothing was bought),
    /// across today's fills and any carried-forward quantity.
    pub fn avg_buy_price(&self) -> f64 {
        let q = self.buy_qty();
        if q > 0 {
            (Self::parse_f64(&self.buy_amount) + Self::parse_f64(&self.cf_buy_amount)) / q as f64
        } else {
            0.0
        }
    }
    /// Volume-weighted average sell fill price (`0.0` if nothing was sold),
    /// across today's fills and any carried-forward quantity.
    pub fn avg_sell_price(&self) -> f64 {
        let q = self.sell_qty();
        if q > 0 { (Self::parse_f64(&self.sell_amount) + Self::parse_f64(&self.cf_sell_amount)) / q as f64 } else { 0.0 }
    }
}

pub(crate) fn chrono_or_epoch() -> String {
    shared_domain::current_ist_timestamp_string()
}

fn find_csv_url(val: &serde_json::Value, segment: &str) -> Option<String> {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get(segment) {
                return Some(s.clone());
            }
            for v in map.values() {
                if let Some(found) = find_csv_url(v, segment) {
                    return Some(found);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                if let Some(found) = find_csv_url(v, segment) {
                    return Some(found);
                }
            }
        }
        serde_json::Value::String(s) => {
            if s.contains(&format!("{}.csv", segment))
                || s.contains(&format!("{}-", segment))
                || (s.ends_with(".csv") && s.contains(segment))
            {
                return Some(s.clone());
            }
        }
        _ => {}
    }
    None
}

/// Day's open, high and low, and the previous close, of a [`KotakQuote`]
/// (`ohlc` in the quotes response; `close` is the *previous* close).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct KotakOhlc {
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
}

/// One instrument's quote from the REST quotes API, every documented field
/// read leniently (`kotak-api-docs/quotes.md` §5 — the API sends them all as
/// strings): a field that is missing or unreadable is `None`, never an
/// error, so a response shaped a little differently from the documentation
/// still yields what it carries. `depth` is not kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct KotakQuote {
    /// `pSymbol` for a contract or an equity; an index's name.
    pub exchange_token: String,
    pub display_symbol: String,
    /// `nse_cm`, `nse_fo`, `bse_cm`, `bse_fo`, …
    pub exchange: String,
    /// `lstup_time`: Unix seconds of the last update.
    pub updated_at: Option<i64>,
    pub ltp: Option<f64>,
    pub last_traded_quantity: Option<f64>,
    /// Top bid quantity.
    pub total_buy: Option<f64>,
    /// Top offer quantity.
    pub total_sell: Option<f64>,
    /// `last_volume`, documented as "most recent trade volume".
    pub last_volume: Option<f64>,
    /// Net change from the previous close.
    pub change: Option<f64>,
    pub per_change: Option<f64>,
    /// Open interest, when the response carries one (`oi` filter, F&O).
    pub oi: Option<f64>,
    pub year_high: Option<f64>,
    pub year_low: Option<f64>,
    pub ohlc: KotakOhlc,
}

impl KotakQuote {
    /// `<exchange>|<exchange_token>` — the key the quote was asked for by.
    pub fn key(&self) -> String {
        format!("{}|{}", self.exchange, self.exchange_token)
    }

    fn from_object(map: &serde_json::Map<String, serde_json::Value>) -> Self {
        let text = |name: &str| map.get(name).map(loose_text).unwrap_or_default();
        let number = |name: &str| map.get(name).and_then(loose_f64);
        let ohlc = map.get("ohlc").and_then(|v| v.as_object());
        let ohlc_number = |name: &str| ohlc.and_then(|o| o.get(name)).and_then(loose_f64);
        KotakQuote {
            exchange_token: text("exchange_token"),
            display_symbol: text("display_symbol"),
            exchange: text("exchange"),
            updated_at: map.get("lstup_time").and_then(loose_i64),
            ltp: number("ltp"),
            last_traded_quantity: number("last_traded_quantity"),
            total_buy: number("total_buy"),
            total_sell: number("total_sell"),
            last_volume: number("last_volume"),
            change: number("change"),
            per_change: number("per_change"),
            oi: number("oi").or_else(|| number("open_interest")).or_else(|| number("openInterest")),
            year_high: number("year_high"),
            year_low: number("year_low"),
            ohlc: KotakOhlc {
                open: ohlc_number("open"),
                high: ohlc_number("high"),
                low: ohlc_number("low"),
                close: ohlc_number("close"),
            },
        }
    }
}

/// A JSON string or number as text; anything else as its JSON.
fn loose_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A finite number from a JSON number or a numeric string (`"81809.3400"`);
/// `None` for anything else, an empty string included.
fn loose_f64(v: &serde_json::Value) -> Option<f64> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

fn loose_i64(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        serde_json::Value::String(s) => {
            let s = s.trim();
            s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
        }
        _ => None,
    }
}

/// Set once the first quotes response has had its shape logged.
static QUOTE_SHAPE_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The field names of a quotes response, for the one-time log: the first
/// element's keys (its `ohlc` and `depth` objects summarised), or an error
/// object's keys.
fn quote_shape(value: &serde_json::Value) -> String {
    fn keys(map: &serde_json::Map<String, serde_json::Value>) -> String {
        let mut names: Vec<String> = map
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::Object(inner) if k == "ohlc" => format!("{k}{{{}}}", keys(inner)),
                serde_json::Value::Object(_) => format!("{k}{{…}}"),
                serde_json::Value::Array(items) => format!("{k}[{}]", items.len()),
                _ => k.clone(),
            })
            .collect();
        names.sort();
        names.join(", ")
    }
    match value {
        serde_json::Value::Array(items) => match items.first() {
            Some(serde_json::Value::Object(first)) => format!("array of {} {{{}}}", items.len(), keys(first)),
            Some(other) => format!("array of {} starting with {other}", items.len()),
            None => "empty array".to_string(),
        },
        serde_json::Value::Object(map) => format!("object {{{}}}", keys(map)),
        other => format!("{other}"),
    }
}

/// `body` up to 200 characters, for an error message.
fn snippet(body: &str) -> String {
    let short: String = body.chars().take(200).collect();
    if short.len() < body.len() { format!("{short}…") } else { short }
}

/// The quotes in a response body: an array of quote objects on success,
/// else the documented error object (`stat`, `emsg`, `stCode`), mapped to
/// [`KotakError::ApiError`] with Kotak's own code.
fn parse_quotes(body: &str, http_status: u16) -> Result<Vec<KotakQuote>, KotakError> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    if !QUOTE_SHAPE_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::info!(shape = %quote_shape(&value), "Kotak quotes: first response shape");
    }
    let items = match &value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => match map.get("data") {
            Some(serde_json::Value::Array(items)) => items,
            _ => {
                let stat = map.get("stat").map(loose_text).unwrap_or_default();
                let message = map
                    .get("emsg")
                    .or_else(|| map.get("errMsg"))
                    .or_else(|| map.get("message"))
                    .map(loose_text)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| snippet(body));
                let status_code = map.get("stCode").and_then(loose_i64).unwrap_or(http_status as i64) as i32;
                return Err(KotakError::ApiError {
                    status_code,
                    message: if stat.is_empty() { message } else { format!("{message} ({stat})") },
                });
            }
        },
        _ => {
            return Err(KotakError::ApiError {
                status_code: http_status as i32,
                message: format!("unexpected quotes response: {}", snippet(body)),
            })
        }
    };
    Ok(items.iter().filter_map(|v| v.as_object()).map(KotakQuote::from_object).collect())
}

// ---------------------------------------------------------------------------
// KotakClient
// ---------------------------------------------------------------------------

/// `RequestBuilder::send` that also records the call's latency (to response
/// headers) and HTTP outcome under `endpoint`. The response or error is passed
/// through untouched, so callers' error handling is unchanged.
trait SendTimed {
    async fn send_timed(self, endpoint: &'static str) -> reqwest::Result<reqwest::Response>;
}

impl SendTimed for reqwest::RequestBuilder {
    async fn send_timed(self, endpoint: &'static str) -> reqwest::Result<reqwest::Response> {
        let started = std::time::Instant::now();
        let res = self.send().await;
        let result = match &res {
            Ok(r) => telemetry::status_class(r.status().as_u16()),
            Err(e) if e.is_timeout() => "timeout",
            Err(_) => "transport_error",
        };
        telemetry::kotak_request(endpoint, result, started.elapsed().as_secs_f64());
        res
    }
}

/// Map an authenticated-endpoint response to [`KotakError::SessionExpired`] when
/// the broker rejects the session token / sid with HTTP 401 or 403, otherwise
/// pass the response straight through.
///
/// Every call that sends the `Auth` + `Sid` headers routes its response through
/// here, so a session that has aged out surfaces as a clean `SessionExpired`
/// (which the server's watchdog acts on by re-logging-in) instead of a
/// downstream JSON-decode failure that looks like a transient network blip.
fn check_session_status(resp: reqwest::Response) -> Result<reqwest::Response, KotakError> {
    match resp.status().as_u16() {
        status @ (401 | 403) => Err(KotakError::SessionExpired { status }),
        _ => Ok(resp),
    }
}

/// Timeout for the read-only calls (order book, positions, limits), in place
/// of the client's 30 s. Kotak answers these in ~50 ms, occasionally 1-2 s;
/// past that it has hung and will not answer at all. The position monitor
/// makes these reads inline in its tick, so a hung read used to freeze every
/// position's stop-loss watch for 30 s (90 s across the three pre-entry funds
/// checks). Order placement and cancels keep the full 30 s: giving up on a
/// send early only turns it into an ambiguous outcome.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Timeout for a quotes call. A background poller's read, never the engine's
/// tick, but a stalled one would hold that poller's whole cycle.
const QUOTES_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Async HTTP client for the Kotak Neo Trade API.
#[derive(Clone)]
pub struct KotakClient {
    pub(crate) http: Client,
    pub(crate) access_token: String,
    pub session: Option<Session>,
    /// Optional IP sent as `X-Forwarded-For` on order calls.
    pub(crate) client_ip: Option<String>,
}

impl KotakClient {
    /// Construct a new client using the static API Dashboard access token.
    pub fn new(access_token: impl Into<String>) -> Result<Self, KotakError> {
        // Without a bound, a stalled Kotak endpoint hangs this call forever —
        // fatal for the unattended 09:05 auto-login task, which never reaches
        // its next scheduling loop (and so stops firing on every subsequent
        // day) if the request it's awaiting never returns. 30s is generous
        // for a broker API call (order placement normally completes in low
        // single-digit seconds) while still bounding the worst case.
        let http = Client::builder()
            .use_rustls_tls()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            access_token: access_token.into(),
            session: None,
            client_ip: None,
        })
    }

    /// Set the `X-Forwarded-For` IP included on every order request.
    pub fn with_client_ip(mut self, ip: impl Into<String>) -> Self {
        self.client_ip = Some(ip.into());
        self
    }

    pub fn restore_session(&mut self, auth_token: String, sid: String, base_url: String) {
        self.session = Some(Session {
            auth_token,
            sid,
            base_url,
            feed_url: None,
            data_center: None,
        });
    }

    // ── Auth helpers ────────────────────────────────────────────────────── //

    async fn login_totp(&self, creds: &KotakCredentials) -> Result<AuthData, KotakError> {
        let payload = TotpLoginPayload {
            mobile_number: &creds.mobile_number,
            ucc: &creds.ucc,
            totp: &creds.totp,
        };
        let resp = self
            .http
            .post(format!("{AUTH_BASE_URL}/tradeApiLogin"))
            .header("Authorization", self.access_token.trim())
            .header("neo-fin-key", NEO_FIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send_timed("login_totp")
            .await?;
        let status = resp.status();
        let body = resp.text().await?;

        if let Ok(LoginApiResponse::Success { data }) = serde_json::from_str::<LoginApiResponse>(&body) {
            return Ok(data);
        }

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            let msg = v.get("message").and_then(|m| m.as_str())
                .or_else(|| v.get("fault").and_then(|f| f.get("message")).and_then(|m| m.as_str()))
                .or_else(|| v.get("error").and_then(|e| e.as_str()))
                .unwrap_or(&body);
            return Err(KotakError::LoginTotpFailed(format!("{msg} (HTTP {status})")));
        }

        Err(KotakError::LoginTotpFailed(format!("{body} (HTTP {status})")))
    }

    async fn validate_mpin(
        &self,
        view_token: &str,
        view_sid: &str,
        mpin: &str,
    ) -> Result<AuthData, KotakError> {
        let payload = MpinPayload { mpin };
        let resp = self
            .http
            .post(format!("{AUTH_BASE_URL}/tradeApiValidate"))
            .header("Authorization", self.access_token.trim())
            .header("neo-fin-key", NEO_FIN_KEY)
            .header("sid", view_sid)
            .header("Auth", view_token)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send_timed("login_mpin")
            .await?;
        let status = resp.status();
        let body = resp.text().await?;

        if let Ok(LoginApiResponse::Success { data }) = serde_json::from_str::<LoginApiResponse>(&body) {
            return Ok(data);
        }

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            let msg = v.get("message").and_then(|m| m.as_str())
                .or_else(|| v.get("fault").and_then(|f| f.get("message")).and_then(|m| m.as_str()))
                .or_else(|| v.get("error").and_then(|e| e.as_str()))
                .unwrap_or(&body);
            return Err(KotakError::LoginMpinFailed(format!("{msg} (HTTP {status})")));
        }

        Err(KotakError::LoginMpinFailed(format!("{body} (HTTP {status})")))
    }

    // ── Public API ──────────────────────────────────────────────────────── //

    /// Same as [`Self::login`], but derives the 6-digit TOTP from a Base32
    /// authenticator secret instead of requiring it to be typed in — see
    /// [`crate::totp::generate_totp`].
    pub async fn login_with_totp_secret(
        &mut self,
        mobile_number: impl Into<String>,
        ucc: impl Into<String>,
        mpin: impl Into<String>,
        totp_secret: &str,
    ) -> Result<(), KotakError> {
        let totp = crate::totp::generate_totp(totp_secret)?;
        self.login(KotakCredentials {
            access_token: self.access_token.clone(),
            mobile_number: mobile_number.into(),
            ucc: ucc.into(),
            totp,
            mpin: mpin.into(),
        })
        .await
    }

    /// Two-step login: TOTP → MPIN validate.  Stores the trading session.
    pub async fn login(&mut self, creds: KotakCredentials) -> Result<(), KotakError> {
        let view = self.login_totp(&creds).await?;
        let trade = self.validate_mpin(&view.token, &view.sid, &creds.mpin).await?;

        self.session = Some(Session {
            auth_token: trade.token,
            sid: trade.sid,
            base_url: trade.base_url.unwrap_or_default(),
            feed_url: trade.feed_url.filter(|u| !u.trim().is_empty()),
            data_center: trade.data_center,
        });

        Ok(())
    }

    /// Fetches the Scrip Master CSV dynamically from Kotak API
    pub async fn get_scrip_master_csv(&self, segment: &str) -> Result<String, KotakError> {
        let sess = self.session.as_ref().ok_or_else(|| KotakError::OrderRejected { status_code: 401, message: "Not logged in".into() })?;
        
        let url = format!("{}/script-details/1.0/masterscrip/file-paths", sess.base_url);
        let resp: serde_json::Value = self.http.get(&url)
            .header("Authorization", self.access_token.trim())
            .send_timed("scrip_master_paths")
            .await?
            .json()
            .await?;

        let csv_url = find_csv_url(&resp, segment)
            .ok_or_else(|| KotakError::OrderRejected { status_code: 404, message: format!("CSV URL not found for {}", segment) })?;

        let csv_text = self.http.get(&csv_url).send_timed("scrip_master_csv").await?.text().await?;
        Ok(csv_text)
    }

    /// Quotes for `keys` — each `<exchange_segment>|<pSymbol>`, or an index
    /// by its exact, case-sensitive name (`nse_cm|Nifty 50`) — via
    /// `GET {baseUrl}/script-details/1.0/quotes/neosymbol/<keys>/<filter>`,
    /// `filter` one of `all`, `ltp`, `ohlc`, `oi`, `depth`, `52W`,
    /// `circuit_limits`, `scrip_details` (`kotak-api-docs/quotes.md`). Like
    /// the scrip master, the call is authorised by the dashboard access
    /// token alone; the session supplies only the base URL, which is why no
    /// quote is possible once the session is cleared.
    ///
    /// How many keys one call may carry, and whether these calls count
    /// against the 10 requests a second the trading APIs share, are not
    /// documented: the caller batches and shrinks on a refusal. An HTTP
    /// error or a `Not_Ok` body is [`KotakError::ApiError`] with the HTTP
    /// status or Kotak's `stCode`, so 429 can be told apart from a refusal.
    pub async fn quotes(&self, keys: &[String], filter: &str) -> Result<Vec<KotakQuote>, KotakError> {
        let sess = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        // The URL crate percent-encodes the spaces of an index name; `|` and
        // `,` go as they are, as the documented example sends them.
        let url = format!("{}/script-details/1.0/quotes/neosymbol/{}/{}", sess.base_url, keys.join(","), filter);
        let resp = self
            .http
            .get(&url)
            .timeout(QUOTES_TIMEOUT)
            .header("Authorization", self.access_token.trim())
            .header(header::CONTENT_TYPE, "application/json")
            .send_timed("quotes")
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(KotakError::ApiError {
                status_code: status.as_u16() as i32,
                message: format!("HTTP {}: {}", status.as_u16(), snippet(&body)),
            });
        }
        parse_quotes(&body, status.as_u16())
    }

    /// Place a live order via `POST {baseUrl}/quick/order/rule/ms/place`.
    ///
    /// The `OrderRequest` is serialised as the `jData` URL-encoded form field.
    /// The call is timed for the admin's order timing (`orders::subscribe_orders`):
    /// a copy only, made after the answer is in, that never changes it.
    pub async fn place_live_order(
        &self,
        order: &OrderRequest,
    ) -> Result<ExecutionResult, KotakError> {
        let sent_at_ms = crate::orders::send_clock();
        let result = self.place_live_order_inner(order).await;
        crate::orders::publish_order(order, sent_at_ms, result.as_ref().map(|r| r.order_id.as_str()).map_err(|e| e.to_string()));
        result
    }

    async fn place_live_order_inner(
        &self,
        order: &OrderRequest,
    ) -> Result<ExecutionResult, KotakError> {
        let session = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;
        let j_data = serde_json::to_string(order)?;

        let mut req = self
            .http
            .post(format!("{}/quick/order/rule/ms/place", session.base_url))
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = check_session_status(
            req.form(&[("jData", j_data.as_str())]).send_timed("place_order").await?,
        )?
        .json::<KotakOrderResponse>()
        .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            return Err(KotakError::OrderRejected {
                status_code: raw.st_code,
                message: raw.emsg.unwrap_or(raw.stat),
            });
        }

        Ok(ExecutionResult {
            order_id: raw.n_ord_no.unwrap_or_default(),
            status: "COMPLETE".into(),
            gross_value: 0.0,
            brokerage: 0.0,
            stt_charge: 0.0,
            sebi_fee: 0.0,
            stamp_duty: 0.0,
            transaction_charge: 0.0,
            gst: 0.0,
            net_value: 0.0,
            timestamp: chrono_or_epoch(),
        })
    }

    /// Modify a resting order via `POST {baseUrl}/quick/order/vr/modify`.
    ///
    /// Kotak's modify payload is the place payload plus the original order
    /// number (`no`). Used to trail a stop-loss / resize a partially-filled
    /// exit leg.
    pub async fn modify_order(
        &self,
        order: &OrderRequest,
        order_no: &str,
    ) -> Result<ExecutionResult, KotakError> {
        let session = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;

        // Serialise the order, then splice in `no` (the Nest order number).
        //
        // Modify uses a different key for validity than Place: `vd`, not `rt`.
        // `OrderRequest`'s `#[serde(rename = "rt")]` is correct for Place (and
        // is what `place_live_order` above relies on), so the rename happens
        // here rather than on the shared struct. Confirmed against the Kotak
        // Postman collection and trading-apis.md's own Modify field table —
        // both use `vd`; only a stray curl example still shows `rt`.
        let mut payload = serde_json::to_value(order)?;
        if let serde_json::Value::Object(ref mut map) = payload {
            if let Some(validity) = map.remove("rt") {
                map.insert("vd".to_string(), validity);
            }
            map.insert("no".to_string(), serde_json::Value::String(order_no.to_string()));
        }
        let j_data = serde_json::to_string(&payload)?;

        let mut req = self
            .http
            .post(format!("{}/quick/order/vr/modify", session.base_url))
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = check_session_status(
            req.form(&[("jData", j_data.as_str())]).send_timed("modify_order").await?,
        )?
        .json::<KotakOrderResponse>()
        .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            return Err(KotakError::OrderRejected {
                status_code: raw.st_code,
                message: raw.emsg.unwrap_or(raw.stat),
            });
        }

        Ok(ExecutionResult {
            order_id: raw.n_ord_no.unwrap_or_else(|| order_no.to_string()),
            status: "COMPLETE".into(),
            gross_value: 0.0,
            brokerage: 0.0,
            stt_charge: 0.0,
            sebi_fee: 0.0,
            stamp_duty: 0.0,
            transaction_charge: 0.0,
            gst: 0.0,
            net_value: 0.0,
            timestamp: chrono_or_epoch(),
        })
    }

    /// Cancel a resting order via `POST {baseUrl}/quick/order/cancel`.
    ///
    /// `trading_symbol` is optional for regular orders but mandatory for AMO
    /// cancellations (we only cancel regular orders here).
    pub async fn cancel_order(
        &self,
        order_no: &str,
        trading_symbol: Option<&str>,
    ) -> Result<ExecutionResult, KotakError> {
        let session = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;

        let mut body = serde_json::json!({ "on": order_no, "am": "NO" });
        if let Some(ts) = trading_symbol {
            body["ts"] = serde_json::Value::String(ts.to_string());
        }
        let j_data = serde_json::to_string(&body)?;

        let mut req = self
            .http
            .post(format!("{}/quick/order/cancel", session.base_url))
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = check_session_status(
            req.form(&[("jData", j_data.as_str())]).send_timed("cancel_order").await?,
        )?
        .json::<KotakOrderResponse>()
        .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            return Err(KotakError::OrderRejected {
                status_code: raw.st_code,
                message: raw.emsg.unwrap_or(raw.stat),
            });
        }

        Ok(ExecutionResult {
            order_id: raw.n_ord_no.unwrap_or_else(|| order_no.to_string()),
            status: "CANCELLED".into(),
            gross_value: 0.0,
            brokerage: 0.0,
            stt_charge: 0.0,
            sebi_fee: 0.0,
            stamp_duty: 0.0,
            transaction_charge: 0.0,
            gst: 0.0,
            net_value: 0.0,
            timestamp: chrono_or_epoch(),
        })
    }

    /// Fetch the current-day positions via `GET {baseUrl}/quick/user/positions`.
    ///
    /// Source of truth for real fill quantities and average prices (used for
    /// fill detection and startup reconciliation).
    pub async fn get_positions(&self) -> Result<Vec<KotakPosition>, KotakError> {
        let session = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;

        let mut req = self
            .http
            .get(format!("{}/quick/user/positions", session.base_url))
            .timeout(READ_TIMEOUT)
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = check_session_status(req.send_timed("positions").await?)?
            .json::<KotakPositionsResponse>()
            .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            if raw.st_code == 5203 {
                return Ok(Vec::new());
            }
            return Err(KotakError::ApiError {
                status_code: raw.st_code,
                message: raw.emsg.unwrap_or(raw.stat),
            });
        }

        Ok(raw.data)
    }

    /// Fetch today's order book via `GET {baseUrl}/quick/user/orders`.
    ///
    /// Authoritative per-order status (`ordSt`), filled quantity (`fldQty`) and
    /// average fill price (`avgPrc`) — this is what the live monitor polls to
    /// settle entries, stop-losses and exits.
    ///
    /// An empty book is reported by the broker as a `Not_Ok` "no data"
    /// response rather than an empty array, so that case is mapped to `Ok(vec![])`
    /// instead of an error (the poller runs continuously and must not treat
    /// "nothing placed yet" as a failure).
    pub async fn get_order_book(&self) -> Result<Vec<KotakOrder>, KotakError> {
        let session = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;

        let mut req = self
            .http
            .get(format!("{}/quick/user/orders", session.base_url))
            .timeout(READ_TIMEOUT)
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = check_session_status(req.send_timed("order_book").await?)?
            .json::<KotakOrderBookResponse>()
            .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            let message = raw.emsg.unwrap_or(raw.stat);
            if raw.st_code == 5203 || message.to_lowercase().contains("no data") {
                return Ok(Vec::new());
            }
            return Err(KotakError::ApiError {
                status_code: raw.st_code,
                message,
            });
        }

        Ok(raw.data)
    }

    /// Fetch account limits via `POST {baseUrl}/quick/user/limits`.
    ///
    /// Consolidated across all segments/exchanges/products. Used as a pre-flight
    /// funds check before a live entry, so an order is not sent that the account
    /// cannot pay for.
    pub async fn get_limits(&self) -> Result<KotakLimits, KotakError> {
        let session = self.session.as_ref().ok_or(KotakError::NotAuthenticated)?;

        let j_data = r#"{"seg":"ALL","exch":"ALL","prod":"ALL"}"#;

        let mut req = self
            .http
            .post(format!("{}/quick/user/limits", session.base_url))
            .timeout(READ_TIMEOUT)
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = check_session_status(req.form(&[("jData", j_data)]).send_timed("limits").await?)?
            .json::<KotakLimitsResponse>()
            .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            return Err(KotakError::ApiError {
                status_code: raw.st_code,
                message: raw.emsg.unwrap_or(raw.stat),
            });
        }

        Ok(KotakLimits {
            net: KotakPosition::parse_f64(&raw.net),
            margin_used: KotakPosition::parse_f64(&raw.margin_used),
            collateral_value: KotakPosition::parse_f64(&raw.collateral_value),
        })
    }

    /// `true` if the client has a valid trading session.
    pub fn is_authenticated(&self) -> bool {
        self.session.is_some()
    }

    /// Returns `(auth_token, sid)` for the active session, or `None`.
    pub fn session_credentials(&self) -> Option<(&str, &str)> {
        self.session.as_ref().map(|s| (s.auth_token.as_str(), s.sid.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::{find_csv_url, KotakPosition, LoginApiResponse};
    use serde_json::json;

    #[test]
    fn login_response_carries_the_feed_url() {
        // The market feed is reached at the `feedUrl` the MPIN-validate
        // response hands back; older responses simply don't have the field.
        let with_feed: LoginApiResponse = serde_json::from_value(json!({
            "data": {
                "token": "t", "sid": "s",
                "baseUrl": "https://e22.kotaksecurities.com",
                "feedUrl": "https://sfeed.kotaksecurities.com/apifeed",
                "rtUrl": "https://e22.kotaksecurities.com/realtime",
            }
        })).unwrap();
        let LoginApiResponse::Success { data } = with_feed else { panic!("expected success") };
        assert_eq!(data.feed_url.as_deref(), Some("https://sfeed.kotaksecurities.com/apifeed"));

        let without_feed: LoginApiResponse = serde_json::from_value(json!({
            "data": { "token": "t", "sid": "s" }
        })).unwrap();
        let LoginApiResponse::Success { data } = without_feed else { panic!("expected success") };
        assert_eq!(data.feed_url, None);
    }

    #[test]
    fn net_qty_counts_carried_forward_quantity() {
        // MIDCPNIFTY held from a prior day, untouched today — Kotak reports
        // it entirely under cfBuyQty with flBuyQty at 0. Before this fix,
        // net_qty() read flBuyQty alone and came back 0 for a real 120-qty
        // holding, making it invisible to reconciliation.
        let carried: KotakPosition = serde_json::from_value(json!({
            "trdSym": "MIDCPNIFTY26SEP14100PE",
            "flBuyQty": "0", "flSellQty": "0",
            "cfBuyQty": "120", "cfSellQty": "0",
            "buyAmt": "0.00", "sellAmt": "0.00",
            "cfBuyAmt": "11370.00", "cfSellAmt": "0.00",
        })).unwrap();
        assert_eq!(carried.net_qty(), 120);
        assert!((carried.avg_buy_price() - 94.75).abs() < 0.01);
    }

    #[test]
    fn net_qty_combines_carry_forward_with_todays_fills() {
        // 120 carried in, all 120 sold today, then 60 bought back today —
        // real holding is 60. Before this fix this came back -60 (flBuyQty
        // 60 minus flSellQty 120), a negative "net short" that never existed.
        let mixed: KotakPosition = serde_json::from_value(json!({
            "trdSym": "BANKNIFTY26SEP55900PE",
            "flBuyQty": "60", "flSellQty": "120",
            "cfBuyQty": "120", "cfSellQty": "0",
            "buyAmt": "18079.50", "sellAmt": "43293.00",
            "cfBuyAmt": "49476.00", "cfSellAmt": "0.00",
        })).unwrap();
        assert_eq!(mixed.net_qty(), 60);
    }

    #[test]
    fn net_qty_with_no_carry_forward_matches_old_behavior() {
        let day_only: KotakPosition = serde_json::from_value(json!({
            "trdSym": "AXISBANK-EQ",
            "flBuyQty": "9", "flSellQty": "0",
            "buyAmt": "5862.90", "sellAmt": "0.00",
        })).unwrap();
        assert_eq!(day_only.net_qty(), 9);
        assert!((day_only.avg_buy_price() - 651.43).abs() < 0.01);
    }

    #[test]
    fn test_find_csv_url_standard_and_versioned() {
        let sample_response = json!({
            "data": {
                "filesPaths": [
                    "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed/nse_fo.csv",
                    "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed/bse_fo.csv",
                    "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed-v1/bse_cm-v1.csv",
                    "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed-v1/nse_cm-v1.csv"
                ],
                "baseFolder": "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod"
            }
        });

        assert_eq!(
            find_csv_url(&sample_response, "nse_fo"),
            Some("https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed/nse_fo.csv".to_string())
        );
        assert_eq!(
            find_csv_url(&sample_response, "bse_fo"),
            Some("https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed/bse_fo.csv".to_string())
        );
        assert_eq!(
            find_csv_url(&sample_response, "nse_cm"),
            Some("https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed-v1/nse_cm-v1.csv".to_string())
        );
        assert_eq!(
            find_csv_url(&sample_response, "bse_cm"),
            Some("https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/2026-07-27/transformed-v1/bse_cm-v1.csv".to_string())
        );
        assert_eq!(
            find_csv_url(&sample_response, "mcx_fo"),
            None
        );
    }

    #[test]
    fn quotes_parse_the_documented_response_with_every_field_a_string() {
        // The sample response of kotak-api-docs/quotes.md §5, plus an `oi`.
        let body = r#"[{
            "exchange_token": "SENSEX", "display_symbol": "SENSEX-IN", "exchange": "bse_cm",
            "lstup_time": "1757915078", "ltp": "81809.3400", "last_traded_quantity": "0",
            "total_buy": "0", "total_sell": "0", "last_volume": "0", "change": "-95.3600",
            "per_change": "-0.1200", "year_high": "0", "year_low": "0",
            "ohlc": { "open": "81925.5100", "high": "81998.5100", "low": "81779.8200", "close": "81904.7000" },
            "depth": { "buy": [{ "price": "0", "quantity": "0", "orders": "0" }], "sell": [] }
        }, {
            "exchange_token": "51386", "exchange": "nse_fo", "ltp": 142.5, "oi": 1234567, "lstup_time": 1757915078,
            "ohlc": { "open": "", "high": "n/a" }
        }]"#;
        let quotes = super::parse_quotes(body, 200).unwrap();
        assert_eq!(quotes.len(), 2);
        let sensex = &quotes[0];
        assert_eq!(sensex.key(), "bse_cm|SENSEX");
        assert_eq!(sensex.display_symbol, "SENSEX-IN");
        assert_eq!(sensex.updated_at, Some(1_757_915_078));
        assert_eq!(sensex.ltp, Some(81_809.34));
        assert_eq!(sensex.change, Some(-95.36));
        assert_eq!(sensex.per_change, Some(-0.12));
        assert_eq!(sensex.last_volume, Some(0.0));
        assert_eq!(sensex.oi, None);
        assert_eq!(
            sensex.ohlc,
            super::KotakOhlc { open: Some(81_925.51), high: Some(81_998.51), low: Some(81_779.82), close: Some(81_904.70) }
        );
        // Numbers as numbers, blanks and junk as None, missing fields tolerated.
        let option = &quotes[1];
        assert_eq!(option.key(), "nse_fo|51386");
        assert_eq!(option.ltp, Some(142.5));
        assert_eq!(option.oi, Some(1_234_567.0));
        assert_eq!(option.updated_at, Some(1_757_915_078));
        assert_eq!(option.ohlc, super::KotakOhlc::default());
        assert_eq!(option.change, None);
        assert!(option.display_symbol.is_empty());
        // A `data` envelope is read the same way.
        assert_eq!(super::parse_quotes(r#"{"data": [{"exchange_token": "1", "exchange": "nse_cm", "ltp": "1"}]}"#, 200).unwrap().len(), 1);
        assert!(super::parse_quotes("[]", 200).unwrap().is_empty());
    }

    #[test]
    fn quotes_errors_carry_kotaks_code_or_the_http_status() {
        let err = super::parse_quotes(r#"{"stat": "Not_Ok", "emsg": "Invalid instrument/code", "stCode": 1009}"#, 200).unwrap_err();
        match err {
            crate::KotakError::ApiError { status_code, message } => {
                assert_eq!(status_code, 1009);
                assert_eq!(message, "Invalid instrument/code (Not_Ok)");
            }
            other => panic!("expected ApiError, got {other}"),
        }
        // An error object with no code takes the HTTP status; junk is a JSON error.
        match super::parse_quotes(r#"{"message": "nope"}"#, 200).unwrap_err() {
            crate::KotakError::ApiError { status_code, message } => {
                assert_eq!(status_code, 200);
                assert_eq!(message, "nope");
            }
            other => panic!("expected ApiError, got {other}"),
        }
        assert!(matches!(super::parse_quotes("<html>", 200).unwrap_err(), crate::KotakError::Json(_)));
        match super::parse_quotes("42", 200).unwrap_err() {
            crate::KotakError::ApiError { status_code: 200, message } => assert!(message.starts_with("unexpected quotes response")),
            other => panic!("expected ApiError, got {other}"),
        }
        assert_eq!(
            super::quote_shape(&serde_json::from_str(r#"[{"ltp": "1", "ohlc": {"open": "1"}, "depth": {"buy": []}}]"#).unwrap()),
            "array of 1 {depth{…}, ltp, ohlc{open}}"
        );
    }
}
