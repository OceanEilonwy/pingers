// SPDX-License-Identifier: AGPL-3.0-only
//! What the hosts in the chain offer (design B.12, "Which host"): each region comes from the
//! first host that lists it, so a host that lacks an area hands only that area to the next.
//! Catalogs say nothing about where the user is.

use crate::fetch::MapFetch;
use crate::manifest::parse_catalog;
use crate::pieces::{Fetching, fetch_small};
use crate::{CatalogEntry, HostChain, MapHost, MapsError};

/// The largest catalog accepted: thousands of regions, but not enough to fill memory.
pub const MAX_CATALOG: u64 = 16 * 1024 * 1024;

/// A region on offer, with the host that will serve it.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionOffer {
    /// The region.
    pub entry: CatalogEntry,
    /// Where it comes from.
    pub host: MapHost,
}

/// Why a host offered nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostFailure {
    /// It did not answer, or not with a catalog.
    Unreachable,
    /// With the relay setting on, only the host the Pingers server forwards to can be
    /// reached, and this is another one; nothing was sent to it.
    NotThroughRelay,
    /// The relay route has no Pingers server to go through yet.
    NoRelayRoute,
    /// Its gateway key changed since it was pinned.
    KeyChanged,
}

impl HostFailure {
    fn of(error: &MapsError) -> Self {
        match error {
            MapsError::NotThroughRelay => Self::NotThroughRelay,
            MapsError::NoRelayRoute => Self::NoRelayRoute,
            MapsError::KeyChanged => Self::KeyChanged,
            MapsError::Transport(_)
            | MapsError::Status { .. }
            | MapsError::Preparing { .. }
            | MapsError::NotOffered
            | MapsError::Checksum
            | MapsError::BadAnswer
            | MapsError::Ohttp(_)
            | MapsError::PinStore
            | MapsError::Io(_)
            | MapsError::Internal => Self::Unreachable,
        }
    }
}

/// A host that offered nothing, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedHost {
    /// The host.
    pub host: MapHost,
    /// Why.
    pub reason: HostFailure,
}

/// Every region on offer, and the hosts that offered nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// Regions, grouped (by group, or name if none) and then by name.
    pub offers: Vec<RegionOffer>,
    /// Hosts that failed, in chain order.
    pub failed: Vec<FailedHost>,
}

/// What one host listed.
#[derive(Clone, Debug, PartialEq)]
pub struct HostListing {
    /// The host.
    pub host: MapHost,
    /// Its catalog's entries.
    pub entries: Vec<CatalogEntry>,
}

/// Fetches each host's `catalog.json` in chain order and resolves the offers.
pub async fn fetch_catalog(fetch: &dyn MapFetch, chain: &HostChain) -> Catalog {
    let mut listed: Vec<HostListing> = Vec::new();
    let mut failed = Vec::new();
    for host in chain.hosts() {
        let mut fetching = Fetching::new(fetch, host);
        let fetched = fetch_small(&mut fetching, "/catalog.json", MAX_CATALOG)
            .await
            .and_then(|body| parse_catalog(&body));
        match fetched {
            Ok(entries) => listed.push(HostListing {
                host: host.clone(),
                entries,
            }),
            Err(error) => failed.push(FailedHost {
                host: host.clone(),
                reason: HostFailure::of(&error),
            }),
        }
    }
    Catalog {
        offers: resolve(listed),
        failed,
    }
}

/// Each region from the first host (in chain order) that lists it, sorted for the picker.
#[must_use]
pub fn resolve(listed: Vec<HostListing>) -> Vec<RegionOffer> {
    let mut offers: Vec<RegionOffer> = Vec::new();
    for HostListing { host, entries } in listed {
        for entry in entries {
            if offers.iter().all(|offer| offer.entry.id() != entry.id()) {
                offers.push(RegionOffer {
                    entry,
                    host: host.clone(),
                });
            }
        }
    }
    offers.sort_by(|left, right| {
        let key = |offer: &RegionOffer| {
            (
                offer.entry.group().unwrap_or(offer.entry.name()).to_owned(),
                offer.entry.name().to_owned(),
            )
        };
        key(left).cmp(&key(right))
    });
    offers
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::MapHostUrl;

    use super::*;
    use crate::{MapHostSource, StemName};

    fn host(url: &str, source: MapHostSource) -> Result<MapHost, MapsError> {
        Ok(MapHost::new(
            &MapHostUrl::new(url.to_owned()).map_err(|_| MapsError::BadAnswer)?,
            source,
        ))
    }

    fn stub(id: &str, name: &str, group: Option<&str>) -> Result<CatalogEntry, MapsError> {
        Ok(CatalogEntry::OnDemand {
            id: id.to_owned(),
            stem: StemName::of_region(id)?,
            name: name.to_owned(),
            group: group.map(str::to_owned),
        })
    }

    #[test]
    fn each_region_comes_from_the_first_host_listing_it() -> Result<(), MapsError> {
        let mine = host("https://mine.example", MapHostSource::UserOverride)?;
        let server = host("https://server.example", MapHostSource::Server)?;
        let offers = resolve(vec![
            HostListing {
                host: mine,
                entries: vec![stub("a/x", "Xville", Some("A"))?],
            },
            HostListing {
                host: server,
                entries: vec![
                    stub("a/x", "Xville", Some("A"))?,
                    stub("b", "Bland", None)?,
                    stub("a/c", "Cetown", Some("A"))?,
                ],
            },
        ]);
        let summary: Vec<(&str, &str)> = offers
            .iter()
            .map(|offer| (offer.entry.id(), offer.host.url().as_str()))
            .collect();
        // Grouped by group (or name), then name: A's Cetown and Xville, then Bland.
        assert_eq!(
            summary,
            [
                ("a/c", "https://server.example"),
                ("a/x", "https://mine.example"),
                ("b", "https://server.example"),
            ]
        );
        Ok(())
    }

    #[test]
    fn failures_say_why() {
        assert_eq!(
            HostFailure::of(&MapsError::NotThroughRelay),
            HostFailure::NotThroughRelay
        );
        assert_eq!(
            HostFailure::of(&MapsError::NoRelayRoute),
            HostFailure::NoRelayRoute
        );
        assert_eq!(
            HostFailure::of(&MapsError::KeyChanged),
            HostFailure::KeyChanged
        );
        assert_eq!(
            HostFailure::of(&MapsError::Status { status: 500 }),
            HostFailure::Unreachable
        );
    }
}
