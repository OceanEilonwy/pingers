// SPDX-License-Identifier: AGPL-3.0-only
//! Server binary: routes to the crates above, nothing else.
//!
//! The self-hosted server binary. It routes requests to pingers-board-store, pingers-mailbox,
//! pingers-delivery and pingers-quota and holds no logic of its own. `axum` over `hyper`,
//! HTTP/2; the OHTTP gateway accepts `POST /ohttp` only from configured relays; identified
//! endpoints are direct; a config file; metrics without identifiers.
#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    anyhow::bail!("pingers-server is not implemented yet")
}
