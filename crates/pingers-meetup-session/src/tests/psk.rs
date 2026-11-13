// SPDX-License-Identifier: AGPL-3.0-only
//! `PSK_meet` (design B.4.1, B.5): each member derives it on its own from the Party's presence
//! root while Inside the Spot; it binds the Meetup to the Party, epoch, Spot and record bucket;
//! and nothing the server receives over the Meetup's life reveals it or the Spot.

use pingers_meetup::MeetupAction;
use pingers_mls::{CipherSuitePolicy, MlsError};
use pingers_presence::{SpotRef, SpotStatus};
use pingers_proto::{ChatBody, Epoch, LeafIndex, SpotId, WelcomeBytes};

use super::{GROUP, Leftovers, Live, Reply, TestError, TestResult, World, key_for, record_start};
use crate::{AddMember, CreateMeetup, Ended, SessionError, create, knock_key_package};

const CHLOE: LeafIndex = LeafIndex::new(2);

/// Whether `needle` occurs anywhere in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A Meetup forms between two members inside the Spot, a third joins, and it ends when the
/// last one leaves. Over that whole life the server receives the PSK, the Spot id and the
/// Party id neither in clear nor in hex: the PSK id and value live only inside the MLS
/// commit and the Welcome's group secrets, both encrypted, and the group id is random.
#[tokio::test]
async fn a_meetup_forms_grows_and_ends_without_the_server_learning_psk_meet_or_the_spot()
-> TestResult {
    let world = World::new(3)?;
    let alice_device = world.device(0)?;

    // Bob knocked; Alice, inside the Spot, creates the group bound to her PSK_meet(b).
    let bob_knock = knock_key_package(
        world.device(1)?.database(),
        CipherSuitePolicy::Classical,
        Box::new(pingers_testkit::seeded_rng(2)),
    )
    .await?;
    let created = create(
        &alice_device.runtime,
        CreateMeetup {
            key: world.meetup_key()?,
            group: GROUP,
            knocker: bob_knock.key_package,
            subscriptions: Vec::new(),
            own_subscription: None,
            suite: CipherSuitePolicy::Classical,
        },
        Box::new(pingers_testkit::seeded_rng(1)),
    )
    .await?;
    let mut welcomes: Vec<WelcomeBytes> = vec![created.welcome.clone()];
    let alice = Live {
        handle: created.handle,
        events: created.events,
    };
    // Bob joins with the PSK he derived himself for the record he knocked on.
    let bob = world.join(1, created.welcome, bob_knock.grant_key).await?;
    alice.connected().await?;
    bob.connected().await?;
    alice
        .handle
        .act(MeetupAction::SendChat {
            body: ChatBody::new("Here".to_owned())?,
            parent: None,
        })
        .await?;

    // Chloé enters later and knocks; Alice adds her to the same Meetup.
    let chloe_knock = knock_key_package(
        world.device(2)?.database(),
        CipherSuitePolicy::Classical,
        Box::new(pingers_testkit::seeded_rng(3)),
    )
    .await?;
    let welcome = alice
        .handle
        .add_member(AddMember {
            key_package: chloe_knock.key_package,
            subscription: None,
        })
        .await?;
    welcomes.push(welcome.clone());
    let chloe = world.join(2, welcome, chloe_knock.grant_key).await?;
    bob.view_until("Chloé in the Meetup", |view| view.peer(CHLOE).is_some())
        .await?;
    chloe
        .view_until("everyone in the Meetup", |view| view.peers.len() == 2)
        .await?;

    // Everyone leaves; the last one out deletes the group on the server and on the phone.
    assert_eq!(alice.handle.leave().await?, Ended::Left);
    bob.view_until("Alice gone", |view| view.peers.len() == 1)
        .await?;
    assert_eq!(bob.handle.leave().await?, Ended::Left);
    chloe
        .view_until("Chloé alone", |view| view.peers.is_empty())
        .await?;
    assert!(world.server.deleted().is_empty());
    assert_eq!(chloe.handle.leave().await?, Ended::Left);
    assert_eq!(world.server.deleted(), vec![GROUP]);
    for device in &world.devices {
        assert_eq!(device.leftovers(&GROUP)?, Leftovers::NOTHING);
    }

    let key = world.meetup_key()?;
    let secrets: [&[u8]; 3] = [
        key.psk().expose_bytes(),
        world.spot.as_bytes(),
        world.party.as_bytes(),
    ];
    let mut seen = world.server.seen();
    // The search finds what the server does learn: the random group id it stores the log under.
    assert!(seen.iter().any(|bytes| contains(bytes, GROUP.as_bytes())));
    // The Welcomes reach the knockers as mailbox posts, so the server holds them too.
    seen.extend(welcomes.into_iter().map(WelcomeBytes::into_bytes));
    for bytes in &seen {
        for secret in secrets {
            assert!(!contains(bytes, secret));
            assert!(!contains(bytes, hex::encode(secret).as_bytes()));
        }
    }
    Ok(())
}

/// The Welcome of a new Meetup joins only under the PSK of the same Party epoch, Spot and
/// record bucket: a member who derived it for another Spot, another bucket or another epoch
/// cannot join, and the right key still does afterwards.
#[tokio::test]
async fn a_welcome_joins_only_under_the_psk_of_its_spot_bucket_and_epoch() -> TestResult {
    let world = World::new(2)?;
    let knock = knock_key_package(
        world.device(1)?.database(),
        CipherSuitePolicy::Classical,
        Box::new(pingers_testkit::seeded_rng(2)),
    )
    .await?;
    let created = create(
        &world.device(0)?.runtime,
        CreateMeetup {
            key: world.meetup_key()?,
            group: GROUP,
            knocker: knock.key_package,
            subscriptions: Vec::new(),
            own_subscription: None,
            suite: CipherSuitePolicy::Classical,
        },
        Box::new(pingers_testkit::seeded_rng(1)),
    )
    .await?;

    let here = world.status();
    let other_spot = SpotStatus {
        spot: SpotRef {
            party: world.party,
            spot: SpotId::from_bytes([0x34; 16]),
        },
        ..here
    };
    let other_epoch = SpotStatus {
        epoch: Epoch::new(2),
        ..here
    };
    let start = record_start()?;
    let next = start.next().ok_or(TestError::Missing("the next bucket"))?;
    let wrong_keys = [
        key_for(&other_spot, start)?,
        key_for(&here, next)?,
        key_for(&other_epoch, start)?,
    ];
    for (seed, wrong) in (20..).zip(wrong_keys) {
        // A refused join consumes the grant secret it was given; the real one is kept for last.
        let grant_key =
            pingers_crypto::GrantKeyPair::generate(&mut pingers_testkit::seeded_rng(seed))?.secret;
        let attempt = world
            .join_with(
                1,
                Reply {
                    welcome: created.welcome.clone(),
                    grant_key,
                },
                wrong,
            )
            .await;
        assert!(matches!(
            attempt,
            Err(TestError::Session(SessionError::Mls(MlsError::PskMismatch)))
        ));
        assert_eq!(world.device(1)?.leftovers(&GROUP)?, Leftovers::NOTHING);
    }
    let bob = world
        .join_with(
            1,
            Reply {
                welcome: created.welcome,
                grant_key: knock.grant_key,
            },
            world.meetup_key()?,
        )
        .await?;
    bob.view_until("Alice in the Meetup", |view| view.peers.len() == 1)
        .await?;
    Ok(())
}
