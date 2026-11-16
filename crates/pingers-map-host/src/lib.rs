// SPDX-License-Identifier: AGPL-3.0-only
//! Map host.
//!
//! A public static service, like the push relay: publisher-hosted by default, and an operator
//! may run their own. It serves whole-region map data that clients download whole, never per
//! viewport: per-region manifests (`<stem>.json`), PMTiles packs (`<stem>.pmtiles`) and place
//! search indexes (`<stem>.places.tsv`), where the stem is the region id with `/` replaced by
//! `.`, plus a generated `catalog.json`. Only allow-listed (`hosted`) regions are served.
//!
//! Two modes:
//!
//! - **Pre-extracted** (default): the directory holds `pingers-tiles` output and is served as
//!   it is.
//! - **Cache** (`[cache]` in the configuration): allow-listed regions of a region list are cut
//!   from an upstream planet pack on first request ([`cache`]), within a disk budget.
//!
//! The listener speaks HTTP/1.1 and cleartext HTTP/2 (h2c); TLS terminates at a reverse proxy
//! in front of it. Logging is off by default and never records addresses, paths or regions
//! ([`counts`]).
//!
//! It also runs its own OHTTP gateway ([`gateway`]) for downloads through a privacy relay.
//!
//! What it learns: a client's IP address and which region files it downloads (unless the
//! client fetches through an OHTTP relay). Never viewports, positions or searches: searches
//! run on the device against the downloaded index.
#![forbid(unsafe_code)]

pub mod cache;
pub mod catalog;
pub mod config;
pub mod counts;
pub mod error;
pub mod files;
pub mod gateway;
pub mod http;
pub mod range;
pub mod region;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use pingers_time::Clock;
use pingers_transport::TokioTimer;
use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, SeedableRng as _};

use crate::cache::{Cache, CacheParts, PackCutter, PmtilesCutter};
use crate::config::{Logging, MapHostConfig};
use crate::counts::{CountsSnapshot, report_counts};
use crate::error::MapHostError;
use crate::files::Directory;
use crate::http::Host;
use crate::region::{RegionEntry, parse_region_list};

/// Reads and checks the configuration file at `path`.
///
/// # Errors
///
/// [`MapHostError::ReadFile`] or [`MapHostError::Config`].
pub fn load_config(path: &Path) -> Result<MapHostConfig, MapHostError> {
    let text = std::fs::read_to_string(path).map_err(|source| MapHostError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    MapHostConfig::parse(&text, base).map_err(MapHostError::Config)
}

/// Opens the listening socket named in the configuration.
///
/// # Errors
///
/// [`MapHostError::Listen`] if the address cannot be bound.
pub async fn listen(config: &MapHostConfig) -> Result<tokio::net::TcpListener, MapHostError> {
    tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|source| MapHostError::Listen {
            address: config.listen,
            source,
        })
}

/// Serves `config.directory` on `listener` until `shutdown` completes. In cache mode, cuts use
/// [`PmtilesCutter`]. The binary passes [`pingers_time::SystemClock`]; tests pass a manual
/// clock. The gateway's randomness is seeded from the operating system. Must be called inside
/// a tokio runtime.
///
/// # Errors
///
/// [`MapHostError::Entropy`] if the system's random number generator cannot be read;
/// otherwise as [`serve_with`].
pub async fn serve(
    config: MapHostConfig,
    listener: tokio::net::TcpListener,
    clock: impl Clock + 'static,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), MapHostError> {
    serve_with(
        config,
        Outside {
            cutter: None,
            clock,
            rng: Box::new(ChaCha20Rng::from_seed(*os_seed()?)),
        },
        listener,
        shutdown,
    )
    .await
}

/// 32 bytes from the operating system's random number generator.
fn os_seed() -> Result<zeroize::Zeroizing<[u8; 32]>, MapHostError> {
    use std::io::Read as _;
    let mut seed = zeroize::Zeroizing::new([0_u8; 32]);
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(seed.as_mut_slice()))
        .map_err(MapHostError::Entropy)?;
    Ok(seed)
}

/// The host's view of the outside world, injected into [`serve_with`].
pub struct Outside<C> {
    /// Cuts regions in cache mode; `None` for [`PmtilesCutter`] with the configured tool and
    /// upstream. Unused in pre-extracted mode.
    pub cutter: Option<Arc<dyn PackCutter>>,
    /// Wall time for last-served and provenance records.
    pub clock: C,
    /// Randomness for the OHTTP gateway: a new key on first start, and response sealing.
    pub rng: Box<dyn CryptoRng + Send>,
}

/// As [`serve`], with an injected cutter: for tests that use a fake upstream.
///
/// # Errors
///
/// [`MapHostError::Directory`] or [`MapHostError::NotADirectory`] if the directory is not
/// usable, [`MapHostError::OhttpKeyFile`] or [`MapHostError::OhttpKey`] if the gateway key
/// cannot be read or made, [`MapHostError::ReadRegionList`] or [`MapHostError::RegionList`] in
/// cache mode, or [`MapHostError::Serve`] if the HTTP server fails.
pub async fn serve_with<C: Clock + 'static>(
    config: MapHostConfig,
    outside: Outside<C>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), MapHostError> {
    let root = tokio::fs::canonicalize(&config.directory)
        .await
        .map_err(|source| MapHostError::Directory {
            path: config.directory.clone(),
            source,
        })?;
    if !tokio::fs::metadata(&root)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(MapHostError::NotADirectory {
            path: config.directory,
        });
    }
    let directory = Directory::new(root);
    let mut rng = outside.rng;
    let key_file = config.ohttp_key_file.clone();
    let (key, rng) = tokio::task::spawn_blocking(move || {
        let key = gateway::load_or_create_key(&key_file, &mut *rng);
        (key, rng)
    })
    .await
    .map_err(|error| MapHostError::Serve(std::io::Error::other(error)))?;
    let gateway = Arc::new(gateway::RelayGateway::new(key?, rng));
    let counts = Arc::default();
    let (cache, retry_after_seconds) = if let Some(cache_config) = &config.cache {
        {
            let regions = load_regions(&config, &cache_config.regions).await?;
            let cutter = outside.cutter.unwrap_or_else(|| {
                Arc::new(PmtilesCutter {
                    pmtiles: cache_config.pmtiles.clone(),
                    upstream: cache_config.upstream.clone(),
                })
            });
            let parts = CacheParts {
                directory: directory.clone(),
                regions,
                upstream: cache_config.upstream.clone(),
                max_disk: config.max_disk,
                max_concurrent_extracts: cache_config.max_concurrent_extracts.get(),
                cutter,
                clock: Arc::new(outside.clock),
                counts: Arc::clone(&counts),
            };
            let cache = tokio::task::spawn_blocking(move || Cache::new(parts))
                .await
                .map_err(|error| MapHostError::Serve(std::io::Error::other(error)))?;
            (Some(Arc::new(cache)), cache_config.retry_after_seconds)
        }
    } else {
        let usage_directory = directory.clone();
        if let Ok(Ok(used)) = tokio::task::spawn_blocking(move || usage_directory.usage()).await
            && used > config.max_disk
        {
            warn_over_budget(used, config.max_disk);
        }
        (None, 0)
    };
    let host = Arc::new(Host {
        directory,
        allow: config.allow,
        counts,
        cache,
        retry_after_seconds,
        gateway,
    });
    let reporter = match config.logging {
        Logging::Off => None,
        Logging::Counts { interval } => {
            let host = Arc::clone(&host);
            let max_disk = config.max_disk;
            Some(tokio::spawn(async move {
                let timer = TokioTimer::new();
                report_counts(&host.counts, &timer, interval, |snapshot| {
                    write_counts(snapshot, host.directory.usage().ok(), max_disk);
                })
                .await;
            }))
        }
    };
    let served = axum::serve(listener, http::router(host))
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(MapHostError::Serve);
    if let Some(reporter) = reporter {
        reporter.abort();
    }
    served
}

/// The region list's allow-listed entries, with zoom overrides applied.
async fn load_regions(
    config: &MapHostConfig,
    path: &Path,
) -> Result<Vec<RegionEntry>, MapHostError> {
    let text =
        tokio::fs::read_to_string(path)
            .await
            .map_err(|source| MapHostError::ReadRegionList {
                path: path.to_owned(),
                source,
            })?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let entries = parse_region_list(&text, base).map_err(|source| MapHostError::RegionList {
        path: path.to_owned(),
        source,
    })?;
    Ok(entries
        .into_iter()
        .filter(|entry| config.allow.allows(&entry.id))
        .map(|mut entry| {
            if let Some(zoom) = config.allow.max_zoom(&entry.id) {
                entry.max_zoom = zoom;
            }
            entry
        })
        .collect())
}

#[expect(
    clippy::print_stderr,
    reason = "a one-line start-up warning; carries no request detail"
)]
fn warn_over_budget(used: u64, max_disk: u64) {
    eprintln!("pingers-map-host: warning: directory holds {used} bytes, over max_disk {max_disk}");
}

#[expect(
    clippy::print_stderr,
    reason = "counts logging is the host's only output, and only when the operator enables it"
)]
fn write_counts(snapshot: CountsSnapshot, used: Option<u64>, max_disk: u64) {
    match used {
        Some(used) => {
            eprintln!("pingers-map-host counts: {snapshot} disk={used}/{max_disk}");
        }
        None => eprintln!("pingers-map-host counts: {snapshot} disk=unknown/{max_disk}"),
    }
}
