//! Trading engine — stateful OMS with fee calculator.
//!
//! # Modules
//! - [`fees`]    — `FeeCalculator`, `ChargeBreakdown`
//! - [`monitor`] — `start_position_monitor` (50 ms state machine)

pub mod fees;
pub mod monitor;
pub mod scrip_master;

pub use fees::{ChargeBreakdown, FeeCalculator};
pub use monitor::start_position_monitor;
pub use scrip_master::{ScripStore, ScripRecord};
