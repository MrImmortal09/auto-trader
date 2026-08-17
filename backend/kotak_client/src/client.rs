use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use shared_domain::{ExecutionResult, OrderRequest};

use crate::{KotakCredentials, KotakError, AUTH_BASE_URL, NEO_FIN_KEY};

// ---------------------------------------------------------------------------
// Internal session state
// ---------------------------------------------------------------------------

pub struct Session {
    pub auth_token: String,
    pub sid: String,
    pub base_url: String,
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
    #[serde(rename = "buyAmt", default)]
    pub buy_amount: String,
    #[serde(rename = "sellAmt", default)]
    pub sell_amount: String,
    #[serde(rename = "optTp", default)]
    pub option_type: String,
    #[serde(rename = "expDt", default)]
    pub expiry_date: String,
}

impl KotakPosition {
    fn parse_f64(s: &str) -> f64 { s.trim().parse().unwrap_or(0.0) }
    fn parse_i32(s: &str) -> i32 { s.trim().parse().unwrap_or(0) }

    /// Filled buy quantity.
    pub fn buy_qty(&self) -> i32 { Self::parse_i32(&self.filled_buy_qty) }
    /// Filled sell quantity.
    pub fn sell_qty(&self) -> i32 { Self::parse_i32(&self.filled_sell_qty) }
    /// Net open quantity (filled buys − filled sells).
    pub fn net_qty(&self) -> i32 { self.buy_qty() - self.sell_qty() }

    /// Volume-weighted average buy fill price (`0.0` if nothing was bought).
    pub fn avg_buy_price(&self) -> f64 {
        let q = self.buy_qty();
        if q > 0 { Self::parse_f64(&self.buy_amount) / q as f64 } else { 0.0 }
    }
    /// Volume-weighted average sell fill price (`0.0` if nothing was sold).
    pub fn avg_sell_price(&self) -> f64 {
        let q = self.sell_qty();
        if q > 0 { Self::parse_f64(&self.sell_amount) / q as f64 } else { 0.0 }
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

// ---------------------------------------------------------------------------
// KotakClient
// ---------------------------------------------------------------------------

/// Async HTTP client for the Kotak Neo Trade API.
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
        let http = Client::builder().use_rustls_tls().build()?;
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
            .send()
            .await?
            .json::<LoginApiResponse>()
            .await?;
        match resp {
            LoginApiResponse::Success { data } => Ok(data),
            LoginApiResponse::Error { message } => Err(KotakError::LoginTotpFailed(message)),
        }
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
            .send()
            .await?
            .json::<LoginApiResponse>()
            .await?;
        match resp {
            LoginApiResponse::Success { data } => Ok(data),
            LoginApiResponse::Error { message } => Err(KotakError::LoginMpinFailed(message)),
        }
    }

    // ── Public API ──────────────────────────────────────────────────────── //

    /// Two-step login: TOTP → MPIN validate.  Stores the trading session.
    pub async fn login(&mut self, creds: KotakCredentials) -> Result<(), KotakError> {
        let view = self.login_totp(&creds).await?;
        let trade = self.validate_mpin(&view.token, &view.sid, &creds.mpin).await?;

        self.session = Some(Session {
            auth_token: trade.token,
            sid: trade.sid,
            base_url: trade.base_url.unwrap_or_default(),
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
            .send()
            .await?
            .json()
            .await?;

        let csv_url = find_csv_url(&resp, segment)
            .ok_or_else(|| KotakError::OrderRejected { status_code: 404, message: format!("CSV URL not found for {}", segment) })?;

        let csv_text = self.http.get(&csv_url).send().await?.text().await?;
        Ok(csv_text)
    }

    /// Place a live order via `POST {baseUrl}/quick/order/rule/ms/place`.
    ///
    /// The `OrderRequest` is serialised as the `jData` URL-encoded form field.
    pub async fn place_live_order(
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

        let raw = req
            .form(&[("jData", j_data.as_str())])
            .send()
            .await?
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

        let raw = req
            .form(&[("jData", j_data.as_str())])
            .send()
            .await?
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

        let raw = req
            .form(&[("jData", j_data.as_str())])
            .send()
            .await?
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
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = req
            .send()
            .await?
            .json::<KotakPositionsResponse>()
            .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
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
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = req
            .send()
            .await?
            .json::<KotakOrderBookResponse>()
            .await?;

        if !raw.stat.eq_ignore_ascii_case("Ok") {
            let message = raw.emsg.unwrap_or(raw.stat);
            if message.to_lowercase().contains("no data") {
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
            .header("Sid", &session.sid)
            .header("Auth", &session.auth_token)
            .header("neo-fin-key", NEO_FIN_KEY);

        if let Some(ip) = &self.client_ip {
            req = req.header("X-Forwarded-For", ip);
        }

        let raw = req
            .form(&[("jData", j_data)])
            .send()
            .await?
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
    use super::find_csv_url;
    use serde_json::json;

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
}
