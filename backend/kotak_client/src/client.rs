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
            if s.contains(&format!("{}.csv", segment)) {
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
            .header("Authorization", self.access_token.trim())
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

        if raw.stat != "Ok" {
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

    /// `true` if the client has a valid trading session.
    pub fn is_authenticated(&self) -> bool {
        self.session.is_some()
    }

    /// Returns `(auth_token, sid)` for the active session, or `None`.
    pub fn session_credentials(&self) -> Option<(&str, &str)> {
        self.session.as_ref().map(|s| (s.auth_token.as_str(), s.sid.as_str()))
    }
}
