// SPDX-License-Identifier: AGPL-3.0-only
//! The served directory: opening region files safely, strong entity tags, disk usage and removal.
//!
//! Only names that [`RegionFile::parse`] accepts reach the file system, so there is no `/`,
//! no `..` and no dotfile. A symlink is followed only if it resolves to a file directly in the
//! directory. entity tags are computed here from the bytes on disk; the manifests' own `sha256`
//! fields are not trusted for this.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use pingers_crypto::{ContentHasher, content_digest};

use crate::region::{FileKind, RegionFile, RegionId};

/// An open region file, positioned at its start.
#[derive(Debug)]
pub struct OpenedFile {
    /// The file.
    pub file: File,
    /// Its length when opened.
    pub len: u64,
    /// Its strong entity tag, quotes included.
    pub etag: Arc<str>,
}

/// Why a file could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Missing, not a regular file, or a symlink leading out of the directory.
    #[error("not found")]
    NotFound,
    /// Anything else.
    #[error("cannot read the file")]
    Io(#[source] io::Error),
}

#[derive(Clone, Debug)]
struct CachedTag {
    len: u64,
    modified: Option<SystemTime>,
    etag: Arc<str>,
}

/// The served directory and its entity tag cache. Cheap to clone; clones share the cache.
#[derive(Clone, Debug)]
pub struct Directory {
    root: Arc<Path>,
    etags: Arc<Mutex<HashMap<String, CachedTag>>>,
}

impl Directory {
    /// The directory at `root`, which must already be canonical.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::from(root),
            etags: Arc::default(),
        }
    }

    /// The directory's path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opens `file`, computing its entity tag on first use (cached by length and modification time).
    /// Blocking: call from `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// [`OpenError::NotFound`] or [`OpenError::Io`].
    pub fn open(&self, file: &RegionFile) -> Result<OpenedFile, OpenError> {
        let name = file.name();
        let path = self.root.join(&name);
        let canonical = std::fs::canonicalize(&path).map_err(not_found_or_io)?;
        if canonical.parent() != Some(&*self.root) {
            return Err(OpenError::NotFound);
        }
        let mut handle = File::open(&canonical).map_err(not_found_or_io)?;
        let metadata = handle.metadata().map_err(OpenError::Io)?;
        if !metadata.is_file() {
            return Err(OpenError::NotFound);
        }
        let len = metadata.len();
        let modified = metadata.modified().ok();
        let cached = self
            .etags
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&name)
            .filter(|tag| tag.len == len && tag.modified == modified)
            .map(|tag| Arc::clone(&tag.etag));
        let etag = if let Some(etag) = cached {
            etag
        } else {
            // Hash the open handle, so the tag describes exactly the bytes that will be sent
            // even if the name is replaced meanwhile.
            let etag: Arc<str> = Arc::from(quoted_sha256(&mut handle).map_err(OpenError::Io)?);
            handle.seek(SeekFrom::Start(0)).map_err(OpenError::Io)?;
            self.etags
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(
                    name,
                    CachedTag {
                        len,
                        modified,
                        etag: Arc::clone(&etag),
                    },
                );
            etag
        };
        Ok(OpenedFile {
            file: handle,
            len,
            etag,
        })
    }

    /// Whether `file` exists as a regular file. Blocking.
    #[must_use]
    pub fn exists(&self, file: &RegionFile) -> bool {
        std::fs::metadata(self.root.join(file.name())).is_ok_and(|metadata| metadata.is_file())
    }

    /// Region files on disk, with their sizes. Other files (the host's own state, anything an
    /// operator left there) are not region files and not listed. Blocking.
    ///
    /// # Errors
    ///
    /// If the directory cannot be listed.
    pub fn region_files(&self) -> io::Result<Vec<(RegionFile, u64)>> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(file) = entry.file_name().to_str().and_then(RegionFile::parse) else {
                continue;
            };
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                files.push((file, metadata.len()));
            }
        }
        Ok(files)
    }

    /// Total bytes of every regular file in the directory, region or not. Blocking.
    ///
    /// # Errors
    ///
    /// If the directory cannot be listed.
    pub fn usage(&self) -> io::Result<u64> {
        let mut total: u64 = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let metadata = entry?.metadata()?;
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
        Ok(total)
    }

    /// Deletes a region's three files, whichever exist. Blocking.
    ///
    /// # Errors
    ///
    /// The first removal that failed for a reason other than the file being absent.
    pub fn remove_region(&self, region: &RegionId) -> io::Result<()> {
        for kind in FileKind::ALL {
            let file = RegionFile {
                region: region.clone(),
                kind,
            };
            let name = file.name();
            match std::fs::remove_file(self.root.join(&name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            self.etags
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&name);
        }
        Ok(())
    }
}

fn not_found_or_io(error: io::Error) -> OpenError {
    if error.kind() == io::ErrorKind::NotFound {
        OpenError::NotFound
    } else {
        OpenError::Io(error)
    }
}

/// `"<sha256 hex>"` of everything `reader` yields.
fn quoted_sha256(reader: &mut impl io::Read) -> io::Result<String> {
    let mut hasher = ContentHasher::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(buffer.get(..read).unwrap_or_default()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(quote(&hasher.finish().to_hex()))
}

/// `"<sha256 hex>"` of `bytes`.
#[must_use]
pub fn etag_of(bytes: &[u8]) -> String {
    quote(&content_digest(bytes).to_hex())
}

fn quote(hex: &str) -> String {
    let mut tag = String::with_capacity(hex.len().saturating_add(2));
    tag.push('"');
    tag.push_str(hex);
    tag.push('"');
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etags_are_quoted_sha256() {
        assert_eq!(
            etag_of(b"abc"),
            "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\""
        );
        let mut reader: &[u8] = b"abc";
        assert_eq!(quoted_sha256(&mut reader).ok(), Some(etag_of(b"abc")));
    }
}
