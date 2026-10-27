//! Who is calling, and what they may reach.
//!
//! Three ways in: the owner's **passkey** (the token `routes::auth_passkey`
//! mints — full power, as always), and a **phone** sign-in by WhatsApp OTP
//! that is either the **admin** (the owner's own number: sees everything,
//! but real-money writes still need the passkey) or a **user** (their own
//! paper portfolio and nothing of the owner's). Nobody signed in is
//! **anonymous** and may read market data.
//!
//! `middleware` works out the [`Identity`] of every `/api/*` request, puts
//! it into the request's extensions, and refuses the request if
//! [`policy::required`] asks for more.

pub mod ip;
pub mod otp;
pub mod policy;
pub mod ratelimit;
pub mod token;
pub mod whatsapp;

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use serde_json::json;

pub use policy::Access;

/// The owner's phone (`ADMIN_PHONE`): seeded as an enabled `admin` on every
/// start and never disabled or removed. `None` when it isn't set or isn't a
/// mobile number, in which case nobody is seeded.
pub fn admin_phone() -> Option<String> {
    let raw = std::env::var("ADMIN_PHONE").ok().filter(|v| !v.trim().is_empty())?;
    let phone = otp::normalize_phone(&raw);
    if phone.is_none() {
        tracing::warn!("ADMIN_PHONE is not a 10-digit Indian mobile — no admin seeded");
    }
    phone
}

/// Who a request comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Anonymous,
    /// The passkey token.
    Owner,
    /// The admin's phone token.
    Admin { user_id: i64 },
    /// A user's phone token.
    User { user_id: i64 },
}

impl Identity {
    pub fn is_owner(&self) -> bool {
        matches!(self, Identity::Owner)
    }

    /// May see the owner's positions, trades, logs, settings and connections.
    pub fn can_read_owner_data(&self) -> bool {
        matches!(self, Identity::Owner | Identity::Admin { .. })
    }

    /// The phone sign-in's user id, for the admin and users.
    pub fn user_id(&self) -> Option<i64> {
        match self {
            Identity::Admin { user_id } | Identity::User { user_id } => Some(*user_id),
            _ => None,
        }
    }

    /// `owner`, `admin`, `user` or `anonymous`.
    pub fn kind(&self) -> &'static str {
        match self {
            Identity::Anonymous => "anonymous",
            Identity::Owner => "owner",
            Identity::Admin { .. } => "admin",
            Identity::User { .. } => "user",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }

    /// `admin` is admin; anything else is a user.
    pub fn parse(s: &str) -> Role {
        if s.eq_ignore_ascii_case("admin") { Role::Admin } else { Role::User }
    }

    pub fn token_ttl_secs(self) -> u64 {
        match self {
            Role::Admin => token::ADMIN_TTL_SECS,
            Role::User => token::USER_TTL_SECS,
        }
    }
}

/// A row of `users`, as the directory keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    pub id: i64,
    pub phone: String,
    pub name: Option<String>,
    pub role: Role,
    pub enabled: bool,
}

/// Every allow-listed phone, in memory: read on each request, changed by the
/// admin routes. A disabled or deleted user is nobody on their next request.
#[derive(Default)]
pub struct UserDirectory {
    by_id: DashMap<i64, UserRecord>,
}

impl UserDirectory {
    pub fn from_records(records: impl IntoIterator<Item = UserRecord>) -> Self {
        let dir = UserDirectory::default();
        for r in records {
            dir.upsert(r);
        }
        dir
    }

    pub fn upsert(&self, record: UserRecord) {
        self.by_id.insert(record.id, record);
    }

    pub fn remove(&self, id: i64) {
        self.by_id.remove(&id);
    }

    pub fn get(&self, id: i64) -> Option<UserRecord> {
        self.by_id.get(&id).map(|r| r.clone())
    }

    pub fn by_phone(&self, phone: &str) -> Option<UserRecord> {
        self.by_id.iter().find(|r| r.phone == phone).map(|r| r.clone())
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

/// What the middleware needs — small, so router tests can build one alone.
#[derive(Clone)]
pub struct AuthState {
    /// `AUTH_SECRET`; `None` keeps today's behaviour (every signed-in route 500s).
    pub secret: Option<Arc<str>>,
    pub users: Arc<UserDirectory>,
}

/// Whether `identity` still holds: a phone sign-in's user must still be on
/// the allow-list, enabled, and of the role the sign-in was made as. Long
/// streams ask this on every tick, so a user disabled by the admin loses
/// them at once, not only on the next request.
pub fn still_signed_in(auth: &AuthState, identity: &Identity) -> bool {
    match identity {
        Identity::Anonymous | Identity::Owner => true,
        Identity::Admin { user_id } => auth.users.get(*user_id).is_some_and(|u| u.enabled && u.role == Role::Admin),
        Identity::User { user_id } => auth.users.get(*user_id).is_some_and(|u| u.enabled && u.role == Role::User),
    }
}

/// Who `token` is right now.
pub fn identify(auth: &AuthState, token: &str) -> Result<Identity, &'static str> {
    let secret = auth.secret.as_deref().ok_or("AUTH_SECRET not configured")?;
    let payload = token::verify_token(token, secret)?;
    if payload.is_owner() {
        return Ok(Identity::Owner);
    }
    let user_id = payload.user_id().ok_or("Unknown token subject")?;
    Ok(match auth.users.get(user_id) {
        Some(u) if u.enabled => match u.role {
            Role::Admin => Identity::Admin { user_id },
            Role::User => Identity::User { user_id },
        },
        // Disabled or deleted since the token was issued: signed out.
        _ => Identity::Anonymous,
    })
}

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// `?token=` — the only way an `EventSource` can send one. Tokens are
/// base64url and dots, so no percent-decoding is needed.
fn query_token(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Every `/api/*` request passes through here.
pub async fn middleware(State(auth): State<AuthState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api/") {
        return next.run(req).await;
    }
    let access = policy::required(req.method(), path);
    let is_read = matches!(*req.method(), Method::GET | Method::HEAD);
    let token = bearer(&req).or_else(|| if is_read { query_token(req.uri().query()) } else { None });

    let identity = match token {
        None => Identity::Anonymous,
        Some(token) => match identify(&auth, &token) {
            Ok(identity) => identity,
            // A stale token must not break public pages; anywhere else it is refused.
            Err(_) if access == Access::Public => Identity::Anonymous,
            Err("AUTH_SECRET not configured") => {
                tracing::error!("AUTH_SECRET not configured");
                return refuse(StatusCode::INTERNAL_SERVER_ERROR, "AUTH_SECRET not configured");
            }
            Err(msg) => return refuse(StatusCode::UNAUTHORIZED, msg),
        },
    };

    if let Err((status, message)) = policy::check(access, &identity) {
        return refuse(status, message);
    }
    req.extensions_mut().insert(identity);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::Request as HttpRequest,
        middleware::from_fn_with_state,
        routing::{get, post},
        Extension, Router,
    };
    use tower::ServiceExt;

    const SECRET: &str = "test-secret";

    fn auth() -> AuthState {
        let users = UserDirectory::from_records([
            UserRecord { id: 1, phone: "7000000001".into(), name: Some("Admin".into()), role: Role::Admin, enabled: true },
            UserRecord { id: 2, phone: "9876543210".into(), name: None, role: Role::User, enabled: true },
            UserRecord { id: 3, phone: "9876543211".into(), name: None, role: Role::User, enabled: false },
        ]);
        AuthState { secret: Some(Arc::from(SECRET)), users: Arc::new(users) }
    }

    fn phone_token(user_id: i64, role: &str) -> String {
        let now = shared_domain::now_ist().timestamp() as u64;
        token::sign(&token::TokenPayload::phone(user_id, role, now, 3600), SECRET)
    }

    fn owner_token() -> String {
        token::sign(&token::TokenPayload::owner(shared_domain::now_ist().timestamp() as u64), SECRET)
    }

    async fn who(Extension(identity): Extension<Identity>) -> String {
        identity.kind().to_string()
    }

    fn app(auth: AuthState) -> Router {
        Router::new()
            .route("/api/health", get(who))
            .route("/api/v2/chain", get(who))
            .route("/api/positions", get(who))
            .route("/api/execute/stream", get(who))
            .route("/api/execute/buy", post(who))
            .route("/api/v2/me/buy", post(who))
            .route("/api/admin/users", post(who))
            .route("/static.js", get(|| async { "js" }))
            .layer(from_fn_with_state(auth, middleware))
    }

    async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut req = HttpRequest::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    #[test]
    fn identify_reads_the_directory_not_the_token() {
        let auth = auth();
        assert_eq!(identify(&auth, &owner_token()), Ok(Identity::Owner));
        assert_eq!(identify(&auth, &phone_token(1, "admin")), Ok(Identity::Admin { user_id: 1 }));
        assert_eq!(identify(&auth, &phone_token(2, "user")), Ok(Identity::User { user_id: 2 }));
        // A token claiming admin for a user row is a user; a disabled or unknown row is nobody.
        assert_eq!(identify(&auth, &phone_token(2, "admin")), Ok(Identity::User { user_id: 2 }));
        assert_eq!(identify(&auth, &phone_token(3, "user")), Ok(Identity::Anonymous));
        assert_eq!(identify(&auth, &phone_token(99, "user")), Ok(Identity::Anonymous));
        assert_eq!(identify(&auth, "garbage"), Err("Malformed token"));
        let no_secret = AuthState { secret: None, users: auth.users.clone() };
        assert_eq!(identify(&no_secret, &owner_token()), Err("AUTH_SECRET not configured"));
    }

    #[test]
    fn a_stream_ends_when_its_user_is_disabled_or_demoted() {
        let auth = auth();
        assert!(still_signed_in(&auth, &Identity::Owner));
        assert!(still_signed_in(&auth, &Identity::Anonymous));
        assert!(still_signed_in(&auth, &Identity::Admin { user_id: 1 }));
        assert!(still_signed_in(&auth, &Identity::User { user_id: 2 }));
        assert!(!still_signed_in(&auth, &Identity::User { user_id: 3 }), "disabled");
        assert!(!still_signed_in(&auth, &Identity::Admin { user_id: 2 }), "a user's token claiming admin");
        assert!(!still_signed_in(&auth, &Identity::User { user_id: 99 }), "deleted");
        auth.users.upsert(UserRecord { id: 1, phone: "7000000001".into(), name: None, role: Role::Admin, enabled: false });
        assert!(!still_signed_in(&auth, &Identity::Admin { user_id: 1 }));
    }

    #[tokio::test]
    async fn the_middleware_lets_each_identity_through_its_own_doors() {
        let app = app(auth());
        let (owner, admin, user) = (owner_token(), phone_token(1, "admin"), phone_token(2, "user"));

        // Public: everyone, and the handler learns who it is.
        assert_eq!(call(&app, "GET", "/api/health", None).await, (StatusCode::OK, "anonymous".into()));
        assert_eq!(call(&app, "GET", "/api/v2/chain", Some(&user)).await, (StatusCode::OK, "user".into()));
        assert_eq!(call(&app, "GET", "/api/execute/stream", Some(&admin)).await, (StatusCode::OK, "admin".into()));
        // Static files are outside the policy.
        assert_eq!(call(&app, "GET", "/static.js", None).await.0, StatusCode::OK);

        // The owner's data.
        assert_eq!(call(&app, "GET", "/api/positions", Some(&owner)).await, (StatusCode::OK, "owner".into()));
        assert_eq!(call(&app, "GET", "/api/positions", Some(&admin)).await, (StatusCode::OK, "admin".into()));
        assert_eq!(call(&app, "GET", "/api/positions", Some(&user)).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&app, "GET", "/api/positions", None).await.0, StatusCode::UNAUTHORIZED);

        // Real money: the passkey only.
        assert_eq!(call(&app, "POST", "/api/execute/buy", Some(&owner)).await.0, StatusCode::OK);
        assert_eq!(call(&app, "POST", "/api/execute/buy", Some(&admin)).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&app, "POST", "/api/execute/buy", Some(&user)).await.0, StatusCode::FORBIDDEN);
        let (status, body) = call(&app, "POST", "/api/execute/buy", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("Write access requires passkey authentication"));

        // A paper portfolio: users only.
        assert_eq!(call(&app, "POST", "/api/v2/me/buy", Some(&user)).await.0, StatusCode::OK);
        assert_eq!(call(&app, "POST", "/api/v2/me/buy", Some(&admin)).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&app, "POST", "/api/v2/me/buy", Some(&owner)).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&app, "POST", "/api/v2/me/buy", None).await.0, StatusCode::UNAUTHORIZED);

        // The allow-list: owner or admin.
        assert_eq!(call(&app, "POST", "/api/admin/users", Some(&admin)).await.0, StatusCode::OK);
        assert_eq!(call(&app, "POST", "/api/admin/users", Some(&user)).await.0, StatusCode::FORBIDDEN);

        // A disabled user's still-valid token is nobody.
        let disabled = phone_token(3, "user");
        assert_eq!(call(&app, "GET", "/api/health", Some(&disabled)).await, (StatusCode::OK, "anonymous".into()));
        assert_eq!(call(&app, "POST", "/api/v2/me/buy", Some(&disabled)).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_query_token_counts_on_reads_only_and_a_stale_token_is_tolerated_on_public_routes() {
        let app = app(auth());
        let admin = phone_token(1, "admin");
        assert_eq!(call(&app, "GET", &format!("/api/positions?x=1&token={admin}"), None).await, (StatusCode::OK, "admin".into()));
        assert_eq!(call(&app, "POST", &format!("/api/admin/users?token={admin}"), None).await.0, StatusCode::UNAUTHORIZED);

        let expired = token::sign(&token::TokenPayload::phone(1, "admin", 1_000, 10), SECRET);
        assert_eq!(call(&app, "GET", "/api/health", Some(&expired)).await, (StatusCode::OK, "anonymous".into()));
        let (status, body) = call(&app, "GET", "/api/positions", Some(&expired)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("Token expired"));
    }

    #[tokio::test]
    async fn without_a_secret_signed_in_routes_fail_loudly_and_public_ones_still_work() {
        let auth = AuthState { secret: None, users: Arc::new(UserDirectory::default()) };
        let app = app(auth);
        assert_eq!(call(&app, "GET", "/api/health", None).await.0, StatusCode::OK);
        assert_eq!(call(&app, "GET", "/api/health", Some("some.token.here")).await.0, StatusCode::OK);
        assert_eq!(call(&app, "POST", "/api/execute/buy", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&app, "POST", "/api/execute/buy", Some("some.token.here")).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
