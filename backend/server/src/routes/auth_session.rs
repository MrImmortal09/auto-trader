use axum::{response::IntoResponse, Extension, Json};
use serde::Serialize;

use crate::auth::Identity;

#[derive(Serialize)]
pub struct SessionStatusResponse {
    pub authenticated: bool,
    /// `admin` or `user` (a phone sign-in), or `read`.
    pub role: &'static str,
}

/// `GET /api/auth/session` — whether the caller is signed in, and as what.
pub async fn session_status_handler(Extension(identity): Extension<Identity>) -> impl IntoResponse {
    let (authenticated, role) = match identity {
        Identity::Admin { .. } => (true, "admin"),
        Identity::User { .. } => (true, "user"),
        Identity::Anonymous => (false, "read"),
    };
    Json(SessionStatusResponse { authenticated, role })
}
