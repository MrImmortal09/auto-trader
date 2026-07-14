//! Telegram MTProto ingester — userbot signal listener and regex parser.
//!
//! # Modules
//! - [`parser`]  — `parse_signal` (regex-based, options-aware)
//! - [`stream`]  — `start_ingester_loop` (stdin-driven login)
//! - [`manager`] — `TelegramManager` (web-driven step-by-step auth)
//! - [`session_file`] — JSON-file persistence for `MemorySession`

pub mod parser;
pub mod session_file;
pub mod stream;
pub mod manager;

pub use parser::parse_signal;
pub use stream::start_ingester_loop;
pub use manager::{TelegramAuthError, TelegramChat, TelegramManager};

/// Path to the JSON session file shared across all auth flows.
/// Replaces the old `session.db` (SQLite) which conflicted with `sqlx`.
pub(crate) const SESSION_FILE: &str = "session.json";
