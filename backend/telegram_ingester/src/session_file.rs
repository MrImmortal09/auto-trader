//! JSON-file persistence layer for `grammers_session::MemorySession`.
//!
//! Replaces `SqliteSession` to avoid the `libsql` vs `sqlx` threading-mode
//! conflict that panics when both libraries coexist in the same process.

use std::collections::HashMap;
use std::sync::Arc;

use grammers_session::storages::MemorySession;
use grammers_session::types::{DcOption, UpdatesState};
use grammers_session::SessionData;
use serde::{Deserialize, Serialize};

use crate::SESSION_FILE;

// ---------------------------------------------------------------------------
// Serialisable mirror of `SessionData`
// ---------------------------------------------------------------------------

/// Minimal serialisable representation of a Telegram session.
///
/// We only persist the fields needed to resume an authenticated session:
/// home DC, DC options (with auth keys), and the update state.
/// `PeerInfo` is intentionally omitted — it's a cache that rebuilds itself.
#[derive(Serialize, Deserialize)]
struct SessionSnapshot {
    home_dc: i32,
    dc_options: HashMap<i32, DcOptionSnapshot>,
    updates_state: UpdatesState,
}

#[derive(Serialize, Deserialize)]
struct DcOptionSnapshot {
    id: i32,
    ipv4: String,
    ipv6: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_key_hex: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load a `MemorySession` from `SESSION_FILE`, or return a default one.
pub fn load_session() -> Arc<MemorySession> {
    match std::fs::read_to_string(SESSION_FILE) {
        Ok(json) => match serde_json::from_str::<SessionSnapshot>(&json) {
            Ok(snap) => {
                tracing::info!("[telegram] Loaded session from {SESSION_FILE}");
                Arc::new(MemorySession::from(snap.into_session_data()))
            }
            Err(e) => {
                tracing::warn!("[telegram] Failed to parse {SESSION_FILE}: {e} — using fresh session");
                Arc::new(MemorySession::default())
            }
        },
        Err(_) => {
            tracing::info!("[telegram] No {SESSION_FILE} found — using fresh session");
            Arc::new(MemorySession::default())
        }
    }
}

/// Persist the current `MemorySession` state to `SESSION_FILE`.
///
/// This should be called after successful authentication so the auth keys
/// survive process restarts.
pub async fn save_session(session: &MemorySession) {
    use grammers_session::Session;

    let snapshot = {
        // Extract data from the session via the `Session` trait.
        let home_dc = match session.home_dc_id() {
            Ok(id) => id,
            Err(e) => { tracing::error!("[telegram] Cannot read home_dc_id: {e}"); return; }
        };

        let updates_state = match session.updates_state().await {
            Ok(s) => s,
            Err(e) => { tracing::error!("[telegram] Cannot read updates_state: {e}"); return; }
        };

        // We need to iterate known DCs. The default `SessionData` has DCs 1–5.
        let mut dc_options = HashMap::new();
        for dc_id in 1..=5 {
            if let Ok(Some(opt)) = session.dc_option(dc_id) {
                dc_options.insert(dc_id, DcOptionSnapshot {
                    id: opt.id,
                    ipv4: opt.ipv4.to_string(),
                    ipv6: opt.ipv6.to_string(),
                    auth_key_hex: opt.auth_key.map(|k| hex::encode(k)),
                });
            }
        }
        // Also try the home DC in case it's not in 1..=5
        if !dc_options.contains_key(&home_dc) {
            if let Ok(Some(opt)) = session.dc_option(home_dc) {
                dc_options.insert(home_dc, DcOptionSnapshot {
                    id: opt.id,
                    ipv4: opt.ipv4.to_string(),
                    ipv6: opt.ipv6.to_string(),
                    auth_key_hex: opt.auth_key.map(|k| hex::encode(k)),
                });
            }
        }

        SessionSnapshot { home_dc, dc_options, updates_state }
    };

    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(SESSION_FILE, json).await {
                tracing::error!("[telegram] Failed to write {SESSION_FILE}: {e}");
            } else {
                tracing::info!("[telegram] Session saved to {SESSION_FILE}");
            }
        }
        Err(e) => tracing::error!("[telegram] Failed to serialise session: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl SessionSnapshot {
    fn into_session_data(self) -> SessionData {
        use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

        let mut data = SessionData::default();
        data.home_dc = self.home_dc;
        data.updates_state = self.updates_state;

        for (id, snap) in self.dc_options {
            let auth_key = snap.auth_key_hex.and_then(|hex_str| {
                let bytes = hex::decode(&hex_str).ok()?;
                if bytes.len() == 256 {
                    let mut arr = [0u8; 256];
                    arr.copy_from_slice(&bytes);
                    Some(arr)
                } else {
                    None
                }
            });

            // Parse addresses, falling back to defaults if they fail.
            let ipv4: SocketAddrV4 = snap
                .ipv4
                .parse()
                .unwrap_or_else(|_| SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 443));
            let ipv6: SocketAddrV6 = snap
                .ipv6
                .parse()
                .unwrap_or_else(|_| SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 443, 0, 0));

            // Merge into existing known DC options (preserving IP if we only added auth_key).
            if let Some(existing) = data.dc_options.get_mut(&id) {
                existing.auth_key = auth_key;
            } else {
                data.dc_options.insert(id, DcOption { id, ipv4, ipv6, auth_key });
            }
        }

        data
    }
}

/// Simple hex encode/decode helpers (avoids pulling in the `hex` crate).
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, String> {
        if s.len() % 2 != 0 {
            return Err("odd-length hex string".into());
        }
        (0..s.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&s[i..i + 2], 16)
                    .map_err(|e| format!("invalid hex at offset {i}: {e}"))
            })
            .collect()
    }
}
