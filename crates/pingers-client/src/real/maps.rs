// SPDX-License-Identifier: AGPL-3.0-only
//! [`MapsPort`] over pingers-map-downloads. Maps are a device setting, so they follow the app's
//! built-in server whatever servers the Parties are on: the chain gets its map host from
//! [`Net`], direct downloads use a public HTTPS client trusting the instance's roots, relayed
//! ones go through the built-in server's router, sealed only to a map host gateway key among
//! that server's pins (from the build, a setup link or an invite; never taken on first use).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pingers_map_downloads::catalog::{Catalog, FailedHost, HostFailure, fetch_catalog};
use pingers_map_downloads::fetch::{DirectFetch, MapFetch, RelayFetch, RelayParts};
use pingers_map_downloads::install::{self, Download, DownloadProgress};
use pingers_map_downloads::{HostChain, MapsError, RegionManifest};
use pingers_transport::{PublicClient, PublicConfig, TlsRoots};

use super::Backend;
use super::sync::Net;
use crate::ClientError;
use crate::ports::{MapDownload, MapHosts, MapRoute, MapsPort};

/// The largest direct answer: one piece and its headers.
pub(super) const DIRECT_LIMIT: usize = 5 * 1024 * 1024;

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

    /// The route for `route`: direct, or through the built-in server's relay to its map host.
    /// `None` for the relay without a built-in server.
    async fn fetch(&self, route: MapRoute) -> Result<Option<Arc<dyn MapFetch>>, ClientError> {
        Ok(match route {
            MapRoute::Direct => Some(self.direct.clone()),
            MapRoute::Relay => match self.net.servers.built_in_link().await {
                Err(ClientError::Unavailable(_)) => None,
                Err(error) => return Err(error),
                Ok(link) => Some(Arc::new(RelayFetch::new(RelayParts {
                    transport: Arc::clone(&link.transport),
                    server_host: self.net.map_host().await?,
                    pins: link.record.map_host_pins.clone(),
                    rng: (self.backend.outside.rng)(),
                }))),
            },
        })
    }
}

#[async_trait]
impl MapsPort for Maps {
    async fn catalog(&self, hosts: MapHosts, route: MapRoute) -> Result<Catalog, ClientError> {
        // The built-in server's parameters, read through its relay once per process (anonymous
        // and public: they say nothing about the device) and its map host recorded, so a fresh
        // install knows it; offline or unreachable, the host last recorded.
        if let Err(error @ (ClientError::Store(_) | ClientError::Records(_))) =
            self.net.built_in_params().await
        {
            return Err(error);
        }
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
}
