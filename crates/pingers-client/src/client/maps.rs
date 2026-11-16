// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s map calls: pingers-map-downloads, so the shell makes no network calls (design
//! E.1). Maps are files on the phone, not part of the snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use pingers_map_downloads::RegionManifest;
use pingers_map_downloads::catalog::Catalog;
use pingers_map_downloads::install::DownloadProgress;

use super::Client;
use crate::ClientError;
use crate::ports::{MapDownload, MapHosts, MapRoute};

impl Client {
    /// What the map host chain offers (Downloaded maps): the user's override, the server's
    /// map host and the publisher's default, over `route`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Store`] or [`ClientError::Records`] if the server's map host cannot be
    /// read; a host that fails is listed in the catalog, not returned as an error.
    pub async fn map_catalog(
        &self,
        hosts: MapHosts,
        route: MapRoute,
    ) -> Result<Catalog, ClientError> {
        self.crates.maps.catalog(hosts, route).await
    }

    /// Downloads, checks and installs a region (Download), telling `progress` as it goes.
    ///
    /// # Errors
    ///
    /// [`ClientError::Maps`] for a download that failed or did not check out.
    pub async fn download_map(
        &self,
        job: MapDownload,
        progress: Arc<dyn DownloadProgress>,
    ) -> Result<RegionManifest, ClientError> {
        self.crates.maps.download(job, progress).await
    }

    /// The regions installed in `directory`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Maps`] if the directory cannot be read.
    pub async fn installed_maps(
        &self,
        directory: PathBuf,
    ) -> Result<Vec<RegionManifest>, ClientError> {
        self.crates.maps.installed(directory).await
    }

    /// Removes region `id` from `directory` (Delete).
    ///
    /// # Errors
    ///
    /// [`ClientError::Maps`] for an id that is not a region id, or a file that cannot go.
    pub async fn delete_map(&self, directory: PathBuf, id: String) -> Result<(), ClientError> {
        self.crates.maps.delete(directory, id).await
    }

    /// Forgets the pinned map host gateway key (the user trusts the host's new key).
    ///
    /// # Errors
    ///
    /// [`ClientError::Store`] or [`ClientError::Records`] if the record cannot be removed.
    pub async fn forget_map_host_key(&self) -> Result<(), ClientError> {
        self.crates.maps.forget_key_pin().await
    }
}
