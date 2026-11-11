//! Quotes for anything in the catalog, during hours and after.
//!
//! The hub (`hub`) answers `get_at(key, now)` for a price-map key from the best
//! source it has: the Kotak feed's tick for a key the feed carries (read
//! from the shared price map, which only the feed task writes), else the
//! latest read of Kotak's REST quotes API (`rest`), which a poller
//! refreshes every 2 s for every key someone asked for in the last 15 s,
//! 25 keys a call, under one process-wide limit of 2 calls a second that
//! pauses entirely while the Kotak session is unhealthy — so the engine
//! keeps at least 8 of the 10 requests a second Kotak allows. While the
//! session is down during hours, Upstox's quotes stand in for equities and
//! indices, never ahead of Kotak. After the close the 15:35 sweep's rows
//! (`eod`) are served as the day's close, until the next day's quotes
//! arrive: the Kotak base URL leaves with the session at 15:40, so no REST
//! quote is possible then.
//!
//! Everything here runs behind the `search` switch (`FeatureFlags::search`),
//! read at run time: off, the poller sends nothing and the sweep does not
//! run, so flipping it needs no restart and a release with it off asks
//! Kotak for nothing.
//!
//! Nothing here writes `AppState.prices`, sends anything to the bridge, or
//! is read by the engine; a paper order never fills on a quote from here.
//! And nothing a signed-in user is answered tells which contracts the feed
//! carries — see `hub::QuoteHub::get_at`.

pub mod eod;
pub mod hub;
pub mod rest;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use shared_domain::FeatureFlags;
use tokio::sync::RwLock;

pub use hub::{HubStatus, QuoteHub};

/// What the hub's tasks work with.
pub struct Deps {
    pub kotak: Arc<tokio::sync::Mutex<Option<kotak_client::KotakClient>>>,
    /// The session watchdog's verdict: nothing is asked of Kotak while false.
    pub healthy: Arc<AtomicBool>,
    pub upstox: Option<upstox_client::UpstoxClient>,
    /// The catalog: the sweep's cash keys, and the database the close
    /// snapshot is kept in (`quote_eod` in `instruments.db`).
    pub catalog: Arc<crate::instruments::Catalog>,
    /// The v2 switches as loaded; `search` gates the poller and the sweep.
    pub flags: Arc<RwLock<FeatureFlags>>,
}

/// Puts the last close on file back in memory, then starts the poller and
/// the daily close sweep.
pub async fn spawn(hub: Arc<QuoteHub>, deps: Deps) {
    let pool = deps.catalog.pool();
    match &pool {
        Some(pool) => match eod::load_latest(pool).await {
            Ok(Some(snapshot)) => {
                tracing::info!(day = %snapshot.day, rows = snapshot.rows.len(), asked = snapshot.asked, partial = snapshot.partial(), "Close snapshot restored from quote_eod");
                hub.set_snapshot(snapshot);
            }
            Ok(None) => tracing::info!("No close snapshot on file yet — the first 15:35 sweep takes one"),
            Err(e) => tracing::error!(error = %e, "quote_eod could not be read — no close is served until the next sweep"),
        },
        None => tracing::error!("Instrument catalog database is not open — no close snapshot is kept or served"),
    }
    tokio::spawn(rest::run_poller(Arc::clone(&hub), deps.kotak, deps.healthy, deps.upstox, Arc::clone(&deps.flags)));
    tokio::spawn(eod::run_close_sweep(hub, pool, deps.catalog, deps.flags));
}
