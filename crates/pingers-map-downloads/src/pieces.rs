// SPDX-License-Identifier: AGPL-3.0-only
//! Files fetched in byte ranges of the route's piece size: small JSON whole into memory,
//! packs and indexes into a `.partial` file that later attempts resume.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pingers_crypto::ContentHasher;
use pingers_ohttp::{BinaryResponse, Header};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::fetch::MapFetch;
use crate::{MapHost, MapsError, PackFile};

/// How long to wait when a preparing host gives no usable `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(30);
/// How often a small file is fetched again from the start when it changed between pieces.
const RESTARTS: u32 = 3;

/// Files from one host over one route, counting the requests, so a relayed download can be
/// padded to a size class.
pub(crate) struct Fetching<'route> {
    fetch: &'route dyn MapFetch,
    host: &'route MapHost,
    requests: u32,
}

/// One piece as the host answered it.
struct Answer {
    /// Where it starts in the file.
    start: u64,
    /// The file's whole length.
    total: u64,
    /// The host's entity tag, to pin later pieces to the same version.
    etag: Option<Vec<u8>>,
    body: Vec<u8>,
}

fn header<'answer>(answer: &'answer BinaryResponse, name: &str) -> Option<&'answer [u8]> {
    answer
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_slice())
}

/// `bytes <start>-<end>/<total>` from a `Content-Range`.
fn content_range(value: &[u8]) -> Option<(u64, u64, u64)> {
    let text = std::str::from_utf8(value).ok()?;
    let (range, total) = text.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

impl<'route> Fetching<'route> {
    /// Fetching from `host` through `fetch`, no requests made yet.
    pub(crate) const fn new(fetch: &'route dyn MapFetch, host: &'route MapHost) -> Self {
        Self {
            fetch,
            host,
            requests: 0,
        }
    }

    /// How many requests were made.
    pub(crate) const fn requests(&self) -> u32 {
        self.requests
    }

    /// Asks for bytes `start` to `start + piece - 1` of `path`, pinned to `etag` if known.
    async fn piece(
        &mut self,
        path: &str,
        start: u64,
        etag: Option<&[u8]>,
    ) -> Result<Answer, MapsError> {
        self.requests = self.requests.saturating_add(1);
        let headers = range_headers(start, self.fetch.piece().bytes, etag);
        let answer = self.fetch.get(self.host, path, headers).await?;
        read_piece(answer, start)
    }
}

/// `Range` for bytes `start` to `start + piece - 1`, and `If-Range` with `etag` if known.
fn range_headers(start: u64, piece: u64, etag: Option<&[u8]>) -> Vec<Header> {
    let end = start.saturating_add(piece.saturating_sub(1));
    let mut headers = vec![Header {
        name: "range".to_owned(),
        value: format!("bytes={start}-{end}").into_bytes(),
    }];
    if let Some(etag) = etag {
        headers.push(Header {
            name: "if-range".to_owned(),
            value: etag.to_vec(),
        });
    }
    headers
}

/// What the host answered to a piece starting at `start`.
fn read_piece(answer: BinaryResponse, start: u64) -> Result<Answer, MapsError> {
    let etag = header(&answer, "etag").map(<[u8]>::to_vec);
    match answer.status.get() {
        206 => {
            let (first, last, total) = header(&answer, "content-range")
                .and_then(content_range)
                .ok_or(MapsError::BadAnswer)?;
            let length = u64::try_from(answer.body.len()).map_err(|_| MapsError::BadAnswer)?;
            if first != start
                || last.checked_sub(first).and_then(|n| n.checked_add(1)) != Some(length)
                || last >= total
            {
                return Err(MapsError::BadAnswer);
            }
            Ok(Answer {
                start,
                total,
                etag,
                body: answer.body,
            })
        }
        // The whole file: the host ignored the range, or the file changed under If-Range.
        200 => Ok(Answer {
            start: 0,
            total: u64::try_from(answer.body.len()).map_err(|_| MapsError::BadAnswer)?,
            etag,
            body: answer.body,
        }),
        503 => Err(MapsError::Preparing {
            retry_after: header(&answer, "retry-after")
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|text| text.trim().parse::<u64>().ok())
                .map_or(DEFAULT_RETRY_AFTER, Duration::from_secs),
        }),
        404 => Err(MapsError::NotOffered),
        status => Err(MapsError::Status { status }),
    }
}

/// A small file (a catalog, a manifest) whole, at most `limit` bytes, in pieces pinned to
/// one version; fetched again from the start if it changed between pieces.
pub(crate) async fn fetch_small(
    fetching: &mut Fetching<'_>,
    path: &str,
    limit: u64,
) -> Result<Vec<u8>, MapsError> {
    let mut restarts = 0_u32;
    'whole: loop {
        let mut body: Vec<u8> = Vec::new();
        let mut etag: Option<Vec<u8>> = None;
        loop {
            let have = u64::try_from(body.len()).map_err(|_| MapsError::BadAnswer)?;
            let answer = fetching.piece(path, have, etag.as_deref()).await?;
            if answer.total > limit {
                return Err(MapsError::BadAnswer);
            }
            if answer.start != have {
                // A new version: start again, a bounded number of times.
                restarts = restarts.saturating_add(1);
                if restarts > RESTARTS {
                    return Err(MapsError::BadAnswer);
                }
                if answer.start == 0 && u64::try_from(answer.body.len()).ok() == Some(answer.total)
                {
                    return Ok(answer.body);
                }
                continue 'whole;
            }
            body.extend_from_slice(&answer.body);
            etag = answer.etag.or(etag);
            if u64::try_from(body.len()).ok() >= Some(answer.total) {
                return Ok(body);
            }
        }
    }
}

/// Where a file's partial download lives: named after the checksum it must reach, so a new
/// version of the region never resumes an old one.
pub(crate) fn partial_path(directory: &Path, file: &PackFile) -> PathBuf {
    let digest: String = file.sha256.to_hex().chars().take(16).collect();
    directory.join(format!("{}.{digest}.partial", file.file))
}

/// Downloads `file` from `host` into `directory`, resuming its partial file, and moves it into
/// place once its length and SHA-256 match the manifest. `progress` gets the bytes of this file
/// so far.
pub(crate) async fn fetch_file(
    fetching: &mut Fetching<'_>,
    directory: &Path,
    file: &PackFile,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<(), MapsError> {
    let target = directory.join(&file.file);
    let partial = partial_path(directory, file);
    let (mut hasher, mut have) = resume(&partial, file.size).await?;
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&partial)
        .await?;
    let path = format!("/{}", file.file);
    let mut etag: Option<Vec<u8>> = None;
    progress(have);
    while have < file.size {
        let answer = fetching.piece(&path, have, etag.as_deref()).await;
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                out.flush().await?;
                return Err(error);
            }
        };
        if answer.total != file.size {
            // Not the file the manifest describes: the partial cannot be trusted either.
            drop(out);
            remove_if_present(&partial).await?;
            return Err(MapsError::Checksum);
        }
        if answer.start != have {
            if answer.start != 0 {
                return Err(MapsError::BadAnswer);
            }
            // The whole file came back: start the partial over with it.
            out.set_len(0).await?;
            hasher = ContentHasher::new();
            have = 0;
        }
        out.write_all(&answer.body).await?;
        hasher.update(&answer.body);
        have = have
            .saturating_add(u64::try_from(answer.body.len()).map_err(|_| MapsError::BadAnswer)?);
        etag = answer.etag.or(etag);
        progress(have);
    }
    out.flush().await?;
    out.sync_all().await?;
    drop(out);
    if have != file.size || hasher.finish() != file.sha256 {
        remove_if_present(&partial).await?;
        return Err(MapsError::Checksum);
    }
    tokio::fs::rename(&partial, &target).await?;
    Ok(())
}

/// The hash and length of what an earlier attempt left in `partial`; a partial longer than
/// the file is thrown away.
async fn resume(partial: &Path, size: u64) -> Result<(ContentHasher, u64), MapsError> {
    let mut hasher = ContentHasher::new();
    let mut file = match tokio::fs::File::open(partial).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((hasher, 0)),
        Err(error) => return Err(error.into()),
    };
    if file.metadata().await?.len() > size {
        drop(file);
        remove_if_present(partial).await?;
        return Ok((hasher, 0));
    }
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut have: u64 = 0;
    loop {
        let read = file.read(&mut buffer).await?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        hasher.update(chunk);
        have = have.saturating_add(u64::try_from(read).map_err(|_| MapsError::Internal)?);
    }
    Ok((hasher, have))
}

/// Removes `path`; already gone is fine.
pub(crate) async fn remove_if_present(path: &Path) -> Result<(), MapsError> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn content_ranges_parse() {
        assert_eq!(content_range(b"bytes 0-9/100"), Some((0, 9, 100)));
        assert_eq!(content_range(b" bytes 10-19/20 "), Some((10, 19, 20)));
        assert_eq!(content_range(b"bytes */100"), None);
        assert_eq!(content_range(b"items 0-9/100"), None);
        assert_eq!(content_range(b"bytes 0-9"), None);
        assert_eq!(content_range(&[0xff]), None);
    }

    #[test]
    fn partials_are_named_after_their_checksum() {
        let file = PackFile {
            file: "a.b.pmtiles".to_owned(),
            size: 3,
            sha256: pingers_crypto::content_digest(b"abc"),
            entries: None,
        };
        assert_eq!(
            partial_path(Path::new("/maps"), &file),
            Path::new("/maps/a.b.pmtiles.ba7816bf8f01cfea.partial")
        );
    }
}
