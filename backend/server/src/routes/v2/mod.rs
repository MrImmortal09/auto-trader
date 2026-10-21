//! `/api/v2/*` — endpoints for the v2 frontend. Everything under `/api/*`
//! keeps its behaviour and response shapes; anything new, or anything whose
//! request or response has to change, lives here.

pub mod flags;

pub use flags::{get_flags_handler, put_flags_handler};
