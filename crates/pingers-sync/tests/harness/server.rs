// SPDX-License-Identifier: AGPL-3.0-only
//! What the client learns about its server: the reachability check, the parameters, and the
//! push relay's key config checked against its pins.

use pingers_crypto::key_config_hash;
use pingers_ohttp::KeyPins;
use pingers_proto::{KeyConfigHash, RelayId};
use pingers_sync::{ClockStatus, Route, SyncError, WakeBudget};
use pingers_testkit::HARNESS_PUSH_RELAY_ID;

use crate::support::{TestError, TestResult, World};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reachability_check_is_the_key_config_fetch_and_nothing_else() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    device.engine.check_relay(WakeBudget::UNLIMITED).await?;
    // The transport fetched the key config through the relay; no API request was made.
    assert!(device.link.seen().is_empty());

    device.link.set_offline(true);
    assert!(matches!(
        device.engine.check_relay(WakeBudget::UNLIMITED).await,
        Err(SyncError::NoRelay {
            route: Route::OhttpConfig
        })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_parameters_come_through_the_relay_and_set_the_clock() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    assert_eq!(device.engine.clock_status(), ClockStatus::Unchecked);
    let params = device.engine.params(WakeBudget::UNLIMITED).await?;
    assert!(params.board_page_records > 0);
    assert!(matches!(
        device.engine.clock_status(),
        ClockStatus::Corrected(_)
    ));
    let seen = device.link.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen.first().map(|seen| seen.path.as_str()),
        Some("/v1/params")
    );
    assert!(seen.iter().all(|seen| seen.authorization.is_none()));
    Ok(())
}

fn harness_key_hash(world: &World) -> Result<KeyConfigHash, TestError> {
    let public_key: [u8; 32] = world
        .harness
        .endpoints()
        .push_relay_key
        .as_slice()
        .try_into()
        .map_err(|_| TestError::Unexpected("push relay key length"))?;
    let config = pingers_proto::PushRelayKeyConfig {
        key_id: pingers_proto::PushRelayKeyId::new(1),
        public_key,
    };
    Ok(key_config_hash(&config.to_bytes()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_push_relay_key_comes_through_the_server_and_must_be_pinned() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let pinned = harness_key_hash(&world)?;
    let next = KeyConfigHash::from_bytes([9; 32]);
    let pins = KeyPins::new([next, pinned])?;
    let verified = device
        .engine
        .push_relay_key(HARNESS_PUSH_RELAY_ID, Some(&pins), WakeBudget::UNLIMITED)
        .await?;
    assert_eq!(verified.relay, HARNESS_PUSH_RELAY_ID);
    assert_eq!(
        verified.key.public_key().as_slice(),
        world.harness.endpoints().push_relay_key.as_slice()
    );
    assert_eq!(verified.key.key_id(), 1);
    assert_eq!(
        device.link.seen().first().map(|seen| seen.path.clone()),
        Some(format!(
            "/v1/push-relays/{}/config",
            HARNESS_PUSH_RELAY_ID.get()
        ))
    );

    // Without pins (a build that has none) the key is taken as it comes.
    let unpinned = device
        .engine
        .push_relay_key(HARNESS_PUSH_RELAY_ID, None, WakeBudget::UNLIMITED)
        .await?;
    assert_eq!(unpinned.key, verified.key);

    // A server handing out another key, or a rotated key the build does not know, is refused.
    assert!(matches!(
        device
            .engine
            .push_relay_key(
                HARNESS_PUSH_RELAY_ID,
                Some(&KeyPins::from(next)),
                WakeBudget::UNLIMITED
            )
            .await,
        Err(SyncError::PushRelayKeyMismatch)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_relay_the_server_does_not_list_is_not_found() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    assert!(matches!(
        device
            .engine
            .push_relay_key(RelayId::new(77), None, WakeBudget::UNLIMITED)
            .await,
        Err(SyncError::Status {
            route: Route::PushRelayConfig,
            status: 404
        })
    ));
    Ok(())
}
