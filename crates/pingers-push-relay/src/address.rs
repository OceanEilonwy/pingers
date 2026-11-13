// SPDX-License-Identifier: AGPL-3.0-only
//! Which addresses the relay may POST a device-chosen UnifiedPush endpoint to.
//!
//! The endpoint comes from a sealed token, so anyone who can seal one could otherwise make the
//! relay send requests into its own network (SSRF). Two checks:
//!
//! - literal IP hosts are checked when the endpoint is parsed ([`ip_is_public`]);
//! - host names are checked after resolution, at connect time, by [`PublicOnlyResolver`], the
//!   HTTP client's resolver. The addresses it checks are the ones the client connects to, so a
//!   name that resolves to a public address once and a private one later (DNS rebinding) is
//!   still refused.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Whether `address` is a globally routable unicast address the relay may connect to.
/// IPv6 forms that embed an IPv4 address (mapped, compatible, NAT64, 6to4) are judged by the
/// embedded address.
#[must_use]
pub fn ip_is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => ipv4_is_public(v4),
        IpAddr::V6(v6) => match embedded_ipv4(v6) {
            Embedded::V4(v4) => ipv4_is_public(v4),
            Embedded::Refused => false,
            Embedded::None => ipv6_is_public(v6),
        },
    }
}

fn ipv4_is_public(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    let reserved = match first {
        // 0.0.0.0/8 "this network", 10/8 private, 127/8 loopback, and 224.0.0.0/3:
        // multicast, reserved and broadcast.
        0 | 10 | 127 | 224..=255 => true,
        // 100.64.0.0/10 shared address space (carrier-grade NAT).
        100 => (64..128).contains(&second),
        // 169.254.0.0/16 link local (cloud metadata).
        169 => second == 254,
        // 172.16.0.0/12 private.
        172 => (16..32).contains(&second),
        192 => matches!(
            (second, third),
            // 192.0.0.0/24 IETF protocol assignments, 192.0.2.0/24 documentation,
            // 192.88.99.0/24 6to4 relay anycast, 192.168.0.0/16 private.
            (0, 0 | 2) | (88, 99) | (168, _)
        ),
        // 198.18.0.0/15 benchmarking, 198.51.100.0/24 documentation.
        198 => matches!(second, 18 | 19) || (second == 51 && third == 100),
        // 203.0.113.0/24 documentation.
        203 => second == 0 && third == 113,
        _ => false,
    };
    !reserved
}

fn ipv6_is_public(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let [first, second, ..] = segments;
    // :: and ::1 never get here: embedded_ipv4 refuses all of ::/96.
    // fc00::/7 unique local, fe80::/10 link local, fec0::/10 site local, ff00::/8 multicast.
    let reserved = first & 0xfe00 == 0xfc00
        || first & 0xffc0 == 0xfe80
        || first & 0xffc0 == 0xfec0
        || first & 0xff00 == 0xff00
        // 100::/64 discard.
        || (first == 0x0100 && segments.get(1..4).is_some_and(|rest| rest.iter().all(|s| *s == 0)))
        // 2001::/32 Teredo (embeds an obfuscated IPv4), 2001:db8::/32 documentation,
        // 2001:10::/28 and 2001:20::/28 ORCHID.
        || (first == 0x2001 && (second == 0 || second == 0x0db8 || second & 0xffe0 == 0x0020 || second & 0xfff0 == 0x0010))
        // 64:ff9b:1::/48 local-use NAT64.
        || (first == 0x0064 && second == 0xff9b && segments.get(2) == Some(&1));
    !reserved
}

enum Embedded {
    /// An IPv4 address carried inside the IPv6 one.
    V4(Ipv4Addr),
    /// A form that is never acceptable whatever it carries.
    Refused,
    /// A plain IPv6 address.
    None,
}

fn embedded_ipv4(address: Ipv6Addr) -> Embedded {
    let segments = address.segments();
    let octets = address.octets();
    let tail = || {
        let [.., a, b, c, d] = octets;
        Ipv4Addr::new(a, b, c, d)
    };
    match segments {
        // ::ffff:a.b.c.d mapped, and 64:ff9b::/96 NAT64, which reaches the embedded address.
        [0, 0, 0, 0, 0, 0xffff, _, _] | [0x0064, 0xff9b, 0, 0, 0, 0, _, _] => Embedded::V4(tail()),
        // ::a.b.c.d compatible (deprecated), with :: and ::1 in the same /96: never public.
        [0, 0, 0, 0, 0, 0, _, _] => Embedded::Refused,
        // 2002:a.b.c.d::/48 6to4.
        [0x2002, high, low, ..] => {
            let [a, b] = high.to_be_bytes();
            let [c, d] = low.to_be_bytes();
            Embedded::V4(Ipv4Addr::new(a, b, c, d))
        }
        _ => Embedded::None,
    }
}

/// Resolves a host name to IP addresses. The production lookup is the system resolver;
/// tests inject fixed answers.
pub trait HostLookup: Send + Sync + 'static {
    /// The addresses of `host`.
    fn lookup(&self, host: String)
    -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;
}

/// The system resolver, through tokio.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLookup;

impl HostLookup for SystemLookup {
    fn lookup(
        &self,
        host: String,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|address| address.ip())
                .collect())
        })
    }
}

/// Why a host name was refused.
#[derive(Debug, thiserror::Error)]
pub enum ResolveRefused {
    /// It resolved to nothing.
    #[error("the host has no addresses")]
    NoAddresses,
    /// One of its addresses is not public; the address is not named.
    #[error("the host resolves to a non-public address")]
    NonPublic,
}

/// An HTTP client resolver that refuses a host if any of its addresses is not public.
pub struct PublicOnlyResolver<L> {
    lookup: Arc<L>,
}

impl<L: HostLookup> PublicOnlyResolver<L> {
    /// A resolver over `lookup`.
    #[must_use]
    pub fn new(lookup: L) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }
}

/// The addresses to connect to, or why none may be used. Every address must be public: a
/// mixed answer is refused rather than filtered, since it is a sign of rebinding.
///
/// # Errors
///
/// [`ResolveRefused`].
pub fn public_addresses(addresses: Vec<IpAddr>) -> Result<Vec<SocketAddr>, ResolveRefused> {
    if addresses.is_empty() {
        return Err(ResolveRefused::NoAddresses);
    }
    if !addresses.iter().all(|address| ip_is_public(*address)) {
        return Err(ResolveRefused::NonPublic);
    }
    // reqwest replaces port 0 with the URL's port.
    Ok(addresses
        .into_iter()
        .map(|address| SocketAddr::new(address, 0))
        .collect())
}

impl<L: HostLookup> Resolve for PublicOnlyResolver<L> {
    fn resolve(&self, name: Name) -> Resolving {
        let lookup = Arc::clone(&self.lookup);
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = lookup.lookup(host).await?;
            let allowed = public_addresses(addresses)?;
            let iterator: Addrs = Box::new(allowed.into_iter());
            Ok(iterator)
        })
    }
}

#[cfg(test)]
mod tests;
