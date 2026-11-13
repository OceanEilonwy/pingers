// SPDX-License-Identifier: AGPL-3.0-only
//! Push relay binary.
//!
//! The publisher-run push relay. Stateless: HPKE open, check the mailbox binding, send to FCM
//! HTTP v1 or a UnifiedPush endpoint; per-token and per-server rate limits; key config
//! endpoint; logging off by default. No database.
#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    anyhow::bail!("pingers-push-relay is not implemented yet")
}
