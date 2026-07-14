//! Kotak Neo API client — REST login & order placement + HSM WebSocket feed.
//!
//! # Modules
//! - [`error`]     — `KotakError`, `KotakCredentials`
//! - [`client`]    — `KotakClient` (REST: login, place_live_order)
//! - [`websocket`] — `start_market_data_stream` (HSM live prices)

pub mod error;
pub mod client;
pub mod websocket;

pub use error::{KotakCredentials, KotakError};
pub use client::KotakClient;
pub use websocket::start_market_data_stream;

use std::sync::Arc;
use dashmap::DashMap;

/// Thread-safe map of trading symbol → last traded price.
/// Populated in real time by [`start_market_data_stream`].
pub type LivePriceMap = Arc<DashMap<String, f64>>;

/// Base URL for the login endpoints — kotak-api-docs/authentication.md.
pub(crate) const AUTH_BASE_URL: &str = "https://mis.kotaksecurities.com/login/1.0";
/// Static header required on every Kotak API call.
pub(crate) const NEO_FIN_KEY: &str = "neotradeapi";
