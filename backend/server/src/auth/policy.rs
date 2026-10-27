//! Who may call what. One table, read by the middleware for every `/api/*`
//! request; anything the table doesn't name needs the owner's passkey, so a
//! new route is private until someone decides otherwise.

use axum::http::{Method, StatusCode};

use super::Identity;

/// What a route asks of its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Anyone, signed in or not.
    Public,
    /// The owner (passkey) or the admin (phone): the owner's own data.
    OwnerRead,
    /// The owner or the admin: paper-only administration (the allow-list).
    AdminWrite,
    /// The owner's passkey only: real money, the broker, the engine.
    Owner,
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

/// First match wins. Anything unmatched is `Owner`.
const RULES: &[Rule] = &[
    // Public: health, signing in, and market data.
    rule(On::Any, "/api/health", Access::Public),
    rule(On::Any, "/api/version", Access::Public),
    rule(On::Write, "/api/auth/verify-passkey", Access::Public),
    rule(On::Write, "/api/auth/otp/request", Access::Public),
    rule(On::Write, "/api/auth/otp/verify", Access::Public),
    rule(On::Read, "/api/auth/me", Access::Public),
    rule(On::Read, "/api/auth/session", Access::Public),
    rule(On::Read, "/api/v2/expiries", Access::Public),
    rule(On::Read, "/api/v2/chain", Access::Public),
    rule(On::Read, "/api/v2/flags", Access::Public),
    rule(On::Read, "/api/v2/candles", Access::Public),
    rule(On::Read, "/api/v2/candles/stream", Access::Public),
    rule(On::Read, "/api/v2/status", Access::Public),
    rule(On::Read, "/api/execute/stream", Access::Public),
    rule(On::Read, "/api/market/*", Access::Public),
    // A user's own paper portfolio.
    rule(On::Any, "/api/v2/me/*", Access::PhoneUser),
    // The allow-list: the admin may add and disable paper users.
    rule(On::Write, "/api/admin/users", Access::AdminWrite),
    rule(On::Write, "/api/admin/users/*", Access::AdminWrite),
    rule(On::Read, "/api/admin/users", Access::OwnerRead),
    rule(On::Read, "/api/admin/credentials", Access::OwnerRead),
    // The owner's data: positions, trades, logs, settings, connections.
    rule(On::Read, "/api/positions", Access::OwnerRead),
    rule(On::Read, "/api/positions/*", Access::OwnerRead),
    rule(On::Read, "/api/prices", Access::OwnerRead),
    rule(On::Read, "/api/portfolio", Access::OwnerRead),
    rule(On::Read, "/api/logs/*", Access::OwnerRead),
    rule(On::Read, "/api/telemetry/*", Access::OwnerRead),
    rule(On::Read, "/api/settings", Access::OwnerRead),
    rule(On::Read, "/api/wallet/balance", Access::OwnerRead),
    rule(On::Read, "/api/kill-switch", Access::OwnerRead),
    rule(On::Read, "/api/status", Access::OwnerRead),
    rule(On::Read, "/api/auth/kotak", Access::OwnerRead),
    rule(On::Read, "/api/auth/kotak/*", Access::OwnerRead),
    rule(On::Read, "/api/scrip/*", Access::OwnerRead),
    rule(On::Read, "/api/auth/telegram/*", Access::OwnerRead),
    rule(On::Read, "/api/v2/desk", Access::OwnerRead),
    rule(On::Read, "/api/v2/feed", Access::OwnerRead),
    rule(On::Read, "/api/v2/history", Access::OwnerRead),
    rule(On::Read, "/api/v2/marks", Access::OwnerRead),
    rule(On::Read, "/api/v2/system", Access::OwnerRead),
    rule(On::Read, "/api/v2/mode-check", Access::OwnerRead),
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
        .unwrap_or(Access::Owner)
}

/// Whether `identity` satisfies `access`; otherwise the status and message
/// the caller gets. 401 means "sign in", 403 means "not with that sign-in".
pub fn check(access: Access, identity: &Identity) -> Result<(), (StatusCode, &'static str)> {
    use Identity::*;
    match access {
        Access::Public => Ok(()),
        Access::OwnerRead => match identity {
            Owner | Admin { .. } => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Sign in as the admin to see this")),
            User { .. } => Err((StatusCode::FORBIDDEN, "Not available to users")),
        },
        Access::AdminWrite => match identity {
            Owner | Admin { .. } => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Sign in as the admin to do this")),
            User { .. } => Err((StatusCode::FORBIDDEN, "Not available to users")),
        },
        Access::Owner => match identity {
            Owner => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Write access requires passkey authentication")),
            Admin { .. } | User { .. } => Err((StatusCode::FORBIDDEN, "This action needs the owner's passkey")),
        },
        Access::PhoneUser => match identity {
            User { .. } => Ok(()),
            Anonymous => Err((StatusCode::UNAUTHORIZED, "Sign in with your phone to trade on paper")),
            Admin { .. } => Err((StatusCode::FORBIDDEN, "The admin trades on the owner's desk, not a paper portfolio")),
            Owner => Err((StatusCode::FORBIDDEN, "Sign in with your phone to use a paper portfolio")),
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
        (POST, "/api/webhook/telegram", Access::Owner),
        (GET, "/api/logs/stream", Access::OwnerRead),
        (GET, "/api/logs/history", Access::OwnerRead),
        (GET, "/api/portfolio", Access::OwnerRead),
        (GET, "/api/health", Access::Public),
        (GET, "/api/version", Access::Public),
        (GET, "/api/telemetry/snapshot", Access::OwnerRead),
        (GET, "/api/telemetry/stream", Access::OwnerRead),
        (GET, "/api/market/live", Access::Public),
        (GET, "/api/market/intraday-trend", Access::Public),
        (GET, "/api/market/option-chain", Access::Public),
        (GET, "/api/market/indicator", Access::Public),
        (GET, "/api/positions", Access::OwnerRead),
        (GET, "/api/prices", Access::OwnerRead),
        (GET, "/api/scrip/search", Access::OwnerRead),
        (GET, "/api/scrip/download", Access::OwnerRead),
        (DELETE, "/api/positions/abc-123", Access::Owner),
        (PATCH, "/api/positions/abc-123", Access::Owner),
        (POST, "/api/positions/abc-123/close", Access::Owner),
        (POST, "/api/positions/abc-123/sell", Access::Owner),
        (GET, "/api/positions/reconcile/preview", Access::OwnerRead),
        (POST, "/api/positions/reconcile/apply", Access::Owner),
        (GET, "/api/execute/stream", Access::Public),
        (POST, "/api/execute/buy", Access::Owner),
        (POST, "/api/execute/exit-all", Access::Owner),
        (GET, "/api/settings", Access::OwnerRead),
        (POST, "/api/settings", Access::Owner),
        (POST, "/api/kill-switch", Access::Owner),
        (GET, "/api/kill-switch", Access::OwnerRead),
        (POST, "/api/kill-switch/reset", Access::Owner),
        (POST, "/api/settings/clear_database", Access::Owner),
        (GET, "/api/wallet/balance", Access::OwnerRead),
        (POST, "/api/wallet/balance", Access::Owner),
        (POST, "/api/update_server", Access::Owner),
        (POST, "/api/auth/kotak", Access::Owner),
        (GET, "/api/auth/kotak", Access::OwnerRead),
        (POST, "/api/auth/kotak/auto-login", Access::Owner),
        (DELETE, "/api/auth/kotak/disconnect", Access::Owner),
        (DELETE, "/api/auth/reset", Access::Owner),
        (GET, "/api/status", Access::OwnerRead),
        (GET, "/api/auth/kotak/scrip-master/raw", Access::OwnerRead),
        (GET, "/api/auth/kotak/scrip-master/json", Access::OwnerRead),
        (POST, "/api/auth/telegram/request-code", Access::Owner),
        (POST, "/api/auth/telegram/submit-code", Access::Owner),
        (POST, "/api/auth/telegram/submit-2fa", Access::Owner),
        (GET, "/api/auth/telegram/status", Access::OwnerRead),
        (GET, "/api/auth/telegram/chats", Access::OwnerRead),
        (POST, "/api/auth/telegram/start", Access::Owner),
        (DELETE, "/api/auth/telegram/disconnect", Access::Owner),
        (POST, "/api/auth/verify-passkey", Access::Public),
        (GET, "/api/auth/session", Access::Public),
        (POST, "/api/auth/otp/request", Access::Public),
        (POST, "/api/auth/otp/verify", Access::Public),
        (GET, "/api/auth/me", Access::Public),
        (GET, "/api/admin/users", Access::OwnerRead),
        (POST, "/api/admin/users", Access::AdminWrite),
        (PATCH, "/api/admin/users/3", Access::AdminWrite),
        (DELETE, "/api/admin/users/3", Access::AdminWrite),
        (GET, "/api/admin/credentials", Access::OwnerRead),
        (PUT, "/api/admin/credentials", Access::Owner),
        (GET, "/api/v2/flags", Access::Public),
        (PUT, "/api/v2/flags", Access::Owner),
        (PATCH, "/api/v2/positions/abc-123", Access::Owner),
        (GET, "/api/v2/history", Access::OwnerRead),
        (GET, "/api/v2/desk", Access::OwnerRead),
        (GET, "/api/v2/feed", Access::OwnerRead),
        (GET, "/api/v2/status", Access::Public),
        (GET, "/api/v2/expiries", Access::Public),
        (GET, "/api/v2/chain", Access::Public),
        (GET, "/api/v2/marks", Access::OwnerRead),
        (GET, "/api/v2/system", Access::OwnerRead),
        (GET, "/api/v2/mode-check", Access::OwnerRead),
        (GET, "/api/v2/candles", Access::Public),
        (GET, "/api/v2/candles/stream", Access::Public),
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
    fn anything_unlisted_needs_the_owner() {
        assert_eq!(required(&GET, "/api/zzz"), Access::Owner);
        assert_eq!(required(&POST, "/api/v2/anything-new"), Access::Owner);
        assert_eq!(required(&GET, "/api/positionsx"), Access::Owner);
        // A prefix rule never matches the bare prefix or an unrelated sibling.
        assert_eq!(required(&GET, "/api/market"), Access::Owner);
        assert_eq!(required(&GET, "/api/v2/me"), Access::Owner);
        assert_eq!(required(&Method::OPTIONS, "/api/execute/buy"), Access::Public);
    }

    #[test]
    fn each_identity_gets_the_right_answer() {
        let owner = Identity::Owner;
        let admin = Identity::Admin { user_id: 1 };
        let user = Identity::User { user_id: 2 };
        let anon = Identity::Anonymous;
        for id in [&owner, &admin, &user, &anon] {
            assert!(check(Access::Public, id).is_ok());
        }
        assert!(check(Access::OwnerRead, &owner).is_ok());
        assert!(check(Access::OwnerRead, &admin).is_ok());
        assert_eq!(check(Access::OwnerRead, &user).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::OwnerRead, &anon).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert!(check(Access::AdminWrite, &admin).is_ok());
        assert_eq!(check(Access::AdminWrite, &user).unwrap_err().0, StatusCode::FORBIDDEN);
        assert!(check(Access::Owner, &owner).is_ok());
        assert_eq!(check(Access::Owner, &admin).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::Owner, &user).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::Owner, &anon).unwrap_err(), (StatusCode::UNAUTHORIZED, "Write access requires passkey authentication"));
        assert!(check(Access::PhoneUser, &user).is_ok());
        assert_eq!(check(Access::PhoneUser, &admin).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::PhoneUser, &owner).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check(Access::PhoneUser, &anon).unwrap_err().0, StatusCode::UNAUTHORIZED);
    }
}
