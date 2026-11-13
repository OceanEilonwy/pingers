// SPDX-License-Identifier: AGPL-3.0-only
//! Load generation: N simulated devices each hold one live lease (four records, design B.4.2),
//! then a reader inside a Spot syncs and reads the whole board, as every device does about 32
//! times a day (design B.4.3, "Size and scaling").

use core::fmt;
use std::sync::Arc;
use std::time::Duration;

use pingers_time::Timer as _;
use pingers_transport::TokioTimer;

use pingers_proto::SpotId;
use pingers_testkit::{random_array, seeded_rng};

use crate::{Device, E2eError, Place, Recorder, Stack, form_party};

/// Board reads per device per day, from the cover policy's 45-minute target spacing.
pub const READS_PER_DAY: u64 = 32;

/// What to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadConfig {
    /// Devices that each post one lease (the reader comes on top).
    pub devices: u32,
    /// Seed for every key.
    pub seed: u64,
}

/// What one load run measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadReport {
    /// Devices that posted a lease, plus the reader.
    pub devices: u32,
    /// Records on the live board when the reader read it.
    pub live_records: u64,
    /// Pages the read took.
    pub pages: u64,
    /// Record bytes in the read (96 per record).
    pub record_bytes: u64,
    /// Inner response bytes of the whole read (CBOR pages as the server encodes them).
    pub read_response_bytes: u64,
    /// Inner request bytes of one sync (the lease upload).
    pub upload_request_bytes: u64,
    /// Bytes between relay and server for the reader's whole sync (encapsulated, both ways,
    /// with HTTP/2 framing): what the relay carries for one read.
    pub relay_bytes_per_read: u64,
    /// Bytes between relay and server for one upload-only sync.
    pub relay_bytes_per_upload: u64,
    /// Wall time to post all the leases.
    pub populate: Duration,
    /// Wall time of the reader's sync, read and trial-match.
    pub read_time: Duration,
}

impl LoadReport {
    /// One device's board download per day at [`READS_PER_DAY`].
    #[must_use]
    pub const fn device_bytes_per_day(&self) -> u64 {
        self.relay_bytes_per_read.saturating_mul(READS_PER_DAY)
    }

    /// The server's board egress per day if every device reads at [`READS_PER_DAY`].
    #[must_use]
    pub fn server_bytes_per_day(&self) -> u64 {
        self.device_bytes_per_day()
            .saturating_mul(u64::from(self.devices))
    }
}

#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "human-readable sizes"
)]
fn human(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1e9 {
        format!("{:.2} GB", value / 1e9)
    } else if value >= 1e6 {
        format!("{:.2} MB", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.1} KB", value / 1e3)
    } else {
        format!("{bytes} B")
    }
}

impl fmt::Display for LoadReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "devices:                 {}", self.devices)?;
        writeln!(formatter, "live records:            {}", self.live_records)?;
        writeln!(formatter, "pages per read:          {}", self.pages)?;
        writeln!(
            formatter,
            "record bytes per read:   {}",
            human(self.record_bytes)
        )?;
        writeln!(
            formatter,
            "inner response per read: {}",
            human(self.read_response_bytes)
        )?;
        writeln!(
            formatter,
            "relay bytes per read:    {}",
            human(self.relay_bytes_per_read)
        )?;
        writeln!(
            formatter,
            "relay bytes per upload:  {} (inner request {})",
            human(self.relay_bytes_per_upload),
            human(self.upload_request_bytes)
        )?;
        writeln!(
            formatter,
            "per device per day:      {} ({READS_PER_DAY} reads)",
            human(self.device_bytes_per_day())
        )?;
        writeln!(
            formatter,
            "server egress per day:   {}",
            human(self.server_bytes_per_day())
        )?;
        writeln!(
            formatter,
            "populate time:           {:.2} s",
            self.populate.as_secs_f64()
        )?;
        write!(
            formatter,
            "read + match time:       {:.1} ms",
            self.read_time.as_secs_f64() * 1e3
        )
    }
}

fn relay_total(stack: &Stack) -> u64 {
    stack
        .tap()
        .bytes_to_server()
        .saturating_add(stack.tap().bytes_from_server())
}

fn delta(after: u64, before: u64) -> u64 {
    after.saturating_sub(before)
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Starts a [`Stack`], has `config.devices` devices post one lease each through the relay (all
/// sharing one transport, each request with its own HPKE context), then measures one more
/// device's full sync and read from inside a Spot.
///
/// # Errors
///
/// Any scenario error.
pub async fn run(config: LoadConfig) -> Result<LoadReport, E2eError> {
    let stack = Stack::start(config.seed).await?;
    let recorder = Recorder::sizes_only();
    let transport = Arc::new(stack.transport(config.seed)?);
    let timer = TokioTimer::new();
    let started = timer.now();
    for index in 0..config.devices {
        let mut device = Device::with_transport(
            &format!("load-{index}"),
            config.seed.wrapping_add(u64::from(index)).wrapping_add(1),
            Arc::clone(&transport),
            Arc::clone(&recorder),
            stack.credentials(),
        );
        device.sync(&stack, false).await?;
    }
    let populate = timer.now().saturating_duration_since(started);

    let mut reader = Device::new(
        "reader",
        config.seed.wrapping_sub(1),
        &stack,
        Arc::clone(&recorder),
    )?;
    let party = form_party(&mut [&mut reader], config.seed)?;
    let mut rng = seeded_rng(config.seed);
    let place = Place {
        party,
        spot: SpotId::from_bytes(random_array(&mut rng)),
        latitude: 52.2053,
        longitude: 0.1218,
        radius_m: 100.0,
    };
    reader.arrive(&stack, place).await?;

    // An upload-only sync first, to price the upload alone (it also registers the knock
    // mailbox, which is excluded by measuring the second upload).
    reader.sync(&stack, false).await?;
    let before_upload = relay_total(&stack);
    let exchanges_before = recorder.exchanges().len();
    reader.sync(&stack, false).await?;
    let relay_bytes_per_upload = delta(relay_total(&stack), before_upload);
    let upload_request_bytes = recorder
        .exchanges()
        .get(exchanges_before..)
        .unwrap_or_default()
        .iter()
        .map(|exchange| count(exchange.request_len))
        .sum();

    let before_read = relay_total(&stack);
    let exchanges_before = recorder.exchanges().len();
    let read_started = timer.now();
    let report = reader.sync(&stack, true).await?;
    let read_time = timer.now().saturating_duration_since(read_started);
    let relay_bytes_per_read = delta(relay_total(&stack), before_read);
    let read_response_bytes = recorder
        .exchanges()
        .get(exchanges_before..)
        .unwrap_or_default()
        .iter()
        .map(|exchange| count(exchange.response_len))
        .sum();
    let live_records = count(report.records.len());
    stack.shutdown().await;
    Ok(LoadReport {
        devices: config.devices.saturating_add(1),
        live_records,
        pages: count(report.pages),
        record_bytes: live_records.saturating_mul(96),
        read_response_bytes,
        upload_request_bytes,
        relay_bytes_per_read,
        relay_bytes_per_upload,
        populate,
        read_time,
    })
}
