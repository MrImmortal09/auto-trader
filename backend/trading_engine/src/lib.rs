//! Trading engine — stateful OMS with fee calculator.
//!
//! # Modules
//! - [`fees`]    — `FeeCalculator`, `ChargeBreakdown`
//! - [`monitor`] — `start_position_monitor` (50 ms state machine)

pub mod fees;
pub mod monitor;
pub mod scrip_master;

pub use fees::{ChargeBreakdown, FeeCalculator};
pub use monitor::{start_position_monitor, preview_reconciliation, apply_reconciliation, round_down_tick, entry_buy_window, lots_for_instrument, is_entry_cutoff_passed};
pub use scrip_master::{index_fo_segment, ChainStrike, OptionChain, ScripRecord, ScripStore};
