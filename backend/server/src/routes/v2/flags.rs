//! `GET/PUT /api/v2/flags` — the v2 engine switches
//! (`shared_domain::FeatureFlags`), stored in their own `feature_flags` table.

use std::sync::LazyLock;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use shared_domain::FeatureFlags;
use tokio::sync::Mutex;

use crate::routes::auth_kotak::send_log;
use crate::AppState;

/// Serializes saves so two at once can't lose one's change. Held across the
/// DB write in place of the flags lock itself, which the Telegram loop reads
/// on every message and must never wait behind disk I/O.
static SAVE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// `PUT /api/v2/flags` body: only the switches being changed. One left out
/// keeps its value, so a stale tab can't turn off a switch it never showed;
/// an unknown name is rejected (422) rather than silently doing nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagsUpdate {
    pub index_options_only: Option<bool>,
    pub chart_stops: Option<bool>,
}

/// `before` with `update` applied, and the `(name, on)` of each switch that
/// actually changed.
fn apply_update(before: FeatureFlags, update: &FlagsUpdate) -> (FeatureFlags, Vec<(&'static str, bool)>) {
    let mut after = before;
    if let Some(on) = update.index_options_only {
        after.index_options_only = on;
    }
    if let Some(on) = update.chart_stops {
        after.chart_stops = on;
    }
    let changes = after
        .entries()
        .into_iter()
        .zip(before.entries())
        .filter(|(now, was)| now.1 != was.1)
        .map(|(now, _)| now)
        .collect();
    (after, changes)
}

/// `GET /api/v2/flags` — every switch.
pub async fn get_flags_handler(State(state): State<AppState>) -> Json<FeatureFlags> {
    Json(*state.flags.read().await)
}

/// `PUT /api/v2/flags` — change some switches. They are saved to SQLite
/// before they take effect, and a failed save changes nothing. Returns every
/// switch.
pub async fn put_flags_handler(
    State(state): State<AppState>,
    Json(update): Json<FlagsUpdate>,
) -> impl IntoResponse {
    let _saving = SAVE_LOCK.lock().await;
    let before = *state.flags.read().await;
    let (after, changes) = apply_update(before, &update);
    if changes.is_empty() {
        return (StatusCode::OK, Json(after)).into_response();
    }

    if let Err(e) = crate::db::save_feature_flags(&state.db_pool, &changes).await {
        tracing::error!("save feature_flags: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "failed to save flags — nothing changed"})),
        )
            .into_response();
    }
    *state.flags.write().await = after;

    let summary = changes
        .iter()
        .map(|(name, on)| format!("{name} {}", if *on { "on" } else { "off" }))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(%summary, "Feature flags changed");
    send_log(&state.db_tx, &state.log_tx, "INFO", serde_json::json!({
        "event": "FLAGS_UPDATED",
        "level": "INFO",
        "message": format!("Feature flags changed: {summary}"),
        "flags": after,
    }))
    .await;

    (StatusCode::OK, Json(after)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_changes_only_the_switches_it_names() {
        let before = FeatureFlags { index_options_only: false, chart_stops: true };

        let (after, changes) = apply_update(before, &FlagsUpdate { index_options_only: Some(true), ..Default::default() });
        assert_eq!(after, FeatureFlags { index_options_only: true, chart_stops: true });
        assert_eq!(changes, vec![("index_options_only", true)]);

        // Re-sending a value that is already set is not a change.
        let (after, changes) = apply_update(before, &FlagsUpdate { chart_stops: Some(true), ..Default::default() });
        assert_eq!(after, before);
        assert!(changes.is_empty());

        let (after, changes) = apply_update(before, &FlagsUpdate::default());
        assert_eq!(after, before);
        assert!(changes.is_empty());
    }

    #[test]
    fn an_unknown_switch_is_rejected() {
        assert!(serde_json::from_str::<FlagsUpdate>(r#"{"index_only": true}"#).is_err());
        let update: FlagsUpdate = serde_json::from_str(r#"{"chart_stops": false}"#).unwrap();
        assert_eq!((update.index_options_only, update.chart_stops), (None, Some(false)));
    }
}
