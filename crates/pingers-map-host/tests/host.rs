// SPDX-License-Identifier: AGPL-3.0-only
//! Integration tests: the host over HTTP on loopback, in pre-extracted and cache mode.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_map_host::cache::{CutError, PackCutter};
use pingers_map_host::config::{CacheConfig, Logging, MapHostConfig};
use pingers_map_host::files::etag_of;
use pingers_map_host::region::{AllowList, RegionEntry, RegionId};
use pingers_map_host::{Outside, serve_with};
use pingers_testkit::ManualClock;
use pingers_time::UnixSeconds;
use reqwest::StatusCode;
use reqwest::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderValue,
    IF_NONE_MATCH, IF_RANGE, RANGE, RETRY_AFTER, SET_COOKIE,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A directory under the system temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> std::io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingers-map-host-{label}-{}-{}",
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

fn ids(list: &[&str]) -> Vec<RegionId> {
    list.iter().copied().filter_map(RegionId::parse).collect()
}

fn config(directory: &Path, hosted: &[&str], max_disk: u64) -> MapHostConfig {
    MapHostConfig {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        directory: directory.to_owned(),
        max_disk,
        allow: AllowList {
            hosted: ids(hosted),
            max_zoom: Vec::new(),
        },
        logging: Logging::Off,
        cache: None,
    }
}

/// A running host; stops when dropped.
struct Running {
    address: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Running {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _sent = stop.send(());
        }
    }
}

async fn start(
    config: MapHostConfig,
    cutter: Option<Arc<dyn PackCutter>>,
    clock: ManualClock,
) -> Result<Running, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_with(
        config,
        Outside { cutter, clock },
        listener,
        async move {
            let _ = stopped.await;
        },
    ));
    Ok(Running {
        address,
        stop: Some(stop),
    })
}

fn clock() -> ManualClock {
    ManualClock::new(UnixSeconds::new(1_800_000_000))
}

/// Sends `request_line` as raw HTTP/1.1, so the path reaches the host without client-side
/// normalisation, and returns the status code.
async fn raw_status(
    address: SocketAddr,
    request_line: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream
        .write_all(format!("{request_line}\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes())
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let text = String::from_utf8_lossy(&response);
    Ok(text.split(' ').nth(1).unwrap_or_default().to_owned())
}

fn pack_bytes() -> Vec<u8> {
    (0..200_000_u32)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

fn write_pre_extracted(directory: &Path) -> std::io::Result<()> {
    std::fs::write(directory.join("oceania.pmtiles"), pack_bytes())?;
    std::fs::write(directory.join("oceania.places.tsv"), "# pingers-places 1\n")?;
    std::fs::write(
        directory.join("oceania.json"),
        r#"{"id":"oceania","name":"Oceania"}"#,
    )?;
    std::fs::write(
        directory.join("oceania.australia.json"),
        r#"{"id":"oceania/australia","name":"Australia"}"#,
    )?;
    std::fs::write(directory.join("europe.pmtiles"), b"not hosted")?;
    std::fs::write(
        directory.join("europe.json"),
        r#"{"id":"europe","name":"Europe"}"#,
    )?;
    std::fs::write(directory.join("broken.json"), "not json")?;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario against one running host"
)]
async fn pre_extracted_files_are_served_with_ranges_and_etags() -> TestResult {
    let directory = TempDir::new("files")?;
    write_pre_extracted(directory.path())?;
    let host = start(
        config(directory.path(), &["oceania", "broken"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let client = reqwest::Client::new();
    let pack = pack_bytes();
    let etag = etag_of(&pack);
    let url = host.url("/oceania.pmtiles");

    let full = client.get(&url).send().await?;
    assert_eq!(full.status(), StatusCode::OK);
    let headers = full.headers().clone();
    assert_eq!(
        headers.get(ETAG).map(HeaderValue::as_bytes),
        Some(etag.as_bytes())
    );
    assert_eq!(
        headers.get(CONTENT_TYPE).map(HeaderValue::as_bytes),
        Some(&b"application/vnd.pmtiles"[..])
    );
    assert_eq!(
        headers.get(CACHE_CONTROL).map(HeaderValue::as_bytes),
        Some(&b"public, max-age=31536000, immutable"[..])
    );
    assert_eq!(
        headers.get(ACCEPT_RANGES).map(HeaderValue::as_bytes),
        Some(&b"bytes"[..])
    );
    assert!(headers.get(SET_COOKIE).is_none());
    assert_eq!(full.bytes().await?.as_ref(), pack.as_slice());

    let head = client.head(&url).send().await?;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        head.headers()
            .get(CONTENT_LENGTH)
            .map(HeaderValue::as_bytes),
        Some(pack.len().to_string().as_bytes())
    );
    assert!(head.bytes().await?.is_empty());

    let part = client.get(&url).header(RANGE, "bytes=10-19").send().await?;
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        part.headers().get(CONTENT_RANGE).map(HeaderValue::as_bytes),
        Some(format!("bytes 10-19/{}", pack.len()).as_bytes())
    );
    assert_eq!(
        part.bytes().await?.as_ref(),
        pack.get(10..20).unwrap_or_default()
    );

    let suffix = client.get(&url).header(RANGE, "bytes=-5").send().await?;
    assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        suffix.bytes().await?.as_ref(),
        pack.get(pack.len().saturating_sub(5)..).unwrap_or_default()
    );

    let beyond = client
        .get(&url)
        .header(RANGE, "bytes=999999-")
        .send()
        .await?;
    assert_eq!(beyond.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        beyond
            .headers()
            .get(CONTENT_RANGE)
            .map(HeaderValue::as_bytes),
        Some(format!("bytes */{}", pack.len()).as_bytes())
    );

    let cached = client.get(&url).header(IF_NONE_MATCH, &etag).send().await?;
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        cached.headers().get(ETAG).map(HeaderValue::as_bytes),
        Some(etag.as_bytes())
    );

    let stale = client
        .get(&url)
        .header(RANGE, "bytes=0-0")
        .header(IF_RANGE, "\"other\"")
        .send()
        .await?;
    assert_eq!(stale.status(), StatusCode::OK);
    let fresh = client
        .get(&url)
        .header(RANGE, "bytes=0-0")
        .header(IF_RANGE, &etag)
        .send()
        .await?;
    assert_eq!(fresh.status(), StatusCode::PARTIAL_CONTENT);

    let manifest = client.get(host.url("/oceania.json")).send().await?;
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(
        manifest
            .headers()
            .get(CACHE_CONTROL)
            .map(HeaderValue::as_bytes),
        Some(&b"public, max-age=300"[..])
    );
    assert_eq!(
        manifest
            .headers()
            .get(CONTENT_TYPE)
            .map(HeaderValue::as_bytes),
        Some(&b"application/json"[..])
    );

    let index = client.get(host.url("/oceania.places.tsv")).send().await?;
    assert_eq!(
        index.headers().get(CONTENT_TYPE).map(HeaderValue::as_bytes),
        Some(&b"text/tab-separated-values; charset=utf-8"[..])
    );
    Ok(())
}

#[tokio::test]
async fn catalog_lists_only_allow_listed_manifests() -> TestResult {
    let directory = TempDir::new("catalog")?;
    write_pre_extracted(directory.path())?;
    let host = start(
        config(directory.path(), &["oceania", "broken"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let response = reqwest::get(host.url("/catalog.json")).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = response.json().await?;
    let listed: Vec<&str> = catalog
        .get("regions")
        .and_then(serde_json::Value::as_array)
        .map(|regions| {
            regions
                .iter()
                .filter_map(|region| region.get("id").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(catalog.get("format").cloned().unwrap_or_default(), 1);
    assert_eq!(listed, vec!["oceania", "oceania/australia"]);
    Ok(())
}

#[tokio::test]
async fn the_status_names_the_mode_and_hosted_prefixes_only() -> TestResult {
    let directory = TempDir::new("status")?;
    write_pre_extracted(directory.path())?;
    let host = start(
        config(directory.path(), &["oceania", "broken"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let response = reqwest::get(host.url("/status")).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(reqwest::header::HeaderValue::as_bytes),
        Some(&b"no-store"[..])
    );
    let status: serde_json::Value = response.json().await?;
    assert_eq!(
        status,
        serde_json::json!({
            "service": "pingers-map-host",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "ok",
            "mode": "pre-extracted",
            "hosted": ["oceania", "broken"],
        })
    );
    let head = reqwest::Client::new()
        .head(host.url("/status"))
        .send()
        .await?;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.bytes().await?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn other_paths_and_methods_are_refused() -> TestResult {
    let directory = TempDir::new("refused")?;
    write_pre_extracted(directory.path())?;
    let outside = TempDir::new("outside")?;
    std::fs::write(outside.path().join("secret.json"), "{}")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.json"),
        directory.path().join("oceania.nz.json"),
    )?;
    std::fs::create_dir(directory.path().join("oceania.sub.json"))?;
    let host = start(
        config(directory.path(), &["oceania"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let client = reqwest::Client::new();
    for path in [
        "/",
        "/europe.pmtiles",
        "/europe.json",
        "/oceania.txt",
        "/oceania.nz.json",
        "/oceania.sub.json",
    ] {
        let response = client.get(host.url(path)).send().await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(response.bytes().await?.is_empty(), "{path}");
    }
    for request in [
        "GET /../oceania.json HTTP/1.1",
        "GET /%2e%2e/etc/passwd HTTP/1.1",
        "GET /./oceania.json HTTP/1.1",
        "GET //oceania.json HTTP/1.1",
        "GET /sub/oceania.json HTTP/1.1",
        "GET /.oceania.json HTTP/1.1",
        "GET /oceania%2Ejson HTTP/1.1",
    ] {
        assert_eq!(raw_status(host.address, request).await?, "404", "{request}");
    }
    let post = client.post(host.url("/oceania.json")).send().await?;
    assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

/// Writes small region files, slowly, counting how many cuts overlap.
#[derive(Default)]
struct FakeCutter {
    pack_size: usize,
    delay: Duration,
    active: AtomicUsize,
    max_active: AtomicUsize,
    calls: Mutex<Vec<String>>,
}

impl FakeCutter {
    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn dyn_cutter(cutter: &Arc<FakeCutter>) -> Arc<dyn PackCutter> {
    let cutter: Arc<FakeCutter> = Arc::clone(cutter);
    let cutter: Arc<dyn PackCutter> = cutter;
    cutter
}

impl PackCutter for FakeCutter {
    fn cut(&self, region: &RegionEntry, directory: &Path) -> Result<(), CutError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        self.max_active.fetch_max(now, Ordering::SeqCst);
        #[expect(
            clippy::disallowed_methods,
            reason = "a fake upstream that takes real time, so concurrent cuts overlap"
        )]
        std::thread::sleep(self.delay);
        let stem = region.id.stem();
        let written = (|| {
            std::fs::write(
                directory.join(format!("{stem}.pmtiles")),
                vec![b'p'; self.pack_size],
            )?;
            std::fs::write(
                directory.join(format!("{stem}.places.tsv")),
                "# pingers-places 1\n",
            )?;
            std::fs::write(
                directory.join(format!("{stem}.json")),
                format!(
                    r#"{{"id":"{}","name":"{}","max_zoom":{}}}"#,
                    region.id, region.name, region.max_zoom
                ),
            )
        })();
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(region.id.to_string());
        self.active.fetch_sub(1, Ordering::SeqCst);
        written.map_err(CutError::Io)
    }
}

const REGION_LIST: &str = r#"
[[region]]
id = "oceania/australia"
name = "Australia"
polygon = "au.geojson"
max_zoom = 14

[[region]]
id = "oceania/australia/victoria"
name = "Victoria"
polygon = "vic.geojson"
max_zoom = 15

[[region]]
id = "oceania/nz"
name = "New Zealand"
polygon = "nz.geojson"
max_zoom = 15

[[region]]
id = "europe/france"
name = "France"
polygon = "fr.geojson"
max_zoom = 15
"#;

fn cache_config(
    directory: &Path,
    lists: &Path,
    max_disk: u64,
    concurrent: usize,
) -> Result<MapHostConfig, Box<dyn std::error::Error>> {
    std::fs::write(lists.join("regions.toml"), REGION_LIST)?;
    let mut config = config(directory, &["oceania"], max_disk);
    config.allow.max_zoom = vec![(RegionId::parse("oceania/australia").ok_or("id")?, 12)];
    config.cache = Some(CacheConfig {
        regions: lists.join("regions.toml"),
        upstream: "https://upstream.example/planet.pmtiles".to_owned(),
        pmtiles: PathBuf::from("pmtiles"),
        max_concurrent_extracts: NonZeroUsize::new(concurrent).ok_or("zero")?,
        retry_after_seconds: 7,
    });
    Ok(config)
}

/// Fetches `path` until it is not 503, for up to five seconds.
async fn fetch_when_ready(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    for _ in 0..500 {
        let response = client.get(url).send().await?;
        if response.status() != StatusCode::SERVICE_UNAVAILABLE {
            return Ok(response);
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "waits for a cut running on a real blocking thread"
        )]
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("still preparing".into())
}

fn region_bytes(directory: &Path) -> std::io::Result<u64> {
    let mut total: u64 = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && name != "cache-state.json" {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario against one running host"
)]
async fn cache_mode_cuts_on_demand() -> TestResult {
    let directory = TempDir::new("cache")?;
    let lists = TempDir::new("lists")?;
    let cutter = Arc::new(FakeCutter {
        pack_size: 100,
        delay: Duration::from_millis(50),
        ..FakeCutter::default()
    });
    let config = cache_config(directory.path(), lists.path(), 1 << 30, 1)?;
    let host = start(config, Some(dyn_cutter(&cutter)), clock()).await?;
    let client = reqwest::Client::new();

    let catalog: serde_json::Value = client
        .get(host.url("/catalog.json"))
        .send()
        .await?
        .json()
        .await?;
    let regions = catalog
        .get("regions")
        .cloned()
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(regions.len(), 3);
    assert!(
        regions
            .iter()
            .all(|region| region.get("status").cloned().unwrap_or_default() == "on_demand")
    );
    assert_eq!(
        regions
            .get(1)
            .map(|region| region.get("group").cloned().unwrap_or_default().clone()),
        Some("Australia".into())
    );

    let url = host.url("/oceania.australia.pmtiles");
    let first = client.get(&url).send().await?;
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        first.headers().get(RETRY_AFTER).map(HeaderValue::as_bytes),
        Some(&b"7"[..])
    );
    let body: serde_json::Value = first.json().await?;
    assert_eq!(
        body,
        serde_json::json!({"status": "preparing", "retry_after": 7})
    );

    let ready = fetch_when_ready(&client, &url).await?;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(ready.bytes().await?.len(), 100);
    assert_eq!(cutter.calls(), vec!["oceania/australia"]);

    let manifest: serde_json::Value = client
        .get(host.url("/oceania.australia.json"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        manifest.get("max_zoom").cloned().unwrap_or_default(),
        12,
        "the max_zoom override applies to cuts"
    );

    let catalog: serde_json::Value = client
        .get(host.url("/catalog.json"))
        .send()
        .await?
        .json()
        .await?;
    let australia = catalog
        .get("regions")
        .cloned()
        .unwrap_or_default()
        .as_array()
        .and_then(|regions| regions.first().cloned())
        .unwrap_or_default();
    assert_eq!(
        australia.get("name").cloned().unwrap_or_default(),
        "Australia"
    );
    assert!(australia.get("status").is_none());

    for path in [
        "/europe.france.pmtiles",
        "/europe.france.json",
        "/oceania.fiji.pmtiles",
    ] {
        assert_eq!(
            client.get(host.url(path)).send().await?.status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(cutter.calls(), vec!["oceania/australia"]);

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("cache-state.json"))?)?;
    assert_eq!(
        state
            .pointer("/sources/oceania~1australia/upstream")
            .cloned()
            .unwrap_or_default(),
        "https://upstream.example/planet.pmtiles"
    );
    assert_eq!(
        state
            .pointer("/sources/oceania~1australia/cut_at")
            .cloned()
            .unwrap_or_default(),
        1_800_000_000_u64
    );
    Ok(())
}

#[tokio::test]
async fn cache_mode_evicts_least_recently_served() -> TestResult {
    let directory = TempDir::new("evict")?;
    let lists = TempDir::new("evict-lists")?;
    let cutter = Arc::new(FakeCutter {
        pack_size: 1000,
        ..FakeCutter::default()
    });
    // Each region is about 1.1 kB; two fit, three do not.
    let config = cache_config(directory.path(), lists.path(), 2500, 1)?;
    let clock = clock();
    let host = start(config, Some(dyn_cutter(&cutter)), clock.clone()).await?;
    let client = reqwest::Client::new();
    let pack = |stem: &str| directory.path().join(format!("{stem}.pmtiles"));

    fetch_when_ready(&client, &host.url("/oceania.australia.pmtiles"))
        .await?
        .bytes()
        .await?;
    clock.advance(Duration::from_secs(10));
    fetch_when_ready(&client, &host.url("/oceania.nz.pmtiles"))
        .await?
        .bytes()
        .await?;
    clock.advance(Duration::from_secs(10));
    // Australia is now more recently served than New Zealand.
    client
        .get(host.url("/oceania.australia.json"))
        .send()
        .await?
        .bytes()
        .await?;
    clock.advance(Duration::from_secs(10));
    fetch_when_ready(&client, &host.url("/oceania.australia.victoria.pmtiles"))
        .await?
        .bytes()
        .await?;

    assert!(pack("oceania.australia").exists());
    assert!(pack("oceania.australia.victoria").exists());
    assert!(!pack("oceania.nz").exists());
    assert!(!directory.path().join("oceania.nz.json").exists());
    assert!(region_bytes(directory.path())? <= 2500);

    // An evicted region is cut again on request.
    let again = client.get(host.url("/oceania.nz.pmtiles")).send().await?;
    assert_eq!(again.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn cache_mode_caps_concurrent_cuts() -> TestResult {
    let directory = TempDir::new("cap")?;
    let lists = TempDir::new("cap-lists")?;
    let cutter = Arc::new(FakeCutter {
        pack_size: 10,
        delay: Duration::from_millis(100),
        ..FakeCutter::default()
    });
    let config = cache_config(directory.path(), lists.path(), 1 << 30, 1)?;
    let host = start(config, Some(dyn_cutter(&cutter)), clock()).await?;
    let client = reqwest::Client::new();
    let urls = [
        "/oceania.australia.pmtiles",
        "/oceania.nz.pmtiles",
        "/oceania.australia.victoria.places.tsv",
    ]
    .map(|path| host.url(path));
    for url in &urls {
        assert_eq!(
            client.get(url).send().await?.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    for url in &urls {
        assert_eq!(
            fetch_when_ready(&client, url).await?.status(),
            StatusCode::OK
        );
    }
    assert_eq!(cutter.max_active.load(Ordering::SeqCst), 1);
    let mut calls = cutter.calls();
    calls.sort();
    assert_eq!(
        calls,
        vec![
            "oceania/australia",
            "oceania/australia/victoria",
            "oceania/nz"
        ]
    );
    Ok(())
}
