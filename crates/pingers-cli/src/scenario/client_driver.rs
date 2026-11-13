// SPDX-License-Identifier: AGPL-3.0-only
//! [`ClientDriver`]: runs scenarios on real [`Client`]s, one per device.
//!
//! Each device's client is opened over its [`FakePlatform`](pingers_testkit::FakePlatform)
//! and [`RecordingEvents`](pingers_testkit::RecordingEvents), with a database in the driver's
//! directory and a database key put in the fake secure store first (as the shell does on first
//! run). The L3 crates come from the driver's `build` function, so the same driver runs on
//! [`FakeCrates`](pingers_testkit::FakeCrates) today and on the real crates against the
//! in-process harness once they land.

use core::future::{self, Future};
use std::path::PathBuf;

use anyhow::Context as _;
use pingers_client::ports::GeofenceTransition;
use pingers_client::{Client, ClientConfig, ClientError, Crates};
use pingers_proto::PushHandle;
use pingers_store::Database;
use pingers_testkit::{random_array, seeded_rng};

use super::format::Transition;
use super::runner::{Device, DeviceInput, ScenarioDriver, World};

/// The handle a scenario push carries. The scenario format has no handles yet, so every push
/// names the same one; a driver over the real crates needs `DeviceInput::Push` to carry the
/// handle the device sealed.
const SCENARIO_PUSH: PushHandle = PushHandle::from_bytes([0; 8]);

/// A [`ScenarioDriver`] over pingers-client.
pub(crate) struct ClientDriver<B> {
    directory: PathBuf,
    build: B,
    clients: Vec<DeviceClient>,
}

struct DeviceClient {
    name: String,
    client: Client,
}

impl<B> ClientDriver<B>
where
    B: FnMut(&Device, Database) -> Result<Crates, ClientError>,
{
    /// A driver that keeps each device's database in `directory` and builds its crates with
    /// `build`.
    pub(crate) fn new(directory: PathBuf, build: B) -> Self {
        Self {
            directory,
            build,
            clients: Vec::new(),
        }
    }

    /// The client of the device called `name`.
    pub(crate) fn client(&self, name: &str) -> anyhow::Result<&Client> {
        self.clients
            .iter()
            .find(|device| device.name == name)
            .map(|device| &device.client)
            .ok_or_else(|| anyhow::anyhow!("no client for {name}"))
    }
}

impl<B> ScenarioDriver for ClientDriver<B>
where
    B: FnMut(&Device, Database) -> Result<Crates, ClientError>,
{
    async fn start(&mut self, world: &World) -> anyhow::Result<()> {
        for (seed, device) in (0_u64..).zip(&world.devices) {
            let key: [u8; 32] = random_array(&mut seeded_rng(seed));
            device
                .platform
                .secure_store_put("pingers.database-key", &key)
                .map_err(|_locked| anyhow::anyhow!("{}'s secure store is locked", device.name))?;
            let config = ClientConfig {
                database: self.directory.join(format!("{}.db", device.name)),
                instance: None,
            };
            let build = &mut self.build;
            let client = Client::open_with(
                config,
                std::sync::Arc::new(device.platform.clone()),
                std::sync::Arc::new(device.events.clone()),
                |database| build(device, database),
            )
            .await
            .with_context(|| format!("opening {}'s client", device.name))?;
            self.clients.push(DeviceClient {
                name: device.name.clone(),
                client,
            });
        }
        Ok(())
    }

    async fn deliver(&mut self, device: &Device, input: DeviceInput) -> anyhow::Result<()> {
        let client = self.client(&device.name)?;
        let result = match input {
            DeviceInput::Fix(fix) => client.on_location(fix).await,
            DeviceInput::Geofence { region, transition } => {
                let transition = match transition {
                    Transition::Enter => GeofenceTransition::Enter,
                    Transition::Exit => GeofenceTransition::Exit,
                };
                client.on_os_geofence(region, transition).await
            }
            DeviceInput::FocusChanged => client.on_dnd_changed().await,
            DeviceInput::ScheduledWake => client.on_wake().await,
            DeviceInput::Push => client.on_push(SCENARIO_PUSH).await,
        };
        result.with_context(|| format!("delivering to {}", device.name))
    }

    /// Every client call awaits its whole wake, so there is nothing left to run. The Meetup
    /// live channel will need a settle hook here once pingers-meetup-session lands.
    fn settle(&mut self, _world: &World) -> impl Future<Output = anyhow::Result<()>> {
        future::ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::sync::atomic::{AtomicU64, Ordering};

    use pingers_client::ports::WakeInput;
    use pingers_gate::FocusState;
    use pingers_testkit::{CrateCall, FakeCrates};

    use super::*;
    use crate::scenario::format::parse;
    use crate::scenario::{Checks, run};

    /// A fresh directory under the system temporary directory, removed on drop.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> std::io::Result<Self> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pingers-cli-driver-{}-{unique}",
                std::process::id()
            ));
            drop(std::fs::remove_dir_all(&path));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    const SCENARIO: &str = r#"
        name = "driver"
        [party]
        members = ["alice", "ben"]
        [[party.spots]]
        name = "anchor"
        latitude = 51.5
        longitude = -0.12
        radius_m = 150
        [[steps]]
        at = 0
        device = "alice"
        geofence = { region = "anchor", transition = "enter" }
        [[steps]]
        at = 1
        device = "ben"
        focus = "on"
        [[steps]]
        at = 2
        device = "alice"
        location = { spot = "anchor", position = "inside" }
        [[steps]]
        at = 3
        device = "ben"
        wake = "push"
        [[steps]]
        at = 4
        device = "alice"
        geofence = { region = "anchor", transition = "exit" }
    "#;

    #[test]
    fn each_input_reaches_its_own_devices_client() -> anyhow::Result<()> {
        let scenario = parse(SCENARIO)?;
        let directory = TestDir::new()?;
        let mut fakes: Vec<(String, FakeCrates)> = Vec::new();
        {
            let mut driver = ClientDriver::new(directory.0.clone(), |device, _database| {
                let device_fakes = FakeCrates::new().map_err(|_invalid| ClientError::Internal)?;
                fakes.push((device.name.clone(), device_fakes.clone()));
                Ok(device_fakes.crates())
            });
            let runtime = tokio::runtime::Builder::new_current_thread().build()?;
            runtime.block_on(run(&scenario, &mut driver, Checks::Skip))?;
            assert!(driver.client("alice").is_ok());
            assert!(driver.client("carol").is_err());
        }
        let calls = |name: &str| -> Vec<CrateCall> {
            fakes
                .iter()
                .find(|(device, _fakes)| device == name)
                .map(|(_device, fakes)| fakes.calls())
                .unwrap_or_default()
        };
        let alice = calls("alice");
        assert_eq!(alice.len(), 3);
        assert_eq!(
            alice.first(),
            Some(&CrateCall::Wake(WakeInput::Geofence {
                region: String::from("anchor"),
                transition: GeofenceTransition::Enter
            }))
        );
        assert!(matches!(
            alice.get(1),
            Some(CrateCall::Wake(WakeInput::Fix(_)))
        ));
        assert_eq!(
            alice.get(2),
            Some(&CrateCall::Wake(WakeInput::Geofence {
                region: String::from("anchor"),
                transition: GeofenceTransition::Exit
            }))
        );
        assert_eq!(
            calls("ben"),
            [
                CrateCall::Wake(WakeInput::FocusChanged(FocusState::On)),
                CrateCall::Wake(WakeInput::Push(SCENARIO_PUSH)),
            ]
        );
        for name in ["alice", "ben"] {
            assert!(directory.0.join(format!("{name}.db")).exists());
        }
        Ok(())
    }
}
