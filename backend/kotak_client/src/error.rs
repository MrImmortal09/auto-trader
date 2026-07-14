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

    #[error("not authenticated — call login() first")]
    NotAuthenticated,

    #[error("TOTP login failed: {0}")]
    LoginTotpFailed(String),

    #[error("MPIN validation failed: {0}")]
    LoginMpinFailed(String),

    #[error("order rejected by broker (code {status_code}): {message}")]
    OrderRejected { status_code: i32, message: String },

    #[error("WebSocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
}
