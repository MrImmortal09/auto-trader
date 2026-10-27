//! The caller's address for rate limits. In production nginx sits in front
//! of the server on the same machine, so every peer is `127.0.0.1`, and
//! nginx appends the address it saw to `X-Forwarded-For`
//! (`$proxy_add_x_forwarded_for`). Only that **last** hop is nginx's own
//! word: everything before it, and any `X-Real-IP`, is whatever the client
//! chose to send, so a limiter keyed on those could be reset at will.
//!
//! IPv6 hosts usually hold a whole /64, so an IPv6 address is keyed by its
//! /64: otherwise each fresh address would bring fresh tries.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

/// The key the limits use for this request: the client's IPv4 address, or
/// its IPv6 /64.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> String {
    let ip = if peer.ip().is_loopback() { forwarded_last_hop(headers).unwrap_or(peer.ip()) } else { peer.ip() };
    limit_key(ip)
}

/// The last `X-Forwarded-For` hop, when it parses as an address.
fn forwarded_last_hop(headers: &HeaderMap) -> Option<IpAddr> {
    headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
}

fn limit_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
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
        // A client's own X-Real-IP is never trusted, and a last hop that isn't an address falls back to the peer.
        assert_eq!(client_ip(peer, &headers(&[("x-real-ip", "1.2.3.4")])), "127.0.0.1");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "203.0.113.9, not-an-ip")])), "127.0.0.1");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", " , ")])), "127.0.0.1");
        assert_eq!(client_ip(peer, &HeaderMap::new()), "127.0.0.1");
    }

    #[test]
    fn ipv6_is_keyed_by_its_64_so_rotating_addresses_share_one_limit() {
        let peer: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "2001:db8:abcd:12::7")])), "2001:db8:abcd:12::/64");
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "2001:db8:abcd:12:ffff:ffff:ffff:1")])), "2001:db8:abcd:12::/64");
        assert_ne!(client_ip(peer, &headers(&[("x-forwarded-for", "2001:db8:abcd:13::7")])), "2001:db8:abcd:12::/64");
        // An IPv4 address carried as IPv6 is the IPv4 address.
        assert_eq!(client_ip(peer, &headers(&[("x-forwarded-for", "::ffff:203.0.113.9")])), "203.0.113.9");
        let v6peer: SocketAddr = "[2001:db8:1:2:3:4:5:6]:5000".parse().unwrap();
        assert_eq!(client_ip(v6peer, &HeaderMap::new()), "2001:db8:1:2::/64");
    }

    #[test]
    fn a_public_peer_cannot_claim_another_address() {
        let peer: SocketAddr = "203.0.113.50:5000".parse().unwrap();
        assert_eq!(client_ip(peer, &headers(&[("x-real-ip", "1.2.3.4"), ("x-forwarded-for", "5.6.7.8")])), "203.0.113.50");
    }
}
