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
///
/// `stock_charts` depends on `search` (the catalog and the quote hub run
/// only while it is on); the save goes through either way, and the log
/// says so when they are left inconsistent.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagsUpdate {
    pub index_options_only: Option<bool>,
    pub chart_stops: Option<bool>,
    pub search: Option<bool>,
    pub watchlists: Option<bool>,
    pub stock_charts: Option<bool>,
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
    if let Some(on) = update.search {
        after.search = on;
    }
    if let Some(on) = update.watchlists {
        after.watchlists = on;
    }
    if let Some(on) = update.stock_charts {
        after.stock_charts = on;
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

/// Why `flags` will not do what they say while `search` is off: the
/// instrument catalog is built, and the quote hub polled, only while it is
/// on, and `stock_charts` and `watchlists` rest on both. `None` when
/// nothing depends on it or it is on.
fn needs_search(flags: &FeatureFlags) -> Option<&'static str> {
    if flags.search {
        return None;
    }
    match (flags.stock_charts, flags.watchlists) {
        (true, true) => Some("stock_charts and watchlists are on with search off: no stock is recognised and nothing is priced until search is turned on"),
        (true, false) => Some("stock_charts is on with search off: no stock is recognised and no ladder row is priced until search is turned on"),
        (false, true) => Some("watchlists is on with search off: keys are not recognised and nothing is priced until search is turned on"),
        (false, false) => None,
    }
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
    // The catalog is built only while `search` is on, so turning it on owes
    // the build the downloads since made (nothing to do when the CSVs on
    // disk are the ones it was last built from).
    if !before.search && after.search {
        state.instruments.rebuild_in_background("search switched on");
    }

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
    if let Some(why) = needs_search(&after) {
        tracing::warn!(why, "Feature flags left inconsistent");
        send_log(&state.db_tx, &state.log_tx, "WARN", serde_json::json!({
            "event": "FLAGS_INCONSISTENT",
            "level": "WARN",
            "message": why,
            "flags": after,
        }))
        .await;
    }

    (StatusCode::OK, Json(after)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_changes_only_the_switches_it_names() {
        let before = FeatureFlags { index_options_only: false, chart_stops: true, search: false, watchlists: false, stock_charts: false };

        let (after, changes) = apply_update(before, &FlagsUpdate { index_options_only: Some(true), ..Default::default() });
        assert_eq!(after, FeatureFlags { index_options_only: true, ..before });
        assert_eq!(changes, vec![("index_options_only", true)]);

        let (after, changes) = apply_update(before, &FlagsUpdate { search: Some(true), ..Default::default() });
        assert_eq!(after, FeatureFlags { search: true, ..before });
        assert_eq!(changes, vec![("search", true)]);

        let (after, changes) = apply_update(before, &FlagsUpdate { watchlists: Some(true), ..Default::default() });
        assert_eq!(after, FeatureFlags { watchlists: true, ..before });
        assert_eq!(changes, vec![("watchlists", true)]);

        let (after, changes) = apply_update(before, &FlagsUpdate { stock_charts: Some(true), ..Default::default() });
        assert_eq!(after, FeatureFlags { stock_charts: true, ..before });
        assert_eq!(changes, vec![("stock_charts", true)]);

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
        assert_eq!(
            (update.index_options_only, update.chart_stops, update.search, update.watchlists, update.stock_charts),
            (None, Some(false), None, None, None)
        );
        let update: FlagsUpdate = serde_json::from_str(r#"{"stock_charts": true}"#).unwrap();
        assert_eq!(update.stock_charts, Some(true));
    }

    #[test]
    fn stock_charts_and_watchlists_without_search_are_called_out() {
        let off = FeatureFlags::default();
        assert_eq!(needs_search(&off), None);
        assert_eq!(needs_search(&FeatureFlags { search: true, stock_charts: true, watchlists: true, ..off }), None);
        assert!(needs_search(&FeatureFlags { stock_charts: true, ..off }).unwrap().starts_with("stock_charts is on with search off"));
        assert!(needs_search(&FeatureFlags { watchlists: true, ..off }).unwrap().starts_with("watchlists is on with search off"));
        assert!(needs_search(&FeatureFlags { stock_charts: true, watchlists: true, ..off }).unwrap().starts_with("stock_charts and watchlists"));
        // The engine's own switches never depend on search.
        assert_eq!(needs_search(&FeatureFlags { index_options_only: true, chart_stops: true, ..off }), None);
    }
}
