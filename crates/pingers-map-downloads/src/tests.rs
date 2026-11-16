// SPDX-License-Identifier: AGPL-3.0-only
//! Downloads against a scripted host in memory: resuming, checking, waiting while a region is
//! prepared, padding relayed downloads, and removing regions. The end-to-end tests against a
//! real map host, direct and through the relay, are in `tests/`.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use pingers_ohttp::{BinaryResponse, Header, StatusCode};
use pingers_proto::MapHostUrl;
use pingers_time::{MonotonicTime, Sleep, Timer};

use crate::catalog::{RegionOffer, fetch_catalog};
use crate::fetch::{MapFetch, Padding, PieceSize, padded_count};
use crate::install::{Download, DownloadProgress, delete, download, installed};
use crate::manifest::tests::manifest_json;
use crate::{CatalogEntry, HostChain, MapHost, MapHostSource, MapsError, RegionManifest, StemName};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How a scripted host gets a ranged answer wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lie {
    /// The piece starts one byte later than asked.
    WrongStart,
    /// The body is one byte shorter than its Content-Range says.
    ShortBody,
    /// The range ends at the file's length, past its last byte.
    EndPastTotal,
}

/// A map host in memory, answering ranges like pingers-map-host.
struct ScriptedHost {
    files: Mutex<HashMap<String, Vec<u8>>>,
    /// How many more times a request answers 503.
    preparing: AtomicUsize,
    piece: PieceSize,
    requests: Mutex<Vec<String>>,
    lie: Mutex<Option<Lie>>,
    /// After this many requests, this file takes these bytes (a new version).
    change: Mutex<Option<(usize, String, Vec<u8>)>>,
}

impl ScriptedHost {
    fn new(piece: u64, padding: Padding) -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            preparing: AtomicUsize::new(0),
            piece: PieceSize {
                bytes: piece,
                padding,
            },
            requests: Mutex::new(Vec::new()),
            lie: Mutex::new(None),
            change: Mutex::new(None),
        }
    }

    fn lie(&self, lie: Lie) {
        *self.lie.lock().unwrap_or_else(PoisonError::into_inner) = Some(lie);
    }

    fn change_after(&self, requests: usize, name: &str, bytes: &[u8]) {
        *self.change.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((requests, format!("/{name}"), bytes.to_vec()));
    }

    fn put(&self, name: &str, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(format!("/{name}"), bytes.to_vec());
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn answer(
    status: u16,
    headers: Vec<(&str, String)>,
    body: Vec<u8>,
) -> Result<BinaryResponse, MapsError> {
    Ok(BinaryResponse {
        status: StatusCode::new(status)?,
        headers: headers
            .into_iter()
            .map(|(name, value)| Header {
                name: name.to_owned(),
                value: value.into_bytes(),
            })
            .collect(),
        body,
    })
}

#[async_trait::async_trait]
impl MapFetch for ScriptedHost {
    async fn get(
        &self,
        _host: &MapHost,
        path: &str,
        headers: Vec<Header>,
    ) -> Result<BinaryResponse, MapsError> {
        let range = headers
            .iter()
            .find(|header| header.name == "range")
            .and_then(|header| String::from_utf8(header.value.clone()).ok());
        let made = {
            let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
            requests.push(format!("{path} {}", range.clone().unwrap_or_default()));
            requests.len()
        };
        if made > MOST_REQUESTS {
            // A download that never stops asking fails here instead of hanging the test.
            return Err(MapsError::Internal);
        }
        let due = self
            .change
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_if(|(after, _, _)| made > *after);
        if let Some((_, name, bytes)) = due {
            self.files
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name, bytes);
        }
        let if_range = headers
            .iter()
            .find(|header| header.name == "if-range")
            .map(|header| header.value.clone());
        if self
            .preparing
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return answer(503, vec![("retry-after", "7".to_owned())], Vec::new());
        }
        let Some(file) = self
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .cloned()
        else {
            return answer(404, Vec::new(), Vec::new());
        };
        let etag = format!("\"{}\"", pingers_crypto::content_digest(&file).to_hex());
        let stale = if_range.is_some_and(|tag| tag != etag.as_bytes());
        let Some((start, end)) = range.as_deref().and_then(|range| {
            let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
            Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
        }) else {
            return answer(200, vec![("etag", etag)], file);
        };
        if stale {
            // If-Range names another version: the whole current one, as RFC 9110 says.
            return answer(200, vec![("etag", etag)], file);
        }
        let end = end.min(file.len().saturating_sub(1));
        let mut body = file.get(start..=end).unwrap_or_default().to_vec();
        let (mut first, mut last) = (start, end);
        match *self.lie.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(Lie::WrongStart) => {
                first = first.saturating_add(1);
                last = last.saturating_add(1);
            }
            Some(Lie::ShortBody) => {
                body.pop();
            }
            Some(Lie::EndPastTotal) => {
                last = file.len();
                body.push(0);
            }
            None => {}
        }
        answer(
            206,
            vec![
                ("etag", etag),
                (
                    "content-range",
                    format!("bytes {first}-{last}/{}", file.len()),
                ),
            ],
            body,
        )
    }

    fn piece(&self) -> PieceSize {
        self.piece
    }
}

/// A timer whose sleeps end at once, recording how long they were.
#[derive(Default)]
struct InstantTimer {
    slept: Mutex<Vec<Duration>>,
}

impl Timer for InstantTimer {
    fn now(&self) -> MonotonicTime {
        MonotonicTime::ORIGIN
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        self.slept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(deadline.saturating_duration_since(MonotonicTime::ORIGIN));
        Box::pin(std::future::ready(()))
    }
}

/// Records what a download reported.
#[derive(Default)]
struct Reports {
    downloaded: Mutex<Vec<(u64, u64)>>,
    preparing: Mutex<Vec<Duration>>,
}

impl DownloadProgress for Reports {
    fn downloaded(&self, done: u64, total: u64) {
        self.downloaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((done, total));
    }

    fn preparing(&self, retry_after: Duration) {
        self.preparing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(retry_after);
    }
}

/// A directory under the system temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingers-map-downloads-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _stale = std::fs::remove_dir_all(&path);
        Self(path)
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

const ID: &str = "oceania/test";
const PACK: &[u8] = b"pack bytes of a small region, enough for several pieces";
const INDEX: &[u8] = b"# pingers-places 1\nkey\tcity\t1\t2\tName\n";

fn map_host() -> Result<MapHost, MapsError> {
    Ok(MapHost::new(
        &MapHostUrl::new("https://maps.example".to_owned()).map_err(|_| MapsError::BadAnswer)?,
        MapHostSource::Server,
    ))
}

fn scripted(padding: Padding) -> ScriptedHost {
    let scripted = ScriptedHost::new(10, padding);
    scripted.put("oceania.test.pmtiles", PACK);
    scripted.put("oceania.test.places.tsv", INDEX);
    scripted.put(
        "oceania.test.json",
        manifest_json(ID, PACK, INDEX).as_bytes(),
    );
    scripted
}

fn ready_offer() -> Result<RegionOffer, MapsError> {
    Ok(RegionOffer {
        entry: CatalogEntry::Ready(Box::new(RegionManifest::from_json(
            manifest_json(ID, PACK, INDEX).as_bytes(),
        )?)),
        host: map_host()?,
    })
}

async fn run(
    fetch: &dyn MapFetch,
    offer: &RegionOffer,
    directory: &Path,
) -> Result<(RegionManifest, Reports, InstantTimer), MapsError> {
    let reports = Reports::default();
    let timer = InstantTimer::default();
    let manifest = bounded(download(&Download {
        fetch,
        timer: &timer,
        offer,
        directory,
        progress: &reports,
    }))
    .await??;
    Ok((manifest, reports, timer))
}

/// More requests than any download in these tests makes; past it the scripted host fails.
const MOST_REQUESTS: usize = 100_000;

/// Runs `work` for at most a minute (each test takes milliseconds), so a download stuck in a
/// loop fails instead of hanging.
async fn bounded<T>(work: impl Future<Output = T>) -> Result<T, MapsError> {
    tokio::time::timeout(Duration::from_secs(60), work)
        .await
        .map_err(|_| MapsError::Internal)
}

#[tokio::test]
async fn a_region_installs_in_pieces_with_its_manifest_last() -> TestResult {
    let directory = TempDir::new("install");
    let host = scripted(Padding::Off);
    let offer = ready_offer()?;
    let (manifest, reports, _) = run(&host, &offer, directory.path()).await?;
    assert_eq!(
        std::fs::read(directory.path().join("oceania.test.pmtiles"))?,
        PACK
    );
    assert_eq!(
        std::fs::read(directory.path().join("oceania.test.places.tsv"))?,
        INDEX
    );
    assert_eq!(installed(directory.path()).await?, vec![manifest.clone()]);
    // Index first, then the pack, in pieces of ten bytes.
    let requests = host.requests();
    assert_eq!(
        requests.first().map(String::as_str),
        Some("/oceania.test.places.tsv bytes=0-9")
    );
    assert_eq!(
        requests.last().map(String::as_str),
        Some("/oceania.test.pmtiles bytes=50-59")
    );
    let pieces = INDEX.len().div_ceil(10) + PACK.len().div_ceil(10);
    assert_eq!(requests.len(), pieces);
    let total = manifest.total_size();
    let reported = reports
        .downloaded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(reported.last(), Some(&(total, total)));
    assert!(reported.windows(2).all(|pair| match pair {
        [earlier, later] => earlier.0 <= later.0,
        _ => true,
    }));
    // No partial files are left behind.
    let leftovers: Vec<String> = std::fs::read_dir(directory.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    Ok(())
}

#[tokio::test]
async fn a_partial_download_resumes_where_it_stopped() -> TestResult {
    let directory = TempDir::new("resume");
    let host = scripted(Padding::Off);
    let offer = ready_offer()?;
    let CatalogEntry::Ready(manifest) = &offer.entry else {
        return Err("ready".into());
    };
    std::fs::create_dir_all(directory.path())?;
    let partial = crate::pieces::partial_path(directory.path(), &manifest.pack);
    std::fs::write(&partial, PACK.get(..30).unwrap_or_default())?;
    run(&host, &offer, directory.path()).await?;
    assert_eq!(
        std::fs::read(directory.path().join("oceania.test.pmtiles"))?,
        PACK
    );
    let pack_requests: Vec<String> = host
        .requests()
        .into_iter()
        .filter(|request| request.starts_with("/oceania.test.pmtiles"))
        .collect();
    assert_eq!(
        pack_requests,
        [
            "/oceania.test.pmtiles bytes=30-39",
            "/oceania.test.pmtiles bytes=40-49",
            "/oceania.test.pmtiles bytes=50-59"
        ]
    );
    assert!(!partial.exists());
    Ok(())
}

#[tokio::test]
async fn a_file_that_does_not_match_its_checksum_is_not_installed() -> TestResult {
    let directory = TempDir::new("checksum");
    let host = scripted(Padding::Off);
    let mut tampered = PACK.to_vec();
    if let Some(byte) = tampered.get_mut(5) {
        *byte ^= 1;
    }
    host.put("oceania.test.pmtiles", &tampered);
    let offer = ready_offer()?;
    assert!(matches!(
        run(&host, &offer, directory.path()).await,
        Err(MapsError::Checksum)
    ));
    assert!(installed(directory.path()).await?.is_empty());
    assert!(!directory.path().join("oceania.test.pmtiles").exists());
    assert!(!directory.path().join("oceania.test.json").exists());
    // The partial is gone too, so the next attempt starts clean.
    let partials = std::fs::read_dir(directory.path())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".pmtiles."))
        .count();
    assert_eq!(partials, 0);

    // A host whose file is not the length the manifest says is refused before checking.
    host.put("oceania.test.pmtiles", b"short");
    assert!(matches!(
        run(&host, &offer, directory.path()).await,
        Err(MapsError::Checksum)
    ));
    Ok(())
}

#[tokio::test]
async fn a_region_being_prepared_is_asked_for_again_as_the_host_says() -> TestResult {
    let directory = TempDir::new("preparing");
    let host = scripted(Padding::Off);
    host.preparing.store(2, Ordering::SeqCst);
    let offer = RegionOffer {
        entry: CatalogEntry::OnDemand {
            id: ID.to_owned(),
            stem: StemName::of_region(ID)?,
            name: "Test".to_owned(),
            group: None,
        },
        host: map_host()?,
    };
    let (manifest, reports, timer) = run(&host, &offer, directory.path()).await?;
    assert_eq!(manifest.id, ID);
    assert_eq!(
        *reports
            .preparing
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        [Duration::from_secs(7), Duration::from_secs(7)]
    );
    assert_eq!(
        *timer.slept.lock().unwrap_or_else(PoisonError::into_inner),
        [Duration::from_secs(7), Duration::from_secs(7)]
    );
    // The manifest came from the host first.
    assert!(
        host.requests()
            .iter()
            .any(|request| request.starts_with("/oceania.test.json"))
    );

    // A host that never finishes is given up on.
    host.preparing.store(usize::MAX, Ordering::SeqCst);
    let other = TempDir::new("preparing-forever");
    assert!(matches!(
        run(&host, &offer, other.path()).await,
        Err(MapsError::Preparing { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn an_on_demand_manifest_for_another_region_is_refused() -> TestResult {
    let directory = TempDir::new("other-manifest");
    let host = scripted(Padding::Off);
    host.put(
        "oceania.test.json",
        manifest_json("oceania/other", PACK, INDEX).as_bytes(),
    );
    let offer = RegionOffer {
        entry: CatalogEntry::OnDemand {
            id: ID.to_owned(),
            stem: StemName::of_region(ID)?,
            name: "Test".to_owned(),
            group: None,
        },
        host: map_host()?,
    };
    assert!(matches!(
        run(&host, &offer, directory.path()).await,
        Err(MapsError::BadAnswer)
    ));
    Ok(())
}

#[tokio::test]
async fn relayed_downloads_pad_their_requests_to_a_size_class() -> TestResult {
    let directory = TempDir::new("padded");
    let host = scripted(Padding::SizeClasses);
    run(&host, &ready_offer()?, directory.path()).await?;
    let pieces = u32::try_from(INDEX.len().div_ceil(10) + PACK.len().div_ceil(10))?;
    let requests = u32::try_from(host.requests().len())?;
    assert!(padded_count(pieces) > pieces, "the test must need padding");
    assert_eq!(requests, padded_count(pieces));
    // The extra requests ask for the pack's first piece, like any other piece.
    assert_eq!(
        host.requests().last().map(String::as_str),
        Some("/oceania.test.pmtiles bytes=0-9")
    );

    // Direct downloads are not padded.
    let direct = scripted(Padding::Off);
    let other = TempDir::new("unpadded");
    run(&direct, &ready_offer()?, other.path()).await?;
    assert_eq!(u32::try_from(direct.requests().len())?, pieces);
    Ok(())
}

#[tokio::test]
async fn deleting_a_region_removes_its_manifest_files_and_partials() -> TestResult {
    let directory = TempDir::new("delete");
    let host = scripted(Padding::Off);
    run(&host, &ready_offer()?, directory.path()).await?;
    std::fs::write(
        directory
            .path()
            .join("oceania.test.pmtiles.0123456789abcdef.partial"),
        b"old",
    )?;
    std::fs::write(directory.path().join("oceania.other.pmtiles"), b"keep")?;
    delete(directory.path(), ID).await?;
    assert!(installed(directory.path()).await?.is_empty());
    let left: Vec<String> = std::fs::read_dir(directory.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["oceania.other.pmtiles"]);
    assert!(delete(directory.path(), "../x").await.is_err());
    // Deleting what is not there is fine.
    delete(directory.path(), ID).await?;
    Ok(())
}

#[tokio::test]
async fn only_complete_regions_count_as_installed() -> TestResult {
    let directory = TempDir::new("installed");
    assert!(
        installed(directory.path()).await?.is_empty(),
        "no directory yet"
    );
    std::fs::create_dir_all(directory.path())?;
    let json = manifest_json(ID, PACK, INDEX);
    std::fs::write(directory.path().join("oceania.test.json"), &json)?;
    // A manifest without its whole pack is not installed.
    std::fs::write(directory.path().join("oceania.test.pmtiles"), b"part")?;
    assert!(installed(directory.path()).await?.is_empty());
    std::fs::write(directory.path().join("oceania.test.pmtiles"), PACK)?;
    assert_eq!(installed(directory.path()).await?.len(), 1);
    // A manifest under another region's name, or one that does not parse, is not either.
    std::fs::write(directory.path().join("oceania.misnamed.json"), &json)?;
    std::fs::write(directory.path().join("oceania.broken.json"), b"{")?;
    std::fs::write(directory.path().join("notes.txt"), b"x")?;
    assert_eq!(installed(directory.path()).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn catalogs_come_through_in_pieces_from_each_host() -> TestResult {
    let host = scripted(Padding::Off);
    let catalog = format!(
        r#"{{"format":1,"regions":[{}]}}"#,
        manifest_json(ID, PACK, INDEX)
    );
    host.put("catalog.json", catalog.as_bytes());
    let url = MapHostUrl::new("https://maps.example".to_owned())?;
    let result = fetch_catalog(&host, &HostChain::new(None, Some(&url), None)).await;
    assert_eq!(result.offers.len(), 1);
    assert!(result.failed.is_empty());
    assert_eq!(
        host.requests().len(),
        catalog.len().div_ceil(10),
        "ten-byte pieces"
    );

    let empty = ScriptedHost::new(10, Padding::Off);
    let result = fetch_catalog(&empty, &HostChain::new(None, Some(&url), None)).await;
    assert!(result.offers.is_empty());
    assert_eq!(result.failed.len(), 1);
    Ok(())
}

mod limits;
