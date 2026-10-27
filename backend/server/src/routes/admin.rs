//! `/api/admin/users` — the allow-list. The admin adds, renames, disables
//! and removes paper users. There is one admin, the owner's own number
//! (`ADMIN_PHONE`, seeded at every start): nobody can be made an admin here,
//! and the admin's row can't be disabled or removed.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{admin_phone, otp, Identity, Role, UserRecord};
use crate::db::{self, UserRow};
use crate::AppState;

#[derive(Serialize)]
pub struct UserOut {
    pub id: i64,
    pub phone: String,
    pub name: Option<String>,
    pub role: &'static str,
    pub enabled: bool,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

impl From<UserRow> for UserOut {
    fn from(r: UserRow) -> Self {
        UserOut {
            role: Role::parse(&r.role).as_str(),
            id: r.id,
            phone: r.phone,
            name: r.name,
            enabled: r.enabled != 0,
            created_at: r.created_at,
            last_login_at: r.last_login_at,
        }
    }
}

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("admin users: {what}: {e}");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("could not {what}"))
}

/// `GET /api/admin/users` — everyone on the allow-list, oldest first.
pub async fn list_users_handler(State(state): State<AppState>) -> Response {
    match db::load_users(&state.db_pool).await {
        Ok(rows) => Json(rows.into_iter().map(UserOut::from).collect::<Vec<_>>()).into_response(),
        Err(e) => server_error("read the users", e),
    }
}

#[derive(Deserialize)]
pub struct NewUser {
    pub phone: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `user`, the default and the only role that may be added.
    #[serde(default)]
    pub role: Option<String>,
}

fn clean_name(name: Option<String>) -> Option<String> {
    name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

/// `POST /api/admin/users {phone, name?, role?}` — add a phone to the allow-list.
pub async fn create_user_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(req): Json<NewUser>,
) -> Response {
    let Some(phone) = otp::normalize_phone(&req.phone) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter a 10-digit Indian mobile number");
    };
    let role = match req.role.as_deref().map(str::trim) {
        None | Some("") | Some("user") => Role::User,
        Some("admin") => return refuse(StatusCode::FORBIDDEN, "There is one admin: the server's ADMIN_PHONE"),
        Some(other) => return refuse(StatusCode::BAD_REQUEST, format!("Unknown role {other:?}")),
    };
    if state.auth.users.by_phone(&phone).is_some() {
        return refuse(StatusCode::CONFLICT, "That number is already added");
    }
    let name = clean_name(req.name);
    let now = shared_domain::current_ist_timestamp_string();
    let id = match sqlx::query("INSERT INTO users (phone, name, role, enabled, created_at) VALUES (?, ?, ?, 1, ?)")
        .bind(&phone)
        .bind(&name)
        .bind(role.as_str())
        .bind(&now)
        .execute(&state.db_pool)
        .await
    {
        Ok(done) => done.last_insert_rowid(),
        Err(e) if e.to_string().contains("UNIQUE") => return refuse(StatusCode::CONFLICT, "That number is already added"),
        Err(e) => return server_error("add the user", e),
    };
    state.auth.users.upsert(UserRecord { id, phone: phone.clone(), name: name.clone(), role, enabled: true });
    tracing::info!(user_id = id, role = role.as_str(), by = identity.kind(), "User added to the allow-list");
    (
        StatusCode::CREATED,
        Json(UserOut { id, phone, name, role: role.as_str(), enabled: true, created_at: now, last_login_at: None }),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct UserPatch {
    /// A new name; an empty string clears it. Left out: unchanged.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Whether `record` is the owner's own seeded row.
fn is_seeded_admin(record: &UserRecord) -> bool {
    admin_phone().is_some_and(|p| p == record.phone)
}

/// `PATCH /api/admin/users/:id {name?, enabled?}`.
pub async fn patch_user_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<UserPatch>,
) -> Response {
    let Some(current) = state.auth.users.get(id) else {
        return refuse(StatusCode::NOT_FOUND, "not found");
    };
    if patch.enabled == Some(false) && is_seeded_admin(&current) {
        return refuse(StatusCode::CONFLICT, "The admin account can't be disabled");
    }
    let name = match patch.name {
        Some(n) => clean_name(Some(n)),
        None => current.name.clone(),
    };
    let enabled = patch.enabled.unwrap_or(current.enabled);
    if let Err(e) = sqlx::query("UPDATE users SET name = ?, enabled = ? WHERE id = ?")
        .bind(&name)
        .bind(enabled as i64)
        .bind(id)
        .execute(&state.db_pool)
        .await
    {
        return server_error("change the user", e);
    }
    state.auth.users.upsert(UserRecord { name, enabled, ..current });
    if !enabled {
        tracing::info!(user_id = id, "User disabled");
    }
    match db::load_user(&state.db_pool, id).await {
        Ok(Some(row)) => Json(UserOut::from(row)).into_response(),
        Ok(None) => refuse(StatusCode::NOT_FOUND, "not found"),
        Err(e) => server_error("read the user", e),
    }
}

/// `DELETE /api/admin/users/:id` — remove a phone from the allow-list.
pub async fn delete_user_handler(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let Some(current) = state.auth.users.get(id) else {
        return refuse(StatusCode::NOT_FOUND, "not found");
    };
    if is_seeded_admin(&current) {
        return refuse(StatusCode::CONFLICT, "The admin account can't be removed");
    }
    // Held until the rows are gone, so they can't open a position meanwhile.
    let Some(flat) = state.paper.hold_flat(id).await else {
        return refuse(
            StatusCode::CONFLICT,
            "This user holds open paper positions — disable them instead, or remove them once they are flat",
        );
    };
    let mut tx = match state.db_pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return server_error("remove the user", e),
    };
    // A user's paper portfolio goes with them: a later user given the same id
    // must start from nothing.
    let done = async {
        sqlx::query("DELETE FROM otp_codes WHERE phone = ?").bind(&current.phone).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM user_trades WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM user_positions WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM user_wallets WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&mut *tx).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    if let Err(e) = done {
        let _ = tx.rollback().await;
        return server_error("remove the user", e);
    }
    if let Err(e) = tx.commit().await {
        return server_error("remove the user", e);
    }
    drop(flat);
    state.paper.forget(id);
    state.auth.users.remove(id);
    tracing::info!(user_id = id, "User removed from the allow-list");
    Json(json!({ "deleted": true, "id": id })).into_response()
}
