// SPDX-License-Identifier: AGPL-3.0-only
//! `vectors/proto/*.json`: pingers-proto's wire encodings, built with [`super::cbor`] rather
//! than pingers-proto. In inputs, an optional field that is absent is written as `""`.
//!
//! Every byte string is a [`ramp`] or a [`fill`], so a vector can be reproduced by hand.

mod api;
mod board;
mod frames;
mod invite;

use anyhow::Result;

use super::cbor::{array, bytes, uint};
use super::json::Json;
use super::{VectorFile, ramp, vector_file};

/// The proto files.
pub(super) fn files() -> Result<Vec<VectorFile>> {
    let mut files = board::files()?;
    files.extend(frames::files()?);
    files.extend(api::files()?);
    files.extend(invite::files()?);
    Ok(files)
}

/// A file in `vectors/proto/`, named without a directory prefix.
fn proto_file(name: &str, description: &str, vectors: Vec<Json>) -> Result<VectorFile> {
    vector_file("proto", name, description, vectors)
}

/// The board record's random prefix `r`; the knock reuses it as the matched randomness.
fn record_randomness() -> Vec<u8> {
    ramp(0, 16)
}

/// The board record's tag; the knock reuses it as the knocker's own randomness.
fn record_tag() -> Vec<u8> {
    ramp(0x10, 16)
}

/// The sealed push blob: 256 bytes of ciphertext and 64 of tag and encapsulated key.
fn push_blob() -> Vec<u8> {
    [ramp(0, 256), ramp(0, 64)].concat()
}

/// The push handle carried next to the blob.
fn push_handle() -> Vec<u8> {
    ramp(0xf0, 8)
}

/// `SealedPush` for listed relay 2, key 1, quiet: `[[0, relay id], key id, blob, urgency,
/// handle]`. Mailbox registrations, knocks and join requests all carry it.
fn listed_sealed_push() -> Vec<u8> {
    array([
        array([uint(0), uint(2)]),
        uint(1),
        bytes(&push_blob()),
        uint(1),
        bytes(&push_handle()),
    ])
}
