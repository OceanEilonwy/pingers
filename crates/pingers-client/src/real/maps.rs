// SPDX-License-Identifier: AGPL-3.0-only
//! [`MapsPort`] over pingers-map-downloads: the chain gets the server's map host from
//! [`Net`], direct downloads use a public HTTPS client trusting the instance's roots, relayed
//! ones go through the instance's router, and the map host gateway key pinned on first use is
//! a record in the store.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pingers_map_downloads::catalog::{Catalog, FailedHost, HostFailure, fetch_catalog};
use pingers_map_downloads::fetch::{DirectFetch, KeyPinStore, MapFetch, RelayFetch, RelayParts};
use pingers_map_downloads::install::{self, Download, DownloadProgress};
use pingers_map_downloads::{HostChain, MapsError, RegionManifest};
use pingers_proto::{KeyConfigHash, MapHostUrl};
use pingers_transport::{PublicClient, PublicConfig, TlsRoots};

use super::Backend;
use super::records::{self, Key};
use super::sync::Net;
use crate::ClientError;
use crate::ports::{MapDownload, MapHosts, MapRoute, MapsPort};

/// The largest direct answer: one piece and its headers.
const DIRECT_LIMIT: usize = 5 * 1024 * 1024;

/// The maps port.
pub(crate) struct Maps {
    backend: Arc<Backend>,
    net: Arc<Net>,
    direct: Arc<DirectFetch>,
}

impl Maps {
    /// The port, with direct downloads trusting `roots`.
    pub(crate) fn new(
        backend: Arc<Backend>,
        net: Arc<Net>,
        roots: TlsRoots,
    ) -> Result<Self, ClientError> {
        let client = PublicClient::new(
            &PublicConfig::new(roots, DIRECT_LIMIT),
            Arc::clone(&backend.outside.timer),
        )?;
        Ok(Self {
            backend,
            net,
            direct: Arc::new(DirectFetch::new(client)),
        })
    }

    /// The route for `route`: direct, or through the relay to the server's map host. `None`
    /// for the relay without an instance.
    async fn fetch(&self, route: MapRoute) -> Result<Option<Arc<dyn MapFetch>>, ClientError> {
        Ok(match route {
            MapRoute::Direct => Some(self.direct.clone()),
            MapRoute::Relay => match &self.backend.transport {
                None => None,
                Some(transport) => Some(Arc::new(RelayFetch::new(RelayParts {
                    transport: Arc::clone(transport),
                    server_host: self.net.map_host().await?,
                    pins: Arc::new(StoredPins(Arc::clone(&self.backend))),
                    rng: (self.backend.outside.rng)(),
                }))),
            },
        })
    }
}

#[async_trait]
impl MapsPort for Maps {
    async fn catalog(&self, hosts: MapHosts, route: MapRoute) -> Result<Catalog, ClientError> {
        let server = self.net.map_host().await?;
        let chain = HostChain::new(
            hosts.user_override.as_ref(),
            server.as_ref(),
            hosts.publisher.as_ref(),
        );
        let Some(fetch) = self.fetch(route).await? else {
            return Ok(Catalog {
                offers: Vec::new(),
                failed: chain
                    .hosts()
                    .iter()
                    .map(|host| FailedHost {
                        host: host.clone(),
                        reason: HostFailure::NoRelayRoute,
                    })
                    .collect(),
            });
        };
        Ok(fetch_catalog(fetch.as_ref(), &chain).await)
    }

    async fn download(
        &self,
        job: MapDownload,
        progress: Arc<dyn DownloadProgress>,
    ) -> Result<RegionManifest, ClientError> {
        let fetch = self
            .fetch(job.route)
            .await?
            .ok_or(MapsError::NoRelayRoute)?;
        Ok(install::download(&Download {
            fetch: fetch.as_ref(),
            timer: self.backend.outside.timer.as_ref(),
            offer: &job.offer,
            directory: &job.directory,
            progress: progress.as_ref(),
        })
        .await?)
    }

    async fn installed(&self, directory: PathBuf) -> Result<Vec<RegionManifest>, ClientError> {
        Ok(install::installed(&directory).await?)
    }

    async fn delete(&self, directory: PathBuf, id: String) -> Result<(), ClientError> {
        Ok(install::delete(&directory, &id).await?)
    }

    async fn forget_key_pin(&self) -> Result<(), ClientError> {
        self.backend
            .store(|session, _clock, _rng| records::delete(session, &Key::MapHostPin))
            .await
    }
}

/// The map host gateway key pinned on first use, in the store: one record naming the host
/// and the hash (the server forwards to one map host at a time).
pub(super) struct StoredPins(pub(super) Arc<Backend>);

#[async_trait]
impl KeyPinStore for StoredPins {
    async fn pinned(&self, host: &MapHostUrl) -> Result<Option<KeyConfigHash>, MapsError> {
        let host = host.as_str().to_owned();
        let stored = self
            .0
            .store(|session, _clock, _rng| {
                records::read(session, &Key::MapHostPin, |reader| {
                    let hash: [u8; 32] = reader.fixed()?;
                    let pinned_host =
                        String::from_utf8(reader.rest()).map_err(|_not_text| records::corrupt())?;
                    Ok((pinned_host, KeyConfigHash::from_bytes(hash)))
                })
            })
            .await
            .map_err(|_store| MapsError::PinStore)?;
        // A pin for another host says nothing about this one.
        Ok(stored.and_then(|(pinned_host, hash)| (pinned_host == host).then_some(hash)))
    }

    async fn pin(&self, host: &MapHostUrl, hash: KeyConfigHash) -> Result<(), MapsError> {
        let host = host.as_str().to_owned();
        self.0
            .store(move |session, _clock, _rng| {
                records::write(session, &Key::MapHostPin, |writer| {
                    writer.fixed(hash.as_bytes());
                    writer.fixed(host.as_bytes());
                })
            })
            .await
            .map_err(|_store| MapsError::PinStore)
    }
}
