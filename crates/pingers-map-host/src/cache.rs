// SPDX-License-Identifier: AGPL-3.0-only
//! Cache mode: allow-listed regions are cut from an upstream planet pack on first request,
//! and the least recently served are evicted to stay within the disk budget.
//!
//! A request for a missing region file starts a cut (unless one is running) and is answered
//! 503 with `Retry-After`. At most `max_concurrent_extracts` cuts run at once; the rest wait.
//! After a cut, regions are evicted by last served time until the region files fit in
//! `max_disk`: never the region just cut or one being cut, and one being served only if
//! nothing else is left. Last-served times and provenance (upstream and cut time per region)
//! are kept in `cache-state.json` in the directory.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use pingers_time::Clock;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::counts::Counts;
use crate::files::Directory;
use crate::region::{RegionEntry, RegionFile, RegionId};

/// The host's state file in the directory.
pub const STATE_FILE: &str = "cache-state.json";

/// Last-served times are written to disk at most this often per region (seconds), so busy
/// regions do not cause a write per request.
const SAVE_EVERY: u64 = 3600;

/// Writes a region's files into the directory.
pub trait PackCutter: Send + Sync + 'static {
    /// Writes `<stem>.pmtiles`, `<stem>.places.tsv` and `<stem>.json` for `region` into
    /// `directory`, the manifest last. Blocking: the host runs it on a blocking thread.
    ///
    /// # Errors
    ///
    /// [`CutError`] if the region could not be cut; the host then removes whatever was written.
    fn cut(&self, region: &RegionEntry, directory: &Path) -> Result<(), CutError>;
}

/// Why a cut failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CutError {
    /// Reading or writing failed.
    #[error("cannot cut the region")]
    Io(#[source] io::Error),
    /// `pmtiles extract`, or building the index and manifest, failed.
    #[error("cannot cut the region")]
    Extract(#[source] pingers_map_packs::extract::ExtractError),
}

/// Where a cached region came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The upstream archive it was cut from.
    pub upstream: String,
    /// When, in Unix seconds from the host's clock.
    pub cut_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    last_served: BTreeMap<String, u64>,
    #[serde(default)]
    sources: BTreeMap<String, Source>,
}

#[derive(Debug, Default)]
struct State {
    persisted: Persisted,
    preparing: HashSet<RegionId>,
    serving: HashMap<RegionId, usize>,
    last_saved: HashMap<RegionId, u64>,
}

/// What a cache-mode host needs from outside.
pub struct CacheParts {
    /// The served directory.
    pub directory: Directory,
    /// Allow-listed regions from the region list, zoom overrides applied.
    pub regions: Vec<RegionEntry>,
    /// The upstream, recorded as provenance.
    pub upstream: String,
    /// Disk budget for region files.
    pub max_disk: u64,
    /// Concurrent cuts.
    pub max_concurrent_extracts: usize,
    /// Cuts regions.
    pub cutter: Arc<dyn PackCutter>,
    /// Time for last-served and provenance records.
    pub clock: Arc<dyn Clock>,
    /// Shared counters.
    pub counts: Arc<Counts>,
}

/// The cache-mode state.
pub struct Cache {
    directory: Directory,
    regions: BTreeMap<RegionId, RegionEntry>,
    upstream: String,
    max_disk: u64,
    cutter: Arc<dyn PackCutter>,
    clock: Arc<dyn Clock>,
    counts: Arc<Counts>,
    permits: Arc<Semaphore>,
    state: Mutex<State>,
    write_lock: Arc<Mutex<()>>,
}

impl Cache {
    /// Sets up the cache, reading `cache-state.json` if present (an unreadable one is ignored:
    /// it only orders evictions and records provenance). Blocking.
    #[must_use]
    pub fn new(parts: CacheParts) -> Self {
        let persisted = std::fs::read(parts.directory.root().join(STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            regions: parts
                .regions
                .into_iter()
                .map(|entry| (entry.id.clone(), entry))
                .collect(),
            directory: parts.directory,
            upstream: parts.upstream,
            max_disk: parts.max_disk,
            cutter: parts.cutter,
            clock: parts.clock,
            counts: parts.counts,
            permits: Arc::new(Semaphore::new(parts.max_concurrent_extracts)),
            state: Mutex::new(State {
                persisted,
                ..State::default()
            }),
            write_lock: Arc::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The listed, allow-listed regions.
    pub fn regions(&self) -> impl Iterator<Item = &RegionEntry> {
        self.regions.values()
    }

    /// Whether `region` can be served or cut here.
    #[must_use]
    pub fn lists(&self, region: &RegionId) -> bool {
        self.regions.contains_key(region)
    }

    /// Whether a cut of `region` is queued or running.
    #[must_use]
    pub fn is_preparing(&self, region: &RegionId) -> bool {
        self.lock().preparing.contains(region)
    }

    /// Where each cached region came from.
    #[must_use]
    pub fn sources(&self) -> BTreeMap<String, Source> {
        self.lock().persisted.sources.clone()
    }

    /// Queues a cut of `region` unless one is already queued or running. Must be called inside
    /// a tokio runtime.
    pub fn start_cut(self: &Arc<Self>, region: &RegionId) {
        let Some(entry) = self.regions.get(region).cloned() else {
            return;
        };
        if !self.lock().preparing.insert(region.clone()) {
            return;
        }
        let cache = Arc::clone(self);
        tokio::spawn(async move { cache.cut(entry).await });
    }

    async fn cut(self: Arc<Self>, entry: RegionEntry) {
        let Ok(permit) = Arc::clone(&self.permits).acquire_owned().await else {
            self.lock().preparing.remove(&entry.id);
            return;
        };
        let cutter = Arc::clone(&self.cutter);
        let root = self.directory.root().to_owned();
        let job = entry.clone();
        let outcome = tokio::task::spawn_blocking(move || cutter.cut(&job, &root)).await;
        drop(permit);
        let id = entry.id;
        if matches!(outcome, Ok(Ok(()))) {
            self.counts.record_cut(true);
            let now = self.clock.now().get();
            {
                let mut state = self.lock();
                state
                    .persisted
                    .last_served
                    .insert(id.as_str().to_owned(), now);
                state.persisted.sources.insert(
                    id.as_str().to_owned(),
                    Source {
                        upstream: self.upstream.clone(),
                        cut_at: now,
                    },
                );
            }
            self.evict(&id).await;
        } else {
            self.counts.record_cut(false);
            let directory = self.directory.clone();
            let failed = id.clone();
            // A partial cut must not be served; the next request tries again.
            let _removed =
                tokio::task::spawn_blocking(move || directory.remove_region(&failed)).await;
        }
        // Saved before the region counts as ready, so whoever is served it next finds its
        // provenance on disk.
        self.save().await;
        self.lock().preparing.remove(&id);
    }

    async fn evict(&self, just_cut: &RegionId) {
        let (mut protected, serving, last_served) = {
            let state = self.lock();
            (
                state.preparing.clone(),
                state
                    .serving
                    .iter()
                    .filter(|(_, count)| **count > 0)
                    .map(|(region, _)| region.clone())
                    .collect::<HashSet<_>>(),
                state.persisted.last_served.clone(),
            )
        };
        protected.insert(just_cut.clone());
        let directory = self.directory.clone();
        let budget = self.max_disk;
        let evicted = tokio::task::spawn_blocking(move || {
            let files = directory.region_files()?;
            let chosen = choose_evictions(&Eviction {
                files: &files,
                budget,
                last_served: &last_served,
                protected: &protected,
                serving: &serving,
            });
            let mut evicted = Vec::with_capacity(chosen.len());
            for region in chosen {
                directory.remove_region(&region)?;
                evicted.push(region);
            }
            io::Result::Ok(evicted)
        })
        .await;
        let Ok(Ok(evicted)) = evicted else {
            return;
        };
        let mut state = self.lock();
        for region in evicted {
            self.counts.record_eviction();
            state.persisted.last_served.remove(region.as_str());
            state.persisted.sources.remove(region.as_str());
        }
    }

    /// Notes that `region` is being served until the guard is dropped, and when.
    #[must_use]
    pub fn serving(self: &Arc<Self>, region: &RegionId) -> ServingGuard {
        let now = self.clock.now().get();
        let save = {
            let mut state = self.lock();
            let count = state.serving.entry(region.clone()).or_insert(0);
            *count = count.saturating_add(1);
            state
                .persisted
                .last_served
                .insert(region.as_str().to_owned(), now);
            let saved = state.last_saved.get(region).copied().unwrap_or(0);
            let due = now.saturating_sub(saved) >= SAVE_EVERY;
            if due {
                state.last_saved.insert(region.clone(), now);
            }
            due
        };
        if save {
            let cache = Arc::clone(self);
            tokio::spawn(async move { cache.save().await });
        }
        ServingGuard {
            cache: Arc::clone(self),
            region: region.clone(),
        }
    }

    /// Writes `cache-state.json` (via a temporary file and a rename). Failures are ignored:
    /// the state only orders evictions and records provenance.
    async fn save(&self) {
        let Ok(bytes) = serde_json::to_vec_pretty(&self.lock().persisted) else {
            return;
        };
        let root = self.directory.root().to_owned();
        let write_lock = Arc::clone(&self.write_lock);
        let _saved = tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().unwrap_or_else(PoisonError::into_inner);
            write_atomically(&root, STATE_FILE, &bytes)
        })
        .await;
    }
}

/// Keeps a region marked as being served; see [`Cache::serving`].
pub struct ServingGuard {
    cache: Arc<Cache>,
    region: RegionId,
}

impl Drop for ServingGuard {
    fn drop(&mut self) {
        let mut state = self.cache.lock();
        if let Some(count) = state.serving.get_mut(&self.region) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.serving.remove(&self.region);
            }
        }
    }
}

/// What [`choose_evictions`] decides from.
pub struct Eviction<'a> {
    /// Region files on disk and their sizes.
    pub files: &'a [(RegionFile, u64)],
    /// The disk budget for region files.
    pub budget: u64,
    /// Last served time per region id.
    pub last_served: &'a BTreeMap<String, u64>,
    /// Regions never to evict: the one just cut, and any being cut.
    pub protected: &'a HashSet<RegionId>,
    /// Regions being served right now: evicted only once nothing else is left.
    pub serving: &'a HashSet<RegionId>,
}

/// Which regions to delete so the region files fit in the budget: least recently served first
/// (never-served regions before all others), never a protected one, and one being served only
/// once nothing else is left.
#[must_use]
pub fn choose_evictions(eviction: &Eviction<'_>) -> Vec<RegionId> {
    let Eviction {
        files,
        budget,
        last_served,
        protected,
        serving,
    } = *eviction;
    let mut sizes: BTreeMap<&RegionId, u64> = BTreeMap::new();
    for (file, len) in files {
        let size = sizes.entry(&file.region).or_insert(0);
        *size = size.saturating_add(*len);
    }
    let mut total = sizes
        .values()
        .fold(0_u64, |sum, size| sum.saturating_add(*size));
    let mut candidates: Vec<(bool, u64, &RegionId, u64)> = sizes
        .iter()
        .filter(|(region, _)| !protected.contains(**region))
        .map(|(region, size)| {
            (
                serving.contains(*region),
                last_served.get(region.as_str()).copied().unwrap_or(0),
                *region,
                *size,
            )
        })
        .collect();
    candidates.sort();
    let mut chosen = Vec::new();
    for (_, _, region, size) in candidates {
        if total <= budget {
            break;
        }
        total = total.saturating_sub(size);
        chosen.push(region.clone());
    }
    chosen
}

fn write_atomically(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".{name}.tmp"));
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, directory.join(name))
}

/// The production cutter: `pmtiles extract` from the upstream archive, then the search index
/// and manifest, both through pingers-map-packs (the same code pingers-tiles runs).
#[derive(Clone, Debug)]
pub struct PmtilesCutter {
    /// The `pmtiles` tool.
    pub pmtiles: PathBuf,
    /// The upstream archive, a URL or a path.
    pub upstream: String,
}

impl PackCutter for PmtilesCutter {
    fn cut(&self, region: &RegionEntry, directory: &Path) -> Result<(), CutError> {
        use pingers_map_packs::extract::{Extractor, Publication, RegionFiles, publish};
        use pingers_map_packs::manifest::build_date_of;
        use pingers_map_packs::regions::{Region, RegionId as PacksRegionId};

        let id = PacksRegionId::new(region.id.as_str())
            .map_err(|error| CutError::Io(io::Error::other(error)))?;
        let region = Region {
            id,
            name: region.name.clone(),
            group: region.group.clone(),
            polygon: region.polygon.clone(),
            max_zoom: region.max_zoom,
        };
        let extractor = Extractor {
            pmtiles: self.pmtiles.clone(),
            build: self.upstream.clone(),
        };
        let files = RegionFiles::of(&region, directory);
        extractor
            .extract(&region, &files.pack)
            .map_err(CutError::Extract)?;
        let build_date = build_date_of(&self.upstream).unwrap_or_default();
        publish(
            &region,
            &Publication {
                directory,
                source: &self.upstream,
                build_date: &build_date,
            },
        )
        .map(|_manifest| ())
        .map_err(CutError::Extract)
    }
}

#[cfg(test)]
mod tests {
    use crate::region::FileKind;

    use super::*;

    fn ids(list: &[&str]) -> Vec<RegionId> {
        list.iter().copied().filter_map(RegionId::parse).collect()
    }

    fn set(list: &[&str]) -> HashSet<RegionId> {
        ids(list).into_iter().collect()
    }

    fn files(sizes: &[(&str, u64)]) -> Vec<(RegionFile, u64)> {
        sizes
            .iter()
            .filter_map(|(id, size)| {
                Some((
                    RegionFile {
                        region: RegionId::parse(id)?,
                        kind: FileKind::Pack,
                    },
                    *size,
                ))
            })
            .collect()
    }

    #[test]
    fn evicts_least_recently_served_until_within_budget() {
        let files = files(&[("a", 100), ("b", 100), ("c", 100), ("d", 100)]);
        let last_served = BTreeMap::from([
            ("a".to_owned(), 30),
            ("b".to_owned(), 10),
            ("c".to_owned(), 20),
            ("d".to_owned(), 40),
        ]);
        let chosen = choose_evictions(&Eviction {
            files: &files,
            budget: 200,
            last_served: &last_served,
            protected: &set(&["d"]),
            serving: &set(&[]),
        });
        assert_eq!(chosen, ids(&["b", "c"]));
    }

    #[test]
    fn serving_regions_go_last_and_protected_never() {
        let files = files(&[("a", 100), ("b", 100), ("c", 100)]);
        let last_served = BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 2)]);
        assert_eq!(
            choose_evictions(&Eviction {
                files: &files,
                budget: 150,
                last_served: &last_served,
                protected: &set(&["c"]),
                serving: &set(&["a"])
            }),
            ids(&["b", "a"])
        );
        assert_eq!(
            choose_evictions(&Eviction {
                files: &files,
                budget: 0,
                last_served: &last_served,
                protected: &set(&["a", "b", "c"]),
                serving: &set(&[])
            }),
            ids(&[])
        );
    }
}
