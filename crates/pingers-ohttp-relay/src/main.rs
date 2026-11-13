// SPDX-License-Identifier: AGPL-3.0-only
//! Reference relay binary.
//!
//! A small reference OHTTP relay (forward only, full-duplex streaming, no logs) so relay
//! operators have something to run. It never decrypts anything.
#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    anyhow::bail!("pingers-ohttp-relay is not implemented yet")
}
