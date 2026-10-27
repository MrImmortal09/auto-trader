//! `/api/v2/*` — endpoints for the v2 frontend. Everything under `/api/*`
//! keeps its behaviour and response shapes; anything new, or anything whose
//! request or response has to change, lives here.

pub mod candles;
pub mod desk;
pub mod flags;
pub mod history;
pub mod me;
pub mod positions;
pub mod scalp;
pub mod system;

pub use candles::{candles_handler, candles_stream_handler};
pub use desk::{desk_handler, feed_handler, status_handler};
pub use flags::{get_flags_handler, put_flags_handler};
pub use history::history_handler;
pub use positions::{chart_index_subscriptions, patch_position_levels_handler};
pub use scalp::{chain_handler, expiries_handler, marks_handler};
pub use system::{mode_check_handler, system_handler};
