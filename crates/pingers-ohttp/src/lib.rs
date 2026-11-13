// SPDX-License-Identifier: AGPL-3.0-only
//! OHTTP and chunked OHTTP encapsulation.
//!
//! Encapsulation only, for both client and gateway (architecture §3):
//! - RFC 9458 requests and responses ([`encapsulate_request`], [`Gateway::decapsulate`]);
//! - chunked OHTTP (draft-ietf-ohai-chunked-ohttp-08) for the Meetup live channel, with
//!   AES-128-GCM pinned and the zero-length non-final chunk rejected, which the upstream crate
//!   misses (design B.4.9);
//! - key config parsing, key pinning and mismatch detection (design B.12).
//!
//! Sans-IO: functions take and return bytes. pingers-transport moves them over HTTP/2.
//! The HPKE suite is fixed: X25519, HKDF-SHA256, AES-128-GCM.
//!
//! The HPKE layer is a patched copy of the `ohttp` crate in `vendor/ohttp` (see its README):
//! it takes randomness from the caller and replaces upstream's chunked `stream` module.
#![forbid(unsafe_code)]

mod chunked;
mod config;
mod error;
mod framing;
mod message;
mod request;

pub use chunked::{
    ChunkPosition, ChunkedClient, ChunkedClientStart, ChunkedGateway, OpenedChunk,
    RequestChunkDecoder, ResponseChunkDecoder,
};
pub use config::{GatewayKey, KeyConfig, check_pin};
pub use error::OhttpError;
pub use message::{BinaryRequest, BinaryResponse, Header, StatusCode};
pub use request::{
    DecapsulatedRequest, EncapsulatedRequest, Gateway, Responder, ResponseContext,
    encapsulate_request,
};
