// SPDX-License-Identifier: AGPL-3.0-only
//! A client on the real crates downloads a region through the privacy relay: it learns the
//! server's map host from `/v1/params`, fetches the catalog and the region sealed to that
//! host's own gateway through the harness relay and server, pins the gateway key in its store,
//! and lists the region as installed.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pingers_client::ports::{MapDownload, MapHosts, MapRoute};
use pingers_client::{Client, ClientConfig, Crates, Instance, Outside, Trust};
use pingers_map_downloads::catalog::HostFailure;
use pingers_map_downloads::install::DownloadProgress;
use pingers_map_host::config::{Logging, MapHostConfig};
use pingers_map_host::region::{AllowList, RegionId};
use pingers_proto::{MapHostUrl, ServerId};
use pingers_testkit::{
    FakePlatform, HARNESS_MAP_HOST, HarnessConfig, InProcessHarness, ManualClock, RecordingEvents,
};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STEM: &str = "oceania.client-test";

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-maps-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

fn pack() -> Vec<u8> {
    (0..300_000_u32)
        .map(|index| u8::try_from(index % 241).unwrap_or(0))
        .collect()
}

const INDEX: &[u8] = b"# pingers-places 1\ntown\tcity\t-37\t145\tTown\n";

fn write_region(directory: &Path) -> std::io::Result<()> {
    let pack = pack();
    std::fs::write(directory.join(format!("{STEM}.pmtiles")), &pack)?;
    std::fs::write(directory.join(format!("{STEM}.places.tsv")), INDEX)?;
    std::fs::write(
        directory.join(format!("{STEM}.json")),
        format!(
            r#"{{"format":1,"id":"oceania/client-test","name":"Client test",
               "bbox":[144.9,-37.9,145.1,-37.7],"max_zoom":14,"build_date":"2026-11-12",
               "pack":{{"file":"{STEM}.pmtiles","size":{},"sha256":"{}"}},
               "index":{{"file":"{STEM}.places.tsv","size":{},"sha256":"{}"}}}}"#,
            pack.len(),
            pingers_crypto::content_digest(&pack).to_hex(),
            INDEX.len(),
            pingers_crypto::content_digest(INDEX).to_hex(),
        ),
    )
}

/// The map host's gateway key, written before it starts, so its pin can be built in as a
/// release build's `PINGERS_MAP_HOST_KEY_PINS` is.
const MAP_HOST_KEY: [u8; 32] = [0x24; 32];

/// The map host, serving the region; its directory goes with it.
async fn map_host(
    directory: &TestDir,
) -> Result<(SocketAddr, tokio::sync::oneshot::Sender<()>), Box<dyn std::error::Error>> {
    write_region(&directory.0)?;
    std::fs::write(
        directory.0.join(".ohttp-key"),
        format!("{}\n", hex::encode(MAP_HOST_KEY)),
    )?;
    let config = MapHostConfig {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        directory: directory.0.clone(),
        max_disk: 1 << 30,
        allow: AllowList {
            hosted: RegionId::parse("oceania").into_iter().collect(),
            max_zoom: Vec::new(),
        },
        logging: Logging::Off,
        cache: None,
        ohttp_key_file: directory.0.join(".ohttp-key"),
    };
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(pingers_map_host::serve_with(
        config,
        pingers_map_host::Outside {
            cutter: None,
            clock: ManualClock::new(pingers_time::UnixSeconds::new(1_800_000_000)),
            rng: Box::new(ChaCha20Rng::seed_from_u64(5)),
        },
        listener,
        async move {
            let _ = stopped.await;
        },
    ));
    Ok((address, stop))
}

async fn open(
    harness: &InProcessHarness,
    clock: &ManualClock,
    database: PathBuf,
) -> Result<Client, Box<dyn std::error::Error>> {
    let platform = FakePlatform::new(clock.clone());
    platform
        .secure_store_put("pingers.database-key", &[6; 32])
        .map_err(|_locked| "secure store")?;
    let seeds = Arc::new(AtomicU64::new(500));
    let outside = Outside {
        prover_self_test: pingers_sync::SelfTestPlan::DESIGN,
        clock: Arc::new(clock.clone()),
        timer: Arc::new(clock.clone()),
        rng: Arc::new(move || {
            Box::new(ChaCha20Rng::seed_from_u64(
                seeds.fetch_add(1, Ordering::Relaxed),
            ))
        }),
    };
    let endpoints = harness.endpoints();
    let instance = Instance {
        server: ServerId::new(endpoints.server_id.clone())?,
        relay: format!("http://{}/ohttp", endpoints.ohttp_relay),
        push_relay: None,
        // Official builds pin their built-in server's gateway key at build time.
        gateway_pins: Some(pingers_ohttp::KeyPins::from(
            pingers_crypto::key_config_hash(&endpoints.ohttp_key_config),
        )),
        push_relay_pins: None,
        map_host_pins: Some(pingers_ohttp::KeyPins::from(
            pingers_ohttp::GatewayKey::from_bytes(1, zeroize::Zeroizing::new(MAP_HOST_KEY))?
                .config()
                .hash(),
        )),
        trust: Trust::from_pem(&endpoints.push_relay_ca)?,
        registration_code: None,
    };
    let crates_platform: Arc<dyn pingers_client::Platform> = Arc::new(platform.clone());
    let opened = instance.clone();
    Ok(Client::open_with(
        ClientConfig {
            database,
            instance: Some(instance),
        },
        Arc::new(platform),
        Arc::new(RecordingEvents::new()),
        move |database| Crates::real(database, crates_platform, outside, Some(&opened)),
    )
    .await?)
}

struct Quiet;

impl DownloadProgress for Quiet {
    fn downloaded(&self, _done: u64, _total: u64) {}
    fn preparing(&self, _retry_after: Duration) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_region_comes_through_the_relay_into_the_maps_directory() -> TestResult {
    let host_directory = TestDir::new("host")?;
    let (address, _stop) = map_host(&host_directory).await?;
    let config = HarnessConfig {
        map_host_gateway: Some(format!("http://{address}")),
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let phone = TestDir::new("phone")?;
    let client = open(&harness, &clock, phone.0.join("pingers.db")).await?;
    // Nothing has read the server's parameters yet: the catalog reads them, and with them its
    // map host.
    let elsewhere = MapHostUrl::new("https://maps.elsewhere.invalid".to_owned())?;
    let catalog = client
        .map_catalog(
            MapHosts {
                user_override: Some(elsewhere),
                publisher: None,
            },
            MapRoute::Relay,
        )
        .await?;
    assert_eq!(
        catalog
            .failed
            .iter()
            .map(|failed| failed.reason)
            .collect::<Vec<_>>(),
        [HostFailure::NotThroughRelay]
    );
    let offer = catalog.offers.first().ok_or("nothing offered")?.clone();
    assert_eq!(offer.host.url().as_str(), HARNESS_MAP_HOST);
    let maps = phone.0.join("maps");
    let manifest = client
        .download_map(
            MapDownload {
                offer,
                route: MapRoute::Relay,
                directory: maps.clone(),
            },
            Arc::new(Quiet),
        )
        .await?;
    assert_eq!(std::fs::read(maps.join(format!("{STEM}.pmtiles")))?, pack());
    assert_eq!(client.installed_maps(maps.clone()).await?, vec![manifest]);
    client
        .delete_map(maps.clone(), "oceania/client-test".to_owned())
        .await?;
    assert!(client.installed_maps(maps).await?.is_empty());
    harness.shutdown();
    Ok(())
}
