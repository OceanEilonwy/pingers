// SPDX-License-Identifier: AGPL-3.0-only
//! Which key a token is sealed to across a rotation, and for how long.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_crypto::PushRelayPublicKey;
use pingers_proto::{
    PushPlatform, PushRelayConfig, PushRelayKeyConfig, PushRelayKeyId, PushToken, RelayId,
};
use pingers_time::UnixSeconds;

use super::*;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const DAY: u64 = 86_400;
const NOW: u64 = 1_000_000;

fn relay(key_id: u8, max_days: u64) -> TestResult<VerifiedPushRelay> {
    Ok(VerifiedPushRelay {
        relay: RelayId::new(7),
        key: PushRelayPublicKey::new(key_id, [key_id; 32]),
        config: PushRelayConfig {
            key_config: PushRelayKeyConfig {
                key_id: PushRelayKeyId::new(key_id),
                public_key: [key_id; 32],
            },
            platforms: vec![PushPlatform::UnifiedPush],
            max_time_to_live: TimeToLive::new(Duration::from_hours(max_days.saturating_mul(24)))?,
            operator: "relay.invalid".to_owned(),
        },
    })
}

fn tokens() -> PushTokens {
    PushTokens {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"endpoint".to_vec()),
    }
}

fn keys(next_activates: Option<u64>) -> TestResult<RelayKeys> {
    Ok(RelayKeys {
        active: relay(1, 60)?,
        next: next_activates
            .map(|at| -> TestResult<ScheduledPushRelay> {
                Ok(ScheduledPushRelay {
                    verified: relay(2, 60)?,
                    activates_at: UnixSeconds::new(at),
                })
            })
            .transpose()?,
        fetched_at: UnixSeconds::new(NOW),
    })
}

#[test]
fn tokens_are_sealed_for_30_days_or_the_relay_s_limit() -> TestResult<()> {
    let now = UnixSeconds::new(NOW);
    let long = wake_push(&relay(3, 60)?, &tokens(), now, None)?;
    assert_eq!(long.alert.token.not_after.get(), NOW + 30 * DAY);
    assert_eq!(long.knock.token.class, PushClass::Location);
    assert_eq!(long.alert.token.class, PushClass::Standard);
    assert_eq!(long.alert.relay, RelayRef::Listed(RelayId::new(7)));
    assert_eq!(long.alert.relay_key.key_id(), 3);
    let short = wake_push(&relay(3, 2)?, &tokens(), now, None)?;
    assert_eq!(short.knock.token.not_after.get(), NOW + 2 * DAY);
    Ok(())
}

#[test]
fn a_token_sealed_to_a_key_with_a_successor_ends_when_the_key_retires() -> TestResult<()> {
    let now = UnixSeconds::new(NOW);
    let until = UnixSeconds::new(NOW + 5 * DAY);
    let capped = wake_push(&relay(1, 60)?, &tokens(), now, Some(until))?;
    assert_eq!(capped.alert.token.not_after, until);
    assert_eq!(capped.knock.token.not_after, until);
    let later = UnixSeconds::new(NOW + 50 * DAY);
    let uncapped = wake_push(&relay(1, 60)?, &tokens(), now, Some(later))?;
    assert_eq!(uncapped.alert.token.not_after.get(), NOW + 30 * DAY);
    Ok(())
}

#[test]
fn without_a_next_key_the_published_key_is_used_for_its_full_lifetime() -> TestResult<()> {
    let keys = keys(None)?;
    let seal_to = keys.seal_to(UnixSeconds::new(NOW));
    assert_eq!(seal_to.relay.key.key_id(), 1);
    assert_eq!(seal_to.until, None);
    Ok(())
}

#[test]
fn before_a_rotation_the_old_key_is_capped_at_its_retirement() -> TestResult<()> {
    let activation = NOW + 10 * DAY;
    let keys = keys(Some(activation))?;
    let seal_to = keys.seal_to(UnixSeconds::new(NOW));
    assert_eq!(seal_to.relay.key.key_id(), 1);
    assert_eq!(seal_to.until, Some(UnixSeconds::new(activation + 30 * DAY)));
    // Up to an hour after the activation the old key is still the one: a clock a little
    // ahead must not seal to a key the relay does not open yet.
    let just_after = keys.seal_to(UnixSeconds::new(activation + 3_599));
    assert_eq!(just_after.relay.key.key_id(), 1);
    Ok(())
}

#[test]
fn an_hour_after_the_activation_the_next_key_is_used() -> TestResult<()> {
    let activation = NOW + 10 * DAY;
    let keys = keys(Some(activation))?;
    let seal_to = keys.seal_to(UnixSeconds::new(activation + 3_600));
    assert_eq!(seal_to.relay.key.key_id(), 2);
    assert_eq!(seal_to.until, None);
    Ok(())
}

#[test]
fn only_push_delivery_failures_downgrade_pushes() {
    assert_eq!(
        downgrade_of(&Unavailable::WakePushesOff.into()),
        Some(PushDowngrade::WakePushesOff)
    );
    assert_eq!(
        downgrade_of(&Unavailable::NoPushToken.into()),
        Some(PushDowngrade::NoPushToken)
    );
    assert_eq!(
        downgrade_of(&Unavailable::NoPushRelay.into()),
        Some(PushDowngrade::NoPushRelay)
    );
    assert_eq!(
        downgrade_of(&SyncError::PushRelayKeyMismatch.into()),
        Some(PushDowngrade::RelayKeyMismatch)
    );
    assert_eq!(downgrade_of(&Unavailable::Sync.into()), None);
    assert_eq!(downgrade_of(&ClientError::Internal), None);
}

#[test]
fn origins_match_with_or_without_a_trailing_slash() {
    assert!(same_origin("https://push.invalid/", "https://push.invalid"));
    assert!(same_origin("https://push.invalid", "https://push.invalid"));
    assert!(!same_origin(
        "https://push.invalid",
        "https://other.invalid"
    ));
}
