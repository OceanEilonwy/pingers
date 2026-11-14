// SPDX-License-Identifier: AGPL-3.0-only
//! Which address a gateway request came from, when a reverse proxy may stand in front.
//!
//! A connection's peer address is the truth unless that peer is a configured trusted proxy.
//! Only then is `X-Forwarded-For` read, from the right: each proxy appends the address it saw,
//! so the rightmost entry that is not itself a trusted proxy is the first address no trusted
//! party vouches for, which is the client. Entries to its left were written by the client or by
//! proxies nobody trusts, and are ignored. Without trusted proxies the header is never read, so
//! a server with nothing in front cannot be fooled by a client that sends one.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

use crate::config::AddressList;

/// The header a proxy appends the address it saw to.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The proxies whose `X-Forwarded-For` is believed (`[ohttp] trusted_proxies`). Empty by
/// default: the header is then ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    proxies: AddressList,
}

impl TrustedProxies {
    /// Exactly these proxies.
    #[must_use]
    pub fn new(addresses: Vec<IpAddr>) -> Self {
        Self {
            proxies: AddressList::new(addresses),
        }
    }

    /// The address a request on a connection from `peer` came from, or `None` when a trusted
    /// proxy forwarded it with an entry that is not an address (which no allow-list matches).
    /// A trusted proxy's own requests, without the header, come from the proxy.
    #[must_use]
    pub(crate) fn client(&self, peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
        if !self.proxies.contains(peer) {
            return Some(peer);
        }
        // Repeated header lines are one list, in order (RFC 9110 §5.3).
        let mut entries = Vec::new();
        for value in headers.get_all(FORWARDED_FOR) {
            entries.extend(value.to_str().ok()?.split(','));
        }
        let mut client = peer;
        for entry in entries.iter().rev() {
            client = forwarded_address(entry)?;
            if !self.proxies.contains(client) {
                break;
            }
        }
        Some(client)
    }
}

/// One `X-Forwarded-For` entry: an address, or an address and port as some proxies write it.
fn forwarded_address(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .or_else(|_| entry.parse::<SocketAddr>().map(|address| address.ip()))
        .ok()
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::net::{Ipv4Addr, Ipv6Addr};

    use axum::http::HeaderValue;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PROXY: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const ROUTER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
    const RELAY: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));
    const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

    fn forwarded(lines: &[&str]) -> Result<HeaderMap, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        for line in lines {
            headers.append(FORWARDED_FOR, HeaderValue::from_str(line)?);
        }
        Ok(headers)
    }

    #[test]
    fn without_trusted_proxies_the_header_is_ignored() -> TestResult {
        let none = TrustedProxies::default();
        assert_eq!(
            none.client(PROXY, &forwarded(&[&RELAY.to_string()])?),
            Some(PROXY)
        );
        // Nor is a peer that is not a trusted proxy believed.
        let trusted = TrustedProxies::new(vec![PROXY]);
        assert_eq!(
            trusted.client(CLIENT, &forwarded(&[&RELAY.to_string()])?),
            Some(CLIENT)
        );
        assert_eq!(trusted.client(CLIENT, &forwarded(&["junk"])?), Some(CLIENT));
        Ok(())
    }

    #[test]
    fn a_trusted_proxy_s_own_request_comes_from_the_proxy() {
        let trusted = TrustedProxies::new(vec![PROXY]);
        assert_eq!(trusted.client(PROXY, &HeaderMap::new()), Some(PROXY));
    }

    #[test]
    fn the_rightmost_untrusted_entry_is_the_client() -> TestResult {
        let trusted = TrustedProxies::new(vec![PROXY, ROUTER]);
        // A client's own claim on the left is not believed.
        let spoofed = format!("{RELAY}, {CLIENT}");
        assert_eq!(
            trusted.client(PROXY, &forwarded(&[&spoofed])?),
            Some(CLIENT)
        );
        // Trusted proxies in the chain are skipped, across repeated header lines.
        let chain = forwarded(&[&format!("{CLIENT}, {RELAY}"), &ROUTER.to_string()])?;
        assert_eq!(trusted.client(PROXY, &chain), Some(RELAY));
        // A chain of nothing but trusted proxies ends at the first of them.
        let internal = format!("{ROUTER}, {PROXY}");
        assert_eq!(
            trusted.client(PROXY, &forwarded(&[&internal])?),
            Some(ROUTER)
        );
        Ok(())
    }

    #[test]
    fn entries_may_carry_ports_and_ipv6_is_unmapped() -> TestResult {
        let trusted = TrustedProxies::new(vec![PROXY]);
        assert_eq!(
            trusted.client(PROXY, &forwarded(&["198.51.100.7:4711"])?),
            Some(CLIENT)
        );
        assert_eq!(
            trusted.client(PROXY, &forwarded(&["[2001:db8::7]:443"])?),
            Some(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)))
        );
        // The proxy reached over an IPv4-mapped address is still the proxy.
        let mapped = IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped());
        assert_eq!(
            trusted.client(mapped, &forwarded(&[&CLIENT.to_string()])?),
            Some(CLIENT)
        );
        Ok(())
    }

    #[test]
    fn an_entry_that_is_not_an_address_matches_nothing() -> TestResult {
        let trusted = TrustedProxies::new(vec![PROXY]);
        for bad in ["unknown", "", "198.51.100.7, ", "_hidden"] {
            assert_eq!(trusted.client(PROXY, &forwarded(&[bad])?), None, "{bad}");
        }
        // Even to the left of the client: the chain cannot be read past it.
        let past = format!("junk, {CLIENT}");
        assert_eq!(trusted.client(PROXY, &forwarded(&[&past])?), Some(CLIENT));
        let mut not_text = forwarded(&[&CLIENT.to_string()])?;
        not_text.append(FORWARDED_FOR, HeaderValue::from_bytes(b"\xff")?);
        assert_eq!(trusted.client(PROXY, &not_text), None);
        Ok(())
    }
}
