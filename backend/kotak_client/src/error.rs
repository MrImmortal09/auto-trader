/// All credentials required for the two-step Kotak Neo login.
/// Reference: kotak-api-docs/authentication.md
pub struct KotakCredentials {
    /// Static API Dashboard access token (`Authorization` header).
    pub access_token: String,
    /// Registered mobile number with ISD prefix, e.g. `"+91XXXXXXXXXX"`.
    pub mobile_number: String,
    /// 5-character Unique Client Code.
    pub ucc: String,
    /// 6-digit TOTP from an authenticator app.
    pub totp: String,
    /// 6-digit trading MPIN.
    pub mpin: String,
}

/// All errors produced by `kotak_client`.
#[derive(Debug, thiserror::Error)]
pub enum KotakError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The broker returned HTTP 401/403 on an authenticated call — the session
    /// token / sid has expired or been invalidated (e.g. the access token was
    /// reset, or Kotak aged the session out). The only fix is a fresh
    /// TOTP → MPIN login. Kept distinct from [`Self::Http`] so callers can tell
    /// "the session is dead, re-login" apart from "the network hiccuped, retry".
    #[error("Kotak session expired (HTTP {status}) — re-login required")]
    SessionExpired { status: u16 },

    #[error("not authenticated — call login() first")]
    NotAuthenticated,

    #[error("TOTP login failed: {0}")]
    LoginTotpFailed(String),

    #[error("MPIN validation failed: {0}")]
    LoginMpinFailed(String),

    #[error("invalid TOTP secret: {0}")]
    TotpSecretInvalid(String),

    #[error("order rejected by broker (code {status_code}): {message}")]
    OrderRejected { status_code: i32, message: String },

    #[error("API error (code {status_code}): {message}")]
    ApiError { status_code: i32, message: String },

    #[error("WebSocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
}

impl KotakError {
    /// `true` when the error means the Kotak session is no longer valid and a
    /// fresh login is the only way forward — never something a plain retry on
    /// the same session fixes.
    pub fn is_session_expired(&self) -> bool {
        match self {
            KotakError::SessionExpired { .. } => true,
            // A "not authenticated" error means there is no session object at
            // all, which the re-login path handles identically.
            KotakError::NotAuthenticated => true,
            _ => false,
        }
    }

    /// `true` when the call failed *before* the broker gave a definitive answer
    /// — a connect failure, a timeout, or an unparseable response. After one of
    /// these the request may or may not have reached Kotak, so an order placed
    /// through it must be treated as "outcome unknown", never as "rejected".
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, KotakError::Http(_) | KotakError::Json(_))
    }

    /// Order-outcome metric label — one of `shared_domain::telemetry::ORDER_RESULTS`.
    pub fn metric_label(&self) -> &'static str {
        if self.is_ambiguous() {
            "ambiguous"
        } else if self.is_session_expired() {
            "session_expired"
        } else if matches!(self, KotakError::OrderRejected { .. }) {
            "rejected"
        } else {
            "error"
        }
    }
}
