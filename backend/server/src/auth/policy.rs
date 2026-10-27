//! Who may call what. One table, read by the middleware for every `/api/*`
//! request; anything the table doesn't name is the admin's, so a new route
//! is private until someone decides otherwise.

use axum::http::{Method, StatusCode};

use super::Identity;

/// What a route asks of its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Anyone, signed in or not.
    Public,
    /// Anyone signed in — the admin or a user: market data.
    SignedIn,
    /// The admin (`ADMIN_PHONE`) only: the owner's data, real money, the
    /// broker, the engine, the allow-list.
    Admin,
    /// A signed-in user's own paper portfolio. The admin trades on the
    /// owner's desk, so the admin is refused here too.
    PhoneUser,
}

#[derive(Clone, Copy)]
enum On {
    /// GET and HEAD.
    Read,
    /// POST, PUT, PATCH, DELETE.
    Write,
    Any,
}

struct Rule {
    on: On,
    /// An exact path, or a prefix ending in `/*` (which matches one or more
    /// further segments, never the prefix itself).
    pattern: &'static str,
    access: Access,
}

const fn rule(on: On, pattern: &'static str, access: Access) -> Rule {
    Rule { on, pattern, access }
}

/// First match wins. Anything unmatched is `Admin`: the owner's positions,
/// trades, logs, settings and connections, every order, the allow-list.
const RULES: &[Rule] = &[
    // Public: health and signing in. Nothing else is served without a sign-in.
    rule(On::Any, "/api/health", Access::Public),
    rule(On::Any, "/api/version", Access::Public),
    rule(On::Write, "/api/auth/otp/request", Access::Public),
    rule(On::Write, "/api/auth/otp/verify", Access::Public),
    rule(On::Read, "/api/auth/me", Access::Public),
    rule(On::Read, "/api/auth/session", Access::Public),
    // Market data, for anyone signed in.
    rule(On::Read, "/api/v2/expiries", Access::SignedIn),
    rule(On::Read, "/api/v2/chain", Access::SignedIn),
    rule(On::Read, "/api/v2/flags", Access::SignedIn),
    rule(On::Read, "/api/v2/candles", Access::SignedIn),
    rule(On::Read, "/api/v2/candles/stream", Access::SignedIn),
    rule(On::Read, "/api/v2/status", Access::SignedIn),
    rule(On::Read, "/api/execute/stream", Access::SignedIn),
    rule(On::Read, "/api/market/*", Access::SignedIn),
    // A user's own paper portfolio.
    rule(On::Any, "/api/v2/me/*", Access::PhoneUser),
];

fn matches(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix("/*") {
        Some(prefix) => path.len() > prefix.len() + 1 && path.starts_with(prefix) && path.as_bytes()[prefix.len()] == b'/',
        None => path == pattern,
    }
}

/// What `method path` under `/api/` asks of its caller. Preflight requests
/// are public (the CORS layer answers them anyway).
pub fn required(method: &Method, path: &str) -> Access {
    if *method == Method::OPTIONS {
        return Access::Public;
    }
    let read = matches!(*method, Method::GET | Method::HEAD);
    RULES
        .iter()
        .find(|r| {
            let on = match r.on {
                On::Any => true,
                On::Read => read,
                On::Write => !read,
            };
            on && matches(r.pattern, path)
        })
        .map(|r| r.access)
        .unwrap_or(Access::Admin)
}

/// Whether `identity` satisfies `access`; otherwise the status and message
/// the caller gets. 401 means "sign in", 403 means "not with that sign-in".
pub fn check(access: Access, identity: &Identity) -> Result<(), (StatusCode, &'static str)> {
    use Identity::*;
    match access {
        Access::Public => Ok(()),
        Access::SignedIn => match identity {
            Admin { .. } | User { .. } => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Sign in to continue")),
        },
        Access::Admin => match identity {
            Admin { .. } => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Sign in as the admin to do this")),
            User { .. } => Err((StatusCode::FORBIDDEN, "Only the admin can do this")),
        },
        Access::PhoneUser => match identity {
            User { .. } => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Sign in with your phone to trade on paper")),
            Admin { .. } => Err((StatusCode::FORBIDDEN, "The admin trades on the owner's desk, not a paper portfolio")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GET: Method = Method::GET;
    const POST: Method = Method::POST;
    const PUT: Method = Method::PUT;
    const PATCH: Method = Method::PATCH;
    const DELETE: Method = Method::DELETE;

    /// Every route `main.rs` registers, with what each method asks.
    const ROUTES: &[(Method, &str, Access)] = &[
        (POST, "/api/webhook/telegram", Access::Admin),
        (GET, "/api/logs/stream", Access::Admin),
        (GET, "/api/logs/history", Access::Admin),
        (GET, "/api/portfolio", Access::Admin),
        (GET, "/api/health", Access::Public),
        (GET, "/api/version", Access::Public),
        (GET, "/api/telemetry/snapshot", Access::Admin),
        (GET, "/api/telemetry/stream", Access::Admin),
        (GET, "/api/market/live", Access::SignedIn),
        (GET, "/api/market/intraday-trend", Access::SignedIn),
        (GET, "/api/market/option-chain", Access::SignedIn),
        (GET, "/api/market/indicator", Access::SignedIn),
        (GET, "/api/positions", Access::Admin),
        (GET, "/api/prices", Access::Admin),
        (GET, "/api/scrip/search", Access::Admin),
        (GET, "/api/scrip/download", Access::Admin),
        (DELETE, "/api/positions/abc-123", Access::Admin),
        (PATCH, "/api/positions/abc-123", Access::Admin),
        (POST, "/api/positions/abc-123/close", Access::Admin),
        (POST, "/api/positions/abc-123/sell", Access::Admin),
        (GET, "/api/positions/reconcile/preview", Access::Admin),
        (POST, "/api/positions/reconcile/apply", Access::Admin),
        (GET, "/api/execute/stream", Access::SignedIn),
        (POST, "/api/execute/buy", Access::Admin),
        (POST, "/api/execute/exit-all", Access::Admin),
        (GET, "/api/settings", Access::Admin),
        (POST, "/api/settings", Access::Admin),
        (POST, "/api/kill-switch", Access::Admin),
        (GET, "/api/kill-switch", Access::Admin),
        (POST, "/api/kill-switch/reset", Access::Admin),
        (POST, "/api/settings/clear_database", Access::Admin),
        (GET, "/api/wallet/balance", Access::Admin),
        (POST, "/api/wallet/balance", Access::Admin),
        (POST, "/api/update_server", Access::Admin),
        (POST, "/api/auth/kotak", Access::Admin),
        (GET, "/api/auth/kotak", Access::Admin),
        (POST, "/api/auth/kotak/auto-login", Access::Admin),
        (DELETE, "/api/auth/kotak/disconnect", Access::Admin),
        (DELETE, "/api/auth/reset", Access::Admin),
        (GET, "/api/status", Access::Admin),
        (GET, "/api/auth/kotak/scrip-master/raw", Access::Admin),
        (GET, "/api/auth/kotak/scrip-master/json", Access::Admin),
        (POST, "/api/auth/telegram/request-code", Access::Admin),
        (POST, "/api/auth/telegram/submit-code", Access::Admin),
        (POST, "/api/auth/telegram/submit-2fa", Access::Admin),
        (GET, "/api/auth/telegram/status", Access::Admin),
        (GET, "/api/auth/telegram/chats", Access::Admin),
        (POST, "/api/auth/telegram/start", Access::Admin),
        (DELETE, "/api/auth/telegram/disconnect", Access::Admin),
        (GET, "/api/auth/session", Access::Public),
        (POST, "/api/auth/otp/request", Access::Public),
        (POST, "/api/auth/otp/verify", Access::Public),
        (GET, "/api/auth/me", Access::Public),
        (GET, "/api/admin/users", Access::Admin),
        (POST, "/api/admin/users", Access::Admin),
        (PATCH, "/api/admin/users/3", Access::Admin),
        (DELETE, "/api/admin/users/3", Access::Admin),
        (GET, "/api/admin/credentials", Access::Admin),
        (PUT, "/api/admin/credentials", Access::Admin),
        (GET, "/api/v2/flags", Access::SignedIn),
        (PUT, "/api/v2/flags", Access::Admin),
        (PATCH, "/api/v2/positions/abc-123", Access::Admin),
        (GET, "/api/v2/history", Access::Admin),
        (GET, "/api/v2/desk", Access::Admin),
        (GET, "/api/v2/feed", Access::Admin),
        (GET, "/api/v2/status", Access::SignedIn),
        (GET, "/api/v2/expiries", Access::SignedIn),
        (GET, "/api/v2/chain", Access::SignedIn),
        (GET, "/api/v2/marks", Access::Admin),
        (GET, "/api/v2/system", Access::Admin),
        (GET, "/api/v2/mode-check", Access::Admin),
        (GET, "/api/v2/candles", Access::SignedIn),
        (GET, "/api/v2/candles/stream", Access::SignedIn),
        (GET, "/api/v2/me/desk", Access::PhoneUser),
        (POST, "/api/v2/me/buy", Access::PhoneUser),
        (PATCH, "/api/v2/me/positions/abc", Access::PhoneUser),
    ];

    #[test]
    fn every_route_asks_what_the_plan_says() {
        for (method, path, want) in ROUTES {
            assert_eq!(required(method, path), *want, "{method} {path}");
        }
    }

    #[test]
    fn anything_unlisted_is_the_admins() {
        assert_eq!(required(&GET, "/api/zzz"), Access::Admin);
        assert_eq!(required(&POST, "/api/v2/anything-new"), Access::Admin);
        assert_eq!(required(&GET, "/api/positionsx"), Access::Admin);
        // A prefix rule never matches the bare prefix or an unrelated sibling.
        assert_eq!(required(&GET, "/api/market"), Access::Admin);
        assert_eq!(required(&GET, "/api/v2/me"), Access::Admin);
        assert_eq!(required(&Method::OPTIONS, "/api/execute/buy"), Access::Public);
    }

    #[test]
    fn each_identity_gets_the_right_answer() {
        let admin = Identity::Admin { user_id: 1 };
        let user = Identity::User { user_id: 2 };
        let anon = Identity::Anonymous;
        for id in [&admin, &user, &anon] {
            assert!(check(Access::Public, id).is_ok());
        }
        assert!(check(Access::SignedIn, &admin).is_ok());
        assert!(check(Access::SignedIn, &user).is_ok());
        assert_eq!(check(Access::SignedIn, &anon).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert!(check(Access::Admin, &admin).is_ok());
        assert_eq!(check(Access::Admin, &user).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::Admin, &anon).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert!(check(Access::PhoneUser, &user).is_ok());
        assert_eq!(check(Access::PhoneUser, &admin).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::PhoneUser, &anon).unwrap_err().0, StatusCode::UNAUTHORIZED);
    }
}
