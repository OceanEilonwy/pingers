// SPDX-License-Identifier: AGPL-3.0-only
//! Cutting packs with the `pmtiles` command-line tool and publishing them: the pack, its search
//! index and its manifest, side by side in one directory.
//!
//! `pmtiles extract <build> <out> --region=<geojson> --maxzoom=<z>` fetches only the byte
//! ranges of the build that the region needs (a few requests to the build's host, made by the
//! operator, never by a phone); `--dry-run` does the same planning without the tiles and prints
//! the size the archive would have.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

use crate::index::{index_text, places_in};
use crate::manifest::{FORMAT, FileEntry, Manifest};
use crate::pmtiles::{Pack, PackError};
use crate::regions::Region;
use crate::size::ByteSize;

/// Why cutting or publishing failed.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// `pmtiles` could not be started.
    #[error("could not run {program}: {source}")]
    Spawn {
        /// The program.
        program: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// `pmtiles` exited with an error.
    #[error("pmtiles failed for {region}: {output}")]
    Failed {
        /// The region.
        region: String,
        /// The last lines it printed.
        output: String,
    },
    /// A dry run printed no size.
    #[error("pmtiles --dry-run printed no archive size for {0}")]
    NoEstimate(String),
    /// The pack could not be read.
    #[error("reading the pack: {0}")]
    Pack(#[from] PackError),
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The manifest could not be encoded.
    #[error("encoding the manifest: {0}")]
    Json(#[from] serde_json::Error),
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> ExtractError + '_ {
    move |source| ExtractError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Where packs are cut from, and with what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extractor {
    /// The `pmtiles` binary.
    pub pmtiles: PathBuf,
    /// The Protomaps build: a URL or a local file.
    pub build: String,
}

/// The files of one region in a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionFiles {
    /// `<stem>.pmtiles`.
    pub pack: PathBuf,
    /// `<stem>.places.tsv`.
    pub index: PathBuf,
    /// `<stem>.json`.
    pub manifest: PathBuf,
}

impl RegionFiles {
    /// The file names for `region` in `directory`.
    #[must_use]
    pub fn of(region: &Region, directory: &Path) -> Self {
        let stem = region.id.file_stem();
        Self {
            pack: directory.join(format!("{stem}.pmtiles")),
            index: directory.join(format!("{stem}.places.tsv")),
            manifest: directory.join(format!("{stem}.json")),
        }
    }
}

impl Extractor {
    /// The arguments of the extract for `region` into `out`.
    #[must_use]
    pub fn arguments(&self, region: &Region, out: &Path) -> Vec<OsString> {
        let mut region_flag = OsString::from("--region=");
        region_flag.push(&region.polygon);
        vec![
            "extract".into(),
            self.build.clone().into(),
            out.as_os_str().to_owned(),
            region_flag,
            format!("--maxzoom={}", region.max_zoom).into(),
        ]
    }

    /// The extract as a shell command line, for operators who run it elsewhere.
    #[must_use]
    pub fn command_line(&self, region: &Region, out: &Path) -> String {
        std::iter::once(self.pmtiles.as_os_str().to_owned())
            .chain(self.arguments(region, out))
            .map(|argument| shell_quote(&argument.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn run(&self, region: &Region, mut arguments: Vec<OsString>) -> Result<String, ExtractError> {
        arguments.push("--download-threads=2".into());
        let output = Command::new(&self.pmtiles)
            .args(&arguments)
            .output()
            .map_err(|source| ExtractError::Spawn {
                program: self.pmtiles.clone(),
                source,
            })?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if output.status.success() {
            Ok(text)
        } else {
            let tail: Vec<&str> = text.lines().rev().take(3).collect();
            Err(ExtractError::Failed {
                region: region.id.to_string(),
                output: tail.into_iter().rev().collect::<Vec<_>>().join(" / "),
            })
        }
    }

    /// The size the pack for `region` would have, from `pmtiles extract --dry-run`.
    ///
    /// # Errors
    ///
    /// [`ExtractError`] if `pmtiles` fails or prints no size.
    pub fn estimate(&self, region: &Region) -> Result<ByteSize, ExtractError> {
        let mut arguments = self.arguments(
            region,
            &std::env::temp_dir().join("pingers-dry-run.pmtiles"),
        );
        arguments.push("--dry-run".into());
        let output = self.run(region, arguments)?;
        estimate_in(&output).ok_or_else(|| ExtractError::NoEstimate(region.id.to_string()))
    }

    /// Cuts `region` into `out`, through a temporary file so a failed cut leaves nothing.
    ///
    /// # Errors
    ///
    /// [`ExtractError`] if `pmtiles` fails or the file cannot be moved into place.
    pub fn extract(&self, region: &Region, out: &Path) -> Result<(), ExtractError> {
        let mut partial = out.as_os_str().to_owned();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        let result = self.run(region, self.arguments(region, &partial));
        if let Err(error) = result {
            let _ignored = fs::remove_file(&partial);
            return Err(error);
        }
        fs::rename(&partial, out).map_err(io_error(out))
    }
}

/// The size in `pmtiles extract`'s "… for an archive size of 3.2 MB" line.
#[must_use]
pub fn estimate_in(output: &str) -> Option<ByteSize> {
    output.lines().rev().find_map(|line| {
        let (_, size) = line.split_once("archive size of ")?;
        ByteSize::parse(size.trim()).ok()
    })
}

fn shell_quote(text: &str) -> String {
    let plain = text
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=,@".contains(&byte));
    if plain && !text.is_empty() {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The lowercase hex SHA-256 and length of a file. A checksum of public map data, for the app
/// to verify downloads with; nothing secret goes through it, so it is not one of
/// pingers-crypto's keyed primitives (style guide §8 concerns those).
///
/// # Errors
///
/// [`ExtractError::Io`] if the file cannot be read.
pub fn sha256_file(path: &Path) -> Result<(String, u64), ExtractError> {
    let file = File::open(path).map_err(io_error(path))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    let mut length: u64 = 0;
    loop {
        let read = reader.read(&mut buffer).map_err(io_error(path))?;
        let Some(chunk) = buffer.get(..read) else {
            break;
        };
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
        length = length.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok((hex::encode(hasher.finalize()), length))
}

/// What [`publish`] needs besides the region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication<'a> {
    /// The directory holding the region's pack, where the index and manifest are written.
    pub directory: &'a Path,
    /// The build the pack was cut from.
    pub source: &'a str,
    /// Its date, `YYYY-MM-DD`.
    pub build_date: &'a str,
}

/// Writes the search index and the manifest for a region whose pack is already in
/// `publication.directory`, and returns the manifest.
///
/// # Errors
///
/// [`ExtractError`] if the pack cannot be read or a file cannot be written.
pub fn publish(region: &Region, publication: &Publication<'_>) -> Result<Manifest, ExtractError> {
    let files = RegionFiles::of(region, publication.directory);
    let mut pack = Pack::open(BufReader::new(
        File::open(&files.pack).map_err(io_error(&files.pack))?,
    ))?;
    let header = pack.header.clone();
    let places = places_in(&mut pack)?;
    fs::write(&files.index, index_text(&places)).map_err(io_error(&files.index))?;
    let (pack_sha, pack_size) = sha256_file(&files.pack)?;
    let (index_sha, index_size) = sha256_file(&files.index)?;
    let name_of = |path: &Path| {
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    };
    let manifest = Manifest {
        format: FORMAT,
        id: region.id.to_string(),
        name: region.name.clone(),
        group: region.group.clone(),
        bbox: [
            header.bounds.west,
            header.bounds.south,
            header.bounds.east,
            header.bounds.north,
        ],
        max_zoom: header.max_zoom,
        build_date: publication.build_date.to_owned(),
        source: publication.source.to_owned(),
        pack: FileEntry {
            file: name_of(&files.pack),
            size: pack_size,
            sha256: pack_sha,
            entries: None,
        },
        index: FileEntry {
            file: name_of(&files.index),
            size: index_size,
            sha256: index_sha,
            entries: Some(u64::try_from(places.len()).unwrap_or(u64::MAX)),
        },
    };
    let mut json = serde_json::to_string_pretty(&manifest)?;
    json.push('\n');
    fs::write(&files.manifest, json).map_err(io_error(&files.manifest))?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::regions::RegionId;

    fn region() -> Option<Region> {
        Some(Region {
            id: RegionId::new("oceania/australia/victoria/melbourne-cbd").ok()?,
            name: "Central Melbourne".into(),
            group: None,
            polygon: PathBuf::from("/srv/regions/melbourne cbd.geojson"),
            max_zoom: 15,
        })
    }

    #[test]
    fn commands_name_the_region_file_and_zoom() {
        let extractor = Extractor {
            pmtiles: "pmtiles".into(),
            build: "https://build.protomaps.com/20261112.pmtiles".into(),
        };
        let line =
            region().map(|region| extractor.command_line(&region, Path::new("/out/a.pmtiles")));
        assert_eq!(
            line.as_deref(),
            Some(
                "pmtiles extract https://build.protomaps.com/20261112.pmtiles /out/a.pmtiles '--region=/srv/regions/melbourne cbd.geojson' --maxzoom=15"
            )
        );
    }

    #[test]
    fn dry_run_sizes_are_read_from_the_last_report_line() {
        let output = "extract.go:450: fetching 42 tiles\nextract.go:612: Extract transferred 3.2 MB (overfetch 0.05) for an archive size of 3.2 MB\n";
        assert_eq!(estimate_in(output), Some(ByteSize::new(3_200_000)));
        assert_eq!(estimate_in("nothing"), None);
    }
}
