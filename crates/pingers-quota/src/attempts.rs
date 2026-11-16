// SPDX-License-Identifier: AGPL-3.0-only
//! Failed registration codes, counted per client address and per account key (design B.1).
//!
//! A code has at least 128 bits of entropy, so guessing one is hopeless anyway; this keeps a
//! guesser from even trying many. `POST /v1/accounts` arrives direct (identified), so the
//! server sees the address. An address counts by its IPv4 address or its IPv6 /64, since one
//! host has a whole /64. In memory only, each map capped at [`MAX_TRACKED`] entries: when one
//! is full of live entries a new address or key is not recorded, rather than evicting one an
//! attacker could then try again from.

use core::time::Duration;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard};

use pingers_crypto::AccountPublicKey;
use pingers_time::UnixSeconds;

use crate::QuotaError;

/// The window failures are counted over.
pub const CODE_ATTEMPT_WINDOW: Duration = Duration::from_hours(1);

/// Failed codes one address may send per [`CODE_ATTEMPT_WINDOW`].
pub const CODE_ATTEMPTS_PER_ADDRESS: u32 = 10;

/// Failed codes one account key may send per [`CODE_ATTEMPT_WINDOW`].
pub const CODE_ATTEMPTS_PER_KEY: u32 = 5;

/// The most addresses, and the most keys, tracked at once.
pub const MAX_TRACKED: usize = 65_536;

/// Where a registration came from: the client's address as the server saw it, after its
/// trusted proxies. `None` when it cannot be told (then only the key is counted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientAddress(Option<IpAddr>);

impl ClientAddress {
    /// Wraps the address the request came from.
    #[must_use]
    pub const fn new(address: Option<IpAddr>) -> Self {
        Self(address)
    }

    /// The key failures are counted under: the IPv4 address, or the IPv6 /64.
    fn key(self) -> Option<[u8; 9]> {
        match self.0? {
            IpAddr::V4(v4) => {
                let mut key = [4, 0, 0, 0, 0, 0, 0, 0, 0];
                for (slot, octet) in key.iter_mut().skip(1).zip(v4.octets()) {
                    *slot = octet;
                }
                Some(key)
            }
            IpAddr::V6(v6) => {
                let mut key = [6, 0, 0, 0, 0, 0, 0, 0, 0];
                for (slot, octet) in key.iter_mut().skip(1).zip(v6.octets()) {
                    *slot = octet;
                }
                Some(key)
            }
        }
    }
}

/// Failures since a window began.
#[derive(Clone, Copy, Debug)]
struct Count {
    since: UnixSeconds,
    failures: u32,
}

#[derive(Default)]
struct Inner {
    addresses: HashMap<[u8; 9], Count>,
    keys: HashMap<[u8; 32], Count>,
}

/// The failed-code counters.
#[derive(Default)]
pub(crate) struct CodeAttempts {
    inner: Mutex<Inner>,
}

/// Whether `count` is still in its window at `now`.
fn live(count: &Count, now: UnixSeconds) -> bool {
    now.saturating_duration_since(count.since) < CODE_ATTEMPT_WINDOW
}

fn over(map_count: Option<&Count>, now: UnixSeconds, limit: u32) -> bool {
    map_count.is_some_and(|count| live(count, now) && count.failures >= limit)
}

fn record<K: Eq + core::hash::Hash>(map: &mut HashMap<K, Count>, key: K, now: UnixSeconds) {
    if map.len() >= MAX_TRACKED && !map.contains_key(&key) {
        map.retain(|_, count| live(count, now));
        if map.len() >= MAX_TRACKED {
            return;
        }
    }
    let count = map.entry(key).or_insert(Count {
        since: now,
        failures: 0,
    });
    if !live(count, now) {
        *count = Count {
            since: now,
            failures: 0,
        };
    }
    count.failures = count.failures.saturating_add(1);
}

impl CodeAttempts {
    fn lock(&self) -> Result<MutexGuard<'_, Inner>, QuotaError> {
        self.inner.lock().map_err(|_| QuotaError::Poisoned)
    }

    /// Refuses a registration from an address or key that has used up its failures.
    pub(crate) fn check(
        &self,
        from: ClientAddress,
        key: &AccountPublicKey,
        now: UnixSeconds,
    ) -> Result<(), QuotaError> {
        let inner = self.lock()?;
        let address = from.key().is_some_and(|address| {
            over(
                inner.addresses.get(&address),
                now,
                CODE_ATTEMPTS_PER_ADDRESS,
            )
        });
        if address || over(inner.keys.get(key.as_bytes()), now, CODE_ATTEMPTS_PER_KEY) {
            return Err(QuotaError::TooManyCodeAttempts);
        }
        Ok(())
    }

    /// Counts a missing or wrong code against the address and the key.
    pub(crate) fn failed(
        &self,
        from: ClientAddress,
        key: &AccountPublicKey,
        now: UnixSeconds,
    ) -> Result<(), QuotaError> {
        let mut inner = self.lock()?;
        if let Some(address) = from.key() {
            record(&mut inner.addresses, address, now);
        }
        record(&mut inner.keys, *key.as_bytes(), now);
        Ok(())
    }
}
