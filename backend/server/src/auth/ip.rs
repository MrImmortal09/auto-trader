//! The caller's address for rate limits. In production nginx sits in front
//! of the server on the same machine, so every peer is `127.0.0.1`, and
//! nginx appends the address it saw to `X-Forwarded-For`
//! (`$proxy_add_x_forwarded_for`). Only that **last** hop is nginx's own
//! word: everything before it, and any `X-Real-IP`, is whatever the client
//! chose to send, so a limiter keyed on those could be reset at will.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

pub fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> String {
    if peer.ip().is_loopback() {
        if let Some(ip) = forwarded_last_hop(headers) {
            return ip.to_string();
        }
    }
    peer.ip().to_string()
}

/// The last `X-Forwarded-For` hop, when it parses as an address.
fn forwarded_last_hop(headers: &HeaderMap) -> Option<IpAddr> {
    headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn behind_the_proxy_only_the_last_forwarded_hop_counts() {
        let peer: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        // nginx appended 203.0.113.9; the client wrote the earlier hops itself.
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "1.2.3.4, 10.0.0.1, 203.0.113.9")])), "203.0.113.9");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", " 198.51.100.2 ")])), "198.51.100.2");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "2001:db8::7")])), "2001:db8::7");
        // A client's own X-Real-IP is never trusted, and a last hop that isn't an address falls back to the peer.
        assert_eq!(client_ip(peer, &headers(&[("x-real-ip", "1.2.3.4")])), "127.0.0.1");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "203.0.113.9, not-an-ip")])), "127.0.0.1");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", " , ")])), "127.0.0.1");
        assert_eq!(client_ip(peer, &HeaderMap::new()), "127.0.0.1");
    }

    #[test]
    fn a_public_peer_cannot_claim_another_address() {
        let peer: SocketAddr = "203.0.113.50:5000".parse().unwrap();
        assert_eq!(client_ip(peer, &headers(&[("x-real-ip", "1.2.3.4"), ("x-forwarded-for", "5.6.7.8")])), "203.0.113.50");
    }
}
