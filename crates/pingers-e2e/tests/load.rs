// SPDX-License-Identifier: AGPL-3.0-only
//! The load generator on a small board: every device holds one lease, the reader reads them
//! all, and the byte counts add up.

use pingers_e2e::E2eError;
use pingers_e2e::load::{LoadConfig, READS_PER_DAY, run};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ten_devices_put_forty_four_records_on_the_board() -> Result<(), E2eError> {
    let report = run(LoadConfig {
        devices: 10,
        seed: 900,
    })
    .await?;
    let check = |condition: bool, what: &str| {
        if condition {
            Ok(())
        } else {
            Err(E2eError::step(format!("{what}: {report:?}")))
        }
    };
    check(report.devices == 11, "ten devices and the reader")?;
    check(
        report.live_records == 44,
        "one lease each, the reader's replaced",
    )?;
    check(report.pages == 1, "one page")?;
    check(report.record_bytes == 44 * 96, "96 bytes a record")?;
    check(
        report.read_response_bytes > report.record_bytes,
        "the page carries the records and a little framing",
    )?;
    check(
        report.relay_bytes_per_read > report.read_response_bytes,
        "encapsulation and HTTP/2 add to the read",
    )?;
    check(
        report.device_bytes_per_day() == report.relay_bytes_per_read.saturating_mul(READS_PER_DAY),
        "daily total",
    )
}
