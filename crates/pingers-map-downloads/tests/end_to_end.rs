// SPDX-License-Identifier: AGPL-3.0-only
//! Downloads from a real pingers-map-host on loopback, both ways the app can reach it:
//!
//! - **direct**: HTTPS (through a TLS front, as a deployment puts a reverse proxy in front of
//!   the host) with [`DirectFetch`];
//! - **through the relay**: the in-process Pingers server and OHTTP relay (pingers-testkit's
//!   harness), with the server forwarding to the map host's own gateway, with [`RelayFetch`].
//!
//! Each fetches the catalog, downloads a region of several pieces, and checks the installed
//! files byte for byte.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_map_downloads::catalog::{Catalog, HostFailure, fetch_catalog};
use pingers_map_downloads::fetch::{DirectFetch, KeyPinStore, MapFetch, RelayFetch, RelayParts};
use pingers_map_downloads::install::{Download, DownloadProgress, download, installed};
use pingers_map_downloads::{HostChain, MapsError};
use pingers_map_host::config::{Logging, MapHostConfig};
use pingers_map_host::region::{AllowList, RegionId};
use pingers_map_host::{Outside, serve_with};
use pingers_proto::{KeyConfigHash, MapHostUrl};
use pingers_testkit::{HARNESS_MAP_HOST, HarnessConfig, InProcessHarness, ManualClock, TlsFront};
use pingers_time::UnixSeconds;
use pingers_transport::{
    KeyPin, OhttpConfig, OhttpTransport, PublicClient, PublicConfig, Relay, TlsRoots,
    certificates_from_pem,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const ID: &str = "oceania/australia/victoria/melbourne-test";
const STEM: &str = "oceania.australia.victoria.melbourne-test";

/// A directory under the system temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> std::io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingers-map-downloads-e2e-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _stale = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// A pack of three relay pieces and a bit, and its index.
fn region_files() -> (Vec<u8>, Vec<u8>) {
    let pack: Vec<u8> = (0..800_000_u32)
        .map(|index| u8::try_from(index.wrapping_mul(7) % 251).unwrap_or(0))
        .collect();
    let index = b"# pingers-places 1\nmelbourne\tcity\t-37.81\t144.96\tMelbourne\n".to_vec();
    (pack, index)
}

/// Writes a pre-extracted region as pingers-tiles would.
fn write_region(directory: &Path) -> std::io::Result<()> {
    let (pack, index) = region_files();
    std::fs::write(directory.join(format!("{STEM}.pmtiles")), &pack)?;
    std::fs::write(directory.join(format!("{STEM}.places.tsv")), &index)?;
    let manifest = format!(
        r#"{{"format":1,"id":"{ID}","name":"Melbourne test","group":"Victoria",
           "bbox":[144.95,-37.825,144.98,-37.805],"max_zoom":15,"build_date":"2026-11-12",
           "source":"https://build.protomaps.com/20261112.pmtiles",
           "pack":{{"file":"{STEM}.pmtiles","size":{},"sha256":"{}"}},
           "index":{{"file":"{STEM}.places.tsv","size":{},"sha256":"{}","entries":1}}}}"#,
        pack.len(),
        pingers_crypto::content_digest(&pack).to_hex(),
        index.len(),
        pingers_crypto::content_digest(&index).to_hex(),
    );
    std::fs::write(directory.join(format!("{STEM}.json")), manifest)
}

/// A running map host; stops when dropped.
struct MapHost {
    address: SocketAddr,
    _stop: tokio::sync::oneshot::Sender<()>,
    _directory: TempDir,
}

async fn map_host() -> Result<MapHost, Box<dyn std::error::Error>> {
    let directory = TempDir::new("host")?;
    write_region(directory.path())?;
    let config = MapHostConfig {
        listen: SocketAddr::new(LOOPBACK, 0),
        directory: directory.path().to_owned(),
        max_disk: 1 << 30,
        allow: AllowList {
            hosted: RegionId::parse("oceania").into_iter().collect(),
            max_zoom: Vec::new(),
        },
        logging: Logging::Off,
        cache: None,
        ohttp_key_file: directory.path().join(".ohttp-key"),
    };
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_with(
        config,
        Outside {
            cutter: None,
            clock: ManualClock::new(UnixSeconds::new(1_800_000_000)),
            rng: Box::new(pingers_testkit::seeded_rng(3)),
        },
        listener,
        async move {
            let _ = stopped.await;
        },
    ));
    Ok(MapHost {
        address,
        _stop: stop,
        _directory: directory,
    })
}

/// Records progress; ignores preparing (pre-extracted hosts never prepare).
#[derive(Default)]
struct Progress(Mutex<Vec<(u64, u64)>>);

impl DownloadProgress for Progress {
    fn downloaded(&self, done: u64, total: u64) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((done, total));
    }

    fn preparing(&self, _retry_after: Duration) {}
}

/// Gateway key pins in memory.
#[derive(Default)]
struct Pins(Mutex<Vec<(String, KeyConfigHash)>>);

#[async_trait::async_trait]
impl KeyPinStore for Pins {
    async fn pinned(&self, host: &MapHostUrl) -> Result<Option<KeyConfigHash>, MapsError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(pinned, _)| pinned == host.as_str())
            .map(|(_, hash)| *hash))
    }

    async fn pin(&self, host: &MapHostUrl, hash: KeyConfigHash) -> Result<(), MapsError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((host.as_str().to_owned(), hash));
        Ok(())
    }
}

/// Fetches the catalog from `chain` and downloads its one region into a new directory, then
/// checks the installed files against the host's.
async fn catalog_and_download(
    fetch: &dyn MapFetch,
    chain: &HostChain,
    timer: &ManualClock,
) -> Result<Catalog, Box<dyn std::error::Error>> {
    let catalog = fetch_catalog(fetch, chain).await;
    let offer = catalog
        .offers
        .iter()
        .find(|offer| offer.entry.id() == ID)
        .ok_or("the region is not offered")?;
    let phone = TempDir::new("phone")?;
    let progress = Progress::default();
    let manifest = download(&Download {
        fetch,
        timer,
        offer,
        directory: phone.path(),
        progress: &progress,
    })
    .await?;
    let (pack, index) = region_files();
    assert_eq!(
        std::fs::read(phone.path().join(format!("{STEM}.pmtiles")))?,
        pack
    );
    assert_eq!(
        std::fs::read(phone.path().join(format!("{STEM}.places.tsv")))?,
        index
    );
    assert_eq!(installed(phone.path()).await?, vec![manifest.clone()]);
    let total = manifest.total_size();
    assert_eq!(
        progress
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last(),
        Some(&(total, total))
    );
    Ok(catalog)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_region_downloads_directly_over_https() -> TestResult {
    let host = map_host().await?;
    let front = TlsFront::start(LOOPBACK, host.address).await?;
    let clock = ManualClock::new(UnixSeconds::new(1_800_000_000));
    let client = PublicClient::new(
        &PublicConfig::new(
            TlsRoots::from_der(&certificates_from_pem(front.ca_pem())?)?,
            5 * 1024 * 1024,
        ),
        Arc::new(clock.clone()),
    )?;
    let fetch = DirectFetch::new(client);
    let url = MapHostUrl::new(format!("https://{}", front.address()))?;
    let catalog =
        catalog_and_download(&fetch, &HostChain::new(Some(&url), None, None), &clock).await?;
    assert!(catalog.failed.is_empty());
    Ok(())
}

/// The harness, with its server forwarding map requests to `host`'s gateway, and a relayed
/// route through its OHTTP relay.
fn relayed(
    host: &MapHost,
    pins: Arc<Pins>,
) -> Result<(InProcessHarness, RelayFetch, ManualClock), Box<dyn std::error::Error>> {
    let config = HarnessConfig {
        map_host_gateway: Some(format!("http://{}", host.address)),
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let relay = harness.endpoints().ohttp_relay;
    let transport = OhttpTransport::new(
        OhttpConfig::new(
            vec![Relay::new(
                &format!("http://{relay}/ohttp"),
                &format!("http://{relay}/v1/ohttp/config"),
            )?],
            KeyPin::FirstUse,
            TlsRoots::from_der(&[])?,
        ),
        Arc::new(clock.clone()),
        Box::new(pingers_testkit::seeded_rng(11)),
    )?;
    let fetch = RelayFetch::new(RelayParts {
        transport: Arc::new(transport),
        server_host: Some(MapHostUrl::new(HARNESS_MAP_HOST.to_owned())?),
        pins,
        rng: Box::new(pingers_testkit::seeded_rng(12)),
    });
    Ok((harness, fetch, clock))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_region_downloads_through_the_relay_and_the_server() -> TestResult {
    let host = map_host().await?;
    let pins = Arc::new(Pins::default());
    let (harness, fetch, clock) = relayed(&host, Arc::clone(&pins))?;
    let server_host = MapHostUrl::new(HARNESS_MAP_HOST.to_owned())?;
    let elsewhere = MapHostUrl::new("https://maps.elsewhere.invalid".to_owned())?;
    let catalog = catalog_and_download(
        &fetch,
        &HostChain::new(Some(&elsewhere), Some(&server_host), None),
        &clock,
    )
    .await?;
    // Only the server's own map host can be reached this way; the other is skipped unasked.
    assert_eq!(catalog.failed.len(), 1);
    assert_eq!(
        catalog.failed.first().map(|failed| failed.reason),
        Some(HostFailure::NotThroughRelay)
    );
    // The gateway key was pinned on first use.
    assert_eq!(
        pins.0.lock().unwrap_or_else(PoisonError::into_inner).len(),
        1
    );
    harness.shutdown();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_map_host_key_stops_relayed_downloads() -> TestResult {
    let host = map_host().await?;
    let pins = Arc::new(Pins::default());
    let server_host = MapHostUrl::new(HARNESS_MAP_HOST.to_owned())?;
    pins.pin(&server_host, KeyConfigHash::from_bytes([7; 32]))
        .await?;
    let (harness, fetch, _clock) = relayed(&host, pins)?;
    let catalog = fetch_catalog(&fetch, &HostChain::new(None, Some(&server_host), None)).await;
    assert!(catalog.offers.is_empty());
    assert_eq!(
        catalog.failed.first().map(|failed| failed.reason),
        Some(HostFailure::KeyChanged)
    );
    harness.shutdown();
    Ok(())
}
