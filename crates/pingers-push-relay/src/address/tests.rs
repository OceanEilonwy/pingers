// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::HashMap;
use std::str::FromStr as _;

use super::*;

fn public(text: &str) -> bool {
    text.parse::<IpAddr>().is_ok_and(ip_is_public)
}

#[test]
fn global_unicast_addresses_are_public() {
    for address in [
        "1.1.1.1",
        "8.8.8.8",
        "100.128.0.1",
        "172.32.0.1",
        "192.0.1.1",
        "198.20.0.1",
        "223.255.255.255",
        "2001:4860::1",
        "2606:4700::1111",
        "::ffff:8.8.8.8",
        "64:ff9b::808:808",
        "2002:808:808::1",
    ] {
        assert!(public(address), "{address} should be public");
    }
}

#[test]
fn reserved_ipv4_ranges_are_refused() {
    for address in [
        "0.0.0.0",
        "0.1.2.3",
        "10.0.0.1",
        "100.64.0.1",
        "100.127.255.255",
        "127.0.0.1",
        "127.255.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.8",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.19.255.255",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "239.255.255.250",
        "240.0.0.1",
        "255.255.255.255",
    ] {
        assert!(!public(address), "{address} should be refused");
    }
}

#[test]
fn reserved_ipv6_ranges_are_refused() {
    for address in [
        "::",
        "::1",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "febf::1",
        "fec0::1",
        "ff02::1",
        "ff0e::1",
        "100::1",
        "2001::1",
        "2001:db8::1",
        "2001:10::1",
        "2001:20::1",
        "64:ff9b:1::1",
    ] {
        assert!(!public(address), "{address} should be refused");
    }
}

#[test]
fn ipv6_forms_carrying_a_private_ipv4_address_are_refused() {
    for address in [
        // Mapped.
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "::ffff:169.254.169.254",
        // Compatible (deprecated): refused whatever it carries.
        "::127.0.0.1",
        "::8.8.8.8",
        "::2",
        // NAT64 to a private address.
        "64:ff9b::7f00:1",
        "64:ff9b::a9fe:a9fe",
        // 6to4 around a private address.
        "2002:7f00:1::1",
        "2002:c0a8:101::1",
    ] {
        assert!(!public(address), "{address} should be refused");
    }
}

/// Answers from a fixed table; unknown names fail like NXDOMAIN.
struct FixedLookup(HashMap<&'static str, Vec<IpAddr>>);

impl HostLookup for FixedLookup {
    fn lookup(
        &self,
        host: String,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
        let answer = self
            .0
            .get(host.as_str())
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"));
        Box::pin(std::future::ready(answer))
    }
}

fn resolver() -> PublicOnlyResolver<FixedLookup> {
    let ip = |text: &str| {
        text.parse::<IpAddr>()
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    };
    PublicOnlyResolver::new(FixedLookup(HashMap::from([
        ("ntfy.example", vec![ip("203.0.114.7"), ip("2001:4860::7")]),
        ("rebind.example", vec![ip("127.0.0.1")]),
        ("mixed.example", vec![ip("203.0.114.7"), ip("10.0.0.7")]),
        ("metadata.example", vec![ip("::ffff:169.254.169.254")]),
        ("empty.example", Vec::new()),
    ])))
}

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn a_name_with_only_public_addresses_resolves() -> TestResult {
    let addresses: Vec<SocketAddr> = resolver()
        .resolve(Name::from_str("ntfy.example")?)
        .await?
        .collect();
    assert_eq!(addresses.len(), 2);
    assert!(addresses.iter().all(|address| ip_is_public(address.ip())));
    Ok(())
}

#[tokio::test]
async fn names_resolving_to_private_mixed_or_no_addresses_are_refused() -> TestResult {
    for host in [
        "rebind.example",
        "mixed.example",
        "metadata.example",
        "empty.example",
        "unknown.example",
    ] {
        assert!(
            resolver().resolve(Name::from_str(host)?).await.is_err(),
            "{host} should be refused"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_client_does_not_connect_to_a_name_that_resolves_to_loopback() -> TestResult {
    // A distributor on loopback, reached by a name that resolves there: the request must fail
    // before it is sent, so the distributor sees nothing.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let client = reqwest::Client::builder()
        .dns_resolver(resolver())
        .build()?;
    let result = client
        .post(format!("http://rebind.example:{port}/up"))
        .send()
        .await;
    // Refused by our resolver, not by a failed lookup or connection.
    let refusal = result
        .err()
        .map(|error| format!("{error:?}"))
        .unwrap_or_default();
    assert!(refusal.contains("NonPublic"), "{refusal}");
    // Had the client connected, the connection would be queued by now.
    let accepted = listener.accept();
    assert!(
        matches!(&accepted, Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "the relay connected to loopback"
    );
    Ok(())
}
