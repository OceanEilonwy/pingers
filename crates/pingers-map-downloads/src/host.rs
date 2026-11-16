// SPDX-License-Identifier: AGPL-3.0-only
//! Map hosts and the chain they are tried in (design B.12, "Which host").

use pingers_proto::MapHostUrl;

/// Where a map host came from, highest priority first (the order of the chain).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapHostSource {
    /// The user set it in Advanced.
    UserOverride,
    /// The Pingers server advertises it (`map_host` in `/v1/params`).
    Server,
    /// The publisher's built-in default.
    Publisher,
}

/// A map host, and why it is in the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapHost {
    url: MapHostUrl,
    source: MapHostSource,
}

impl MapHost {
    /// A host at `url` (an `https://` origin), from `source`. A trailing slash is dropped.
    #[must_use]
    pub fn new(url: &MapHostUrl, source: MapHostSource) -> Self {
        let trimmed = url.as_str().trim_end_matches('/');
        Self {
            url: MapHostUrl::new(trimmed.to_owned()).unwrap_or_else(|_| url.clone()),
            source,
        }
    }

    /// The host's origin, without a trailing slash.
    #[must_use]
    pub const fn url(&self) -> &MapHostUrl {
        &self.url
    }

    /// Why it is in the chain.
    #[must_use]
    pub const fn source(&self) -> MapHostSource {
        self.source
    }

    /// The URL of `file` on this host.
    pub(crate) fn file_url(&self, path: &str) -> String {
        format!("{}{path}", self.url.as_str())
    }
}

/// The hosts to try, highest priority first, each once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostChain {
    hosts: Vec<MapHost>,
}

impl HostChain {
    /// The chain from the user's override, the server's host and the publisher's default, in
    /// that order, without repeats (a host named twice keeps its higher place).
    #[must_use]
    pub fn new(
        user_override: Option<&MapHostUrl>,
        server: Option<&MapHostUrl>,
        publisher: Option<&MapHostUrl>,
    ) -> Self {
        let mut hosts: Vec<MapHost> = Vec::with_capacity(3);
        for (url, source) in [
            (user_override, MapHostSource::UserOverride),
            (server, MapHostSource::Server),
            (publisher, MapHostSource::Publisher),
        ] {
            let Some(url) = url else { continue };
            let host = MapHost::new(url, source);
            if hosts.iter().all(|known| known.url != host.url) {
                hosts.push(host);
            }
        }
        Self { hosts }
    }

    /// The hosts, highest priority first.
    #[must_use]
    pub fn hosts(&self) -> &[MapHost] {
        &self.hosts
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn url(text: &str) -> Result<MapHostUrl, pingers_proto::ProtoError> {
        MapHostUrl::new(text.to_owned())
    }

    #[test]
    fn the_chain_keeps_priority_order_and_drops_repeats() -> Result<(), pingers_proto::ProtoError> {
        let mine = url("https://mine.example/")?;
        let server = url("https://server.example")?;
        let publisher = url("https://mine.example")?;
        let chain = HostChain::new(Some(&mine), Some(&server), Some(&publisher));
        let hosts: Vec<(&str, MapHostSource)> = chain
            .hosts()
            .iter()
            .map(|host| (host.url().as_str(), host.source()))
            .collect();
        assert_eq!(
            hosts,
            [
                ("https://mine.example", MapHostSource::UserOverride),
                ("https://server.example", MapHostSource::Server),
            ]
        );
        let only = HostChain::new(None, None, Some(&publisher));
        assert_eq!(only.hosts().len(), 1);
        assert_eq!(
            only.hosts().first().map(MapHost::source),
            Some(MapHostSource::Publisher)
        );
        assert!(HostChain::new(None, None, None).hosts().is_empty());
        Ok(())
    }

    #[test]
    fn file_urls_join_without_doubling_slashes() -> Result<(), pingers_proto::ProtoError> {
        let host = MapHost::new(&url("https://maps.example/")?, MapHostSource::Server);
        assert_eq!(
            host.file_url("/catalog.json"),
            "https://maps.example/catalog.json"
        );
        Ok(())
    }
}
