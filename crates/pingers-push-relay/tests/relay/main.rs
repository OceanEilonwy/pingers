// SPDX-License-Identifier: AGPL-3.0-only
//! Integration tests: the relay over HTTP with the real dispatcher, and the opt-in end-to-end
//! tests against a local ntfy (run by `scripts/e2e-ntfy.sh`), and an opt-in live FCM test.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod common;
mod e2e_ntfy;
mod fcm_live;
mod http;
