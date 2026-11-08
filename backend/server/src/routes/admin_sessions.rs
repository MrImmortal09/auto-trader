//! Admin API for special trading sessions (e.g. Diwali Muhurat trading).
//!
//! * `GET /api/admin/special-sessions` — list sessions for today and later.
//! * `PUT /api/admin/special-sessions/:date` — set/update session for a date.
//! * `DELETE /api/admin/special-sessions/:date` — delete session for a date.

use std::sync::LazyLock;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{NaiveDate, Timelike};
use serde::Deserialize;
use shared_domain::{format_hhmm, parse_hhmm, SpecialSession};
use tokio::sync::Mutex;

use crate::routes::auth_kotak::send_log;
use crate::{db, AppState};

/// Serializes saves and deletes across concurrent admin calls.
static SAVE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Deserialize)]
pub struct SessionInput {
    pub start: String,
    pub end: String,
}

/// `GET /api/admin/special-sessions`
pub async fn get_sessions_handler(State(_state): State<AppState>) -> impl IntoResponse {
    let today = shared_domain::today_ist();
    let mut sessions: Vec<SpecialSession> = shared_domain::special_sessions()
        .into_iter()
        .filter(|s| s.date >= today)
        .collect();
    sessions.sort_by_key(|s| s.date);
    let active = shared_domain::in_special_session();

    Json(serde_json::json!({
        "sessions": sessions,
        "today": today.format("%Y-%m-%d").to_string(),
        "active": active,
    }))
}

/// `PUT /api/admin/special-sessions/:date`
pub async fn put_session_handler(
    State(state): State<AppState>,
    Path(date_str): Path<String>,
    Json(input): Json<SessionInput>,
) -> impl IntoResponse {
    let _saving = SAVE_LOCK.lock().await;

    let Ok(date) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid date format, expected YYYY-MM-DD"})),
        )
            .into_response();
    };

    let today = shared_domain::today_ist();
    if date < today {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Session date cannot be in the past"})),
        )
            .into_response();
    }

    let Some(start) = parse_hhmm(&input.start) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid start time, expected HH:MM (00:00–23:59)"})),
        )
            .into_response();
    };

    let Some(end) = parse_hhmm(&input.end) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid end time, expected HH:MM (00:00–23:59)"})),
        )
            .into_response();
    };

    if start >= end {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Start time must be before end time"})),
        )
            .into_response();
    }

    if end - start > 8 * 60 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Session duration cannot exceed 8 hours"})),
        )
            .into_response();
    }

    // Refuse (409) changing today's session while it is in progress
    if date == today {
        let now = shared_domain::now_ist();
        let minute = now.hour() * 60 + now.minute();
        if let Some(existing) = shared_domain::special_session_on(today) {
            if minute >= existing.start && minute < existing.end {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "A session in progress can't be changed. Use the kill switch to stop trading."
                    })),
                )
                    .into_response();
            }
        }
    }

    let session = SpecialSession { date, start, end };

    // Save to SQLite FIRST
    if let Err(e) = db::save_special_session(&state.db_pool, &session).await {
        tracing::error!("save special_session: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Failed to save special session"})),
        )
            .into_response();
    }

    // Update in-memory list
    let mut list = shared_domain::special_sessions();
    list.retain(|s| s.date != date);
    list.push(session);
    list.sort_by_key(|s| s.date);
    shared_domain::set_special_sessions(list);

    let start_str = format_hhmm(start);
    let end_str = format_hhmm(end);
    let summary = format!("{start_str}–{end_str} IST");
    tracing::info!(date = %date_str, %summary, "Special session set");

    send_log(
        &state.db_tx,
        &state.log_tx,
        "INFO",
        serde_json::json!({
            "event": "SPECIAL_SESSION_SET",
            "level": "INFO",
            "message": format!("Special session set for {date_str}: {summary}"),
            "date": date_str,
            "start": start_str,
            "end": end_str,
        }),
    )
    .await;

    (StatusCode::OK, Json(session)).into_response()
}

/// `DELETE /api/admin/special-sessions/:date`
pub async fn delete_session_handler(
    State(state): State<AppState>,
    Path(date_str): Path<String>,
) -> impl IntoResponse {
    let _saving = SAVE_LOCK.lock().await;

    let Ok(date) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid date format, expected YYYY-MM-DD"})),
        )
            .into_response();
    };

    let today = shared_domain::today_ist();
    // Refuse (409) deleting today's session while it is in progress
    if date == today {
        let now = shared_domain::now_ist();
        let minute = now.hour() * 60 + now.minute();
        if let Some(existing) = shared_domain::special_session_on(today) {
            if minute >= existing.start && minute < existing.end {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "A session in progress can't be changed. Use the kill switch to stop trading."
                    })),
                )
                    .into_response();
            }
        }
    }

    // Delete from SQLite FIRST
    let deleted = match db::delete_special_session(&state.db_pool, &date_str).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("delete special_session: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Failed to delete special session"})),
            )
                .into_response();
        }
    };

    if !deleted {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Special session not found"})),
        )
            .into_response();
    }

    // Update in-memory list
    let mut list = shared_domain::special_sessions();
    let existing = list.iter().find(|s| s.date == date).copied();
    list.retain(|s| s.date != date);
    shared_domain::set_special_sessions(list);

    tracing::info!(date = %date_str, "Special session removed");

    send_log(
        &state.db_tx,
        &state.log_tx,
        "INFO",
        serde_json::json!({
            "event": "SPECIAL_SESSION_REMOVED",
            "level": "INFO",
            "message": format!("Special session removed for {date_str}"),
            "date": date_str,
            "start": existing.map(|s| format_hhmm(s.start)),
            "end": existing.map(|s| format_hhmm(s.end)),
        }),
    )
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hhmm_valid_and_invalid() {
        assert_eq!(parse_hhmm("14:20"), Some(14 * 60 + 20));
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(23 * 60 + 59));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("14:60"), None);
        assert_eq!(parse_hhmm("14-20"), None);
        assert_eq!(parse_hhmm("1:20"), None);
        assert_eq!(parse_hhmm(""), None);
    }

    #[test]
    fn validation_rules() {
        // start < end
        assert!(parse_hhmm("14:20").unwrap() < parse_hhmm("15:30").unwrap());
        assert!(!(parse_hhmm("15:30").unwrap() < parse_hhmm("14:20").unwrap()));
        assert!(!(parse_hhmm("14:20").unwrap() < parse_hhmm("14:20").unwrap()));

        // <= 8 hours (480 min)
        let s1 = parse_hhmm("09:15").unwrap();
        let e1 = parse_hhmm("17:15").unwrap();
        assert_eq!(e1 - s1, 480);
        assert!(e1 - s1 <= 480);

        let e2 = parse_hhmm("17:16").unwrap();
        assert!(e2 - s1 > 480);
    }
}
