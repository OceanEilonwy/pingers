// SPDX-License-Identifier: AGPL-3.0-only
//! Installing, listing and removing regions in the maps directory.
//!
//! A region is installed exactly when its manifest is on disk: the index and the pack are
//! downloaded and checked first, and the manifest is written last (and removed first).

use std::path::Path;
use std::time::Duration;

use pingers_ohttp::Header;
use pingers_time::Timer;

use crate::catalog::RegionOffer;
use crate::fetch::{MapFetch, Padding, padded_count};
use crate::pieces::{Fetching, fetch_file, fetch_small, partial_path, remove_if_present};
use crate::{CatalogEntry, MapHost, MapsError, RegionManifest, StemName};

/// The largest manifest accepted.
const MAX_MANIFEST: u64 = 64 * 1024;
/// How many times a region the host is still preparing is asked for again.
const PREPARING_ATTEMPTS: u32 = 40;
/// The shortest wait between those attempts, whatever the host asks.
const SHORTEST_WAIT: Duration = Duration::from_secs(1);
/// The longest wait between those attempts, whatever the host asks.
const LONGEST_WAIT: Duration = Duration::from_mins(2);

/// What a download reports while it runs.
pub trait DownloadProgress: Send + Sync {
    /// `done` of `total` bytes (index and pack together) are on the phone.
    fn downloaded(&self, done: u64, total: u64);
    /// The host is cutting the region (cache mode); the download asks again after
    /// `retry_after`.
    fn preparing(&self, retry_after: Duration);
}

/// One download: what, where to, over which route, and who hears about it.
pub struct Download<'job> {
    /// The route.
    pub fetch: &'job dyn MapFetch,
    /// Waits while a cache-mode host prepares the region.
    pub timer: &'job dyn Timer,
    /// The region and its host.
    pub offer: &'job RegionOffer,
    /// The maps directory.
    pub directory: &'job Path,
    /// Told of progress.
    pub progress: &'job dyn DownloadProgress,
}

/// Downloads the offer into the directory, waiting while a cache-mode host prepares the
/// region, and returns the installed manifest.
///
/// # Errors
///
/// [`MapsError::Checksum`] if a file does not match its manifest (its partial is removed);
/// [`MapsError::Preparing`] if the host was still preparing after every attempt;
/// [`MapsError::NotOffered`], [`MapsError::NotThroughRelay`], [`MapsError::KeyChanged`];
/// [`MapsError::Io`] if the directory cannot be written; any other exchange error.
pub async fn download(job: &Download<'_>) -> Result<RegionManifest, MapsError> {
    tokio::fs::create_dir_all(job.directory).await?;
    let mut attempt = 0_u32;
    loop {
        match download_once(job).await {
            Err(MapsError::Preparing { retry_after }) if attempt < PREPARING_ATTEMPTS => {
                attempt = attempt.saturating_add(1);
                job.progress.preparing(retry_after);
                job.timer
                    .sleep(retry_after.clamp(SHORTEST_WAIT, LONGEST_WAIT))
                    .await;
            }
            result => return result,
        }
    }
}

async fn download_once(job: &Download<'_>) -> Result<RegionManifest, MapsError> {
    let mut fetching = Fetching::new(job.fetch, &job.offer.host);
    let manifest = match &job.offer.entry {
        CatalogEntry::Ready(manifest) => (**manifest).clone(),
        CatalogEntry::OnDemand { id, stem, .. } => {
            let path = format!("/{}", stem.manifest_file());
            let body = fetch_small(&mut fetching, &path, MAX_MANIFEST).await?;
            let manifest = RegionManifest::from_json(&body)?;
            if manifest.id != *id {
                return Err(MapsError::BadAnswer);
            }
            manifest
        }
    };
    let total = manifest.total_size();
    let index_size = manifest.index.size;
    let progress = job.progress;
    fetch_file(&mut fetching, job.directory, &manifest.index, &mut |done| {
        progress.downloaded(done, total);
    })
    .await?;
    fetch_file(&mut fetching, job.directory, &manifest.pack, &mut |done| {
        progress.downloaded(index_size.saturating_add(done), total);
    })
    .await?;
    pad_requests(job.fetch, &job.offer.host, &manifest, fetching.requests()).await;
    write_manifest(job.directory, &manifest).await?;
    Ok(manifest)
}

/// Through the relay, tops the download's requests up to its size class with requests for the
/// pack's first piece (answered and padded like any other), so the relay and the server see a
/// class, not a size. Their answers are not needed and failures do not matter.
async fn pad_requests(fetch: &dyn MapFetch, host: &MapHost, manifest: &RegionManifest, made: u32) {
    let piece = fetch.piece();
    if piece.padding != Padding::SizeClasses {
        return;
    }
    let path = format!("/{}", manifest.pack.file);
    let last = piece.bytes.min(manifest.pack.size).saturating_sub(1);
    for _ in made..padded_count(made) {
        let range = Header {
            name: "range".to_owned(),
            value: format!("bytes=0-{last}").into_bytes(),
        };
        let _padding = fetch.get(host, &path, vec![range]).await;
    }
}

/// Writes the manifest beside its files, through a temporary name, last.
async fn write_manifest(directory: &Path, manifest: &RegionManifest) -> Result<(), MapsError> {
    let target = directory.join(manifest.stem.manifest_file());
    let temporary = directory.join(format!("{}.partial", manifest.stem.manifest_file()));
    tokio::fs::write(&temporary, manifest.to_json()?).await?;
    tokio::fs::rename(&temporary, &target).await?;
    Ok(())
}

/// The installed regions: every manifest in `directory` that parses and whose pack is there at
/// its full length. A missing directory holds none.
///
/// # Errors
///
/// [`MapsError::Io`] if the directory cannot be listed.
pub async fn installed(directory: &Path) -> Result<Vec<RegionManifest>, MapsError> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut manifests = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(entry.path()).await else {
            continue;
        };
        let Ok(manifest) = RegionManifest::from_json(&bytes) else {
            continue;
        };
        // A manifest named for another region is not this file's.
        if manifest.stem.manifest_file() != name {
            continue;
        }
        let pack = tokio::fs::metadata(directory.join(&manifest.pack.file)).await;
        if pack.is_ok_and(|pack| pack.len() == manifest.pack.size) {
            manifests.push(manifest);
        }
    }
    manifests.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(manifests)
}

/// Removes region `id` from `directory`: its manifest first, so it stops counting as
/// installed, then its files and any partial downloads.
///
/// # Errors
///
/// [`MapsError::BadAnswer`] for an id that is not a region id; [`MapsError::Io`] if a file
/// cannot be removed.
pub async fn delete(directory: &Path, id: &str) -> Result<(), MapsError> {
    let stem = StemName::of_region(id)?;
    let manifest = directory.join(stem.manifest_file());
    let known = tokio::fs::read(&manifest)
        .await
        .ok()
        .and_then(|bytes| RegionManifest::from_json(&bytes).ok());
    remove_if_present(&manifest).await?;
    for file in [stem.pack_file(), stem.index_file()] {
        remove_if_present(&directory.join(file)).await?;
    }
    if let Some(known) = known {
        for file in [&known.pack, &known.index] {
            remove_if_present(&partial_path(directory, file)).await?;
        }
    }
    remove_partials(directory, &stem).await
}

/// Removes every partial download of `stem`'s files, whatever version it was for.
async fn remove_partials(directory: &Path, stem: &StemName) -> Result<(), MapsError> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let prefixes = [
        format!("{}.", stem.pack_file()),
        format!("{}.", stem.index_file()),
        format!("{}.", stem.manifest_file()),
    ];
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".partial") && prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            remove_if_present(&entry.path()).await?;
        }
    }
    Ok(())
}
