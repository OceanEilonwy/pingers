// SPDX-License-Identifier: AGPL-3.0-only
//! Region map downloads (design B.12, "Map host"; android/docs/maps.md).
//!
//! The app shell makes no network calls (design E.1): it renders the packs this crate puts in
//! its maps directory. This crate:
//!
//! - fetches `catalog.json` from every host in the chain (the user's override, the host the
//!   Pingers server advertises, the publisher's default) and offers each region from the first
//!   host that lists it ([`catalog`]);
//! - downloads a region's place index and PMTiles pack in pieces, resuming a `.partial` file,
//!   checks each against the manifest's SHA-256 (pingers-crypto), moves them into place and
//!   writes the manifest last, so a region is installed exactly when its manifest is on disk
//!   ([`install`]);
//! - reaches the map host one of two ways ([`fetch`]): directly over HTTPS, which tells the
//!   host the device's address and the region; or, with "Download maps through privacy relay"
//!   on, sealed to the map host's own OHTTP gateway and carried by the user's OHTTP relay and
//!   the Pingers server, which forwards it unopened. Then the relay sees the address but not
//!   the file, the server sees neither, and the map host sees the file but not the address.
//!   Every piece is padded to one size and the number of pieces to a size class, so the sizes
//!   the relay and the server see name a size class, not a region.
//!
//! Nothing here logs, and no error carries a region, a host or a path.
#![forbid(unsafe_code)]

pub mod catalog;
mod error;
pub mod fetch;
mod host;
pub mod install;
mod manifest;
mod pieces;
#[cfg(test)]
mod tests;

pub use error::MapsError;
pub use host::{HostChain, MapHost, MapHostSource};
pub use manifest::{BoundingBox, CatalogEntry, PackFile, RegionManifest, StemName};
