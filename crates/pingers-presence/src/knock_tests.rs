// SPDX-License-Identifier: AGPL-3.0-only
//! Knocks: round trip, and every way a knock must be refused (design B.4.2, B.4.4, A.2).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_crypto::{
    CryptoError, KnockPlaintext, KnockSignedFields, RevokeKey, derive_knock_key, derive_seal_key,
    seal_knock, sign_knock,
};
use pingers_proto::{
    BoardBeacon, Epoch, KeyPackageBytes, Knock, MailboxId, PushHandle, RecordRandomness, RelayId,
    RelayRef, SealedPush, SealedPushBlob, SizeClass, Subscription, SubscriptionId, TimeToLive,
    Urgency, WireEncode as _,
};
use pingers_testkit::{TestRng, random_array, seeded_rng};
use pingers_time::Bucket;

use crate::test_support::{Member, TestError, TestParty, TestRoster};
use crate::{
    ActiveSpot, BoardMatch, KnockContext, KnockInputs, LeaseInputs, OpenedKnock, OutgoingKnock,
    PresenceError, RealSlot, SlotContent, build_knock, build_lease, open_knock, verify_knock,
};

const EPOCH: Epoch = Epoch::new(5);
const NEXT_EPOCH: Epoch = Epoch::new(6);

fn now() -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(1_970_000)?)
}

/// A poster's live record, and the match a knocker made of it.
struct Posted {
    randomness: RecordRandomness,
    matched: BoardMatch,
}

fn post_record(spot: &ActiveSpot, rng: &mut TestRng) -> Result<Posted, TestError> {
    let mailbox = MailboxId::from_bytes(random_array(rng));
    let revoke_key = RevokeKey::generate(rng);
    let built = build_lease(
        &LeaseInputs {
            start: now()?,
            real: vec![RealSlot {
                spot,
                knock_mailbox: mailbox,
                meetup_hint: None,
            }],
            revoke_key: &revoke_key,
        },
        rng,
    )?;
    let (real_slot, record) = built
        .slots
        .iter()
        .zip(built.lease.records)
        .find(|(candidate, _)| matches!(candidate.content, SlotContent::Real { .. }))
        .ok_or(TestError::Malformed("no real slot"))?;
    Ok(Posted {
        randomness: real_slot.randomness,
        matched: BoardMatch {
            spot: spot.spot(),
            epoch: spot.epoch(),
            start: now()?,
            record,
            beacon: BoardBeacon {
                knock_mailbox: mailbox,
                meetup_hint: None,
            },
        },
    })
}

fn subscription(rng: &mut TestRng) -> Result<Subscription, TestError> {
    Ok(Subscription {
        id: SubscriptionId::from_bytes(random_array(rng)),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(1)),
            key_id: pingers_proto::PushRelayKeyId::new(1),
            blob: SealedPushBlob::from_bytes(random_array(rng)),
            urgency: Urgency::Alert,
            handle: PushHandle::from_bytes(random_array(rng)),
        },
        time_to_live: TimeToLive::new(Duration::from_hours(2))?,
        owner: pingers_proto::SubscriptionOwner::from_bytes([7; 32]),
    })
}

struct Knocking<'test> {
    knocker: &'test Member,
    spot: &'test ActiveSpot,
    matched: &'test BoardMatch,
    class: SizeClass,
}

fn knock(knocking: &Knocking<'_>, rng: &mut TestRng) -> Result<OutgoingKnock, TestError> {
    Ok(build_knock(
        &KnockInputs {
            matched: knocking.matched,
            spot: knocking.spot,
            own_record: None,
            own_leaf: knocking.knocker.leaf,
            key_package: KeyPackageBytes::from_bytes(vec![0x4b; 300]),
            reply_mailbox: MailboxId::from_bytes(random_array(rng)),
            meetup_subscription: Some(subscription(rng)?),
            signing_key: &knocking.knocker.signing_key,
            class: knocking.class,
        },
        rng,
    )?)
}

fn roster(members: &[&Member]) -> TestRoster {
    TestRoster {
        current: EPOCH,
        members: members
            .iter()
            .map(|member| member.entry(Epoch::new(0), None))
            .collect(),
    }
}

/// A knock sealed and signed by hand, so its fields can disagree with each other.
struct HandKnock<'test> {
    signer: &'test Member,
    spot: &'test ActiveSpot,
    sealed_for: &'test RecordRandomness,
    knock: Knock,
}

fn seal_by_hand(hand: &mut HandKnock<'_>, rng: &mut TestRng) -> Result<Vec<u8>, TestError> {
    let spot = hand.spot.spot();
    hand.knock.signature = sign_knock(
        &hand.signer.signing_key,
        &KnockSignedFields {
            party: &spot.party,
            epoch: hand.knock.party_epoch,
            spot: &spot.spot,
            start: now()?,
            matched: &hand.knock.matched_randomness,
            key_package: &hand.knock.key_package,
            reply_mailbox: &hand.knock.reply_mailbox,
            meetup_subscription: hand.knock.meetup_subscription.as_ref(),
        },
    )?;
    let seal_key = derive_seal_key(hand.spot.spot_key(), now()?)?;
    let key = derive_knock_key(&seal_key, hand.sealed_for)?;
    Ok(seal_knock(
        &key,
        &KnockPlaintext {
            encoded: &hand.knock.encode()?,
            class: SizeClass::Bytes1024,
        },
        rng,
    )?)
}

fn hand_knock(member: &Member, matched: RecordRandomness, epoch: Epoch) -> Knock {
    Knock {
        matched_randomness: matched,
        own_randomness: None,
        leaf_index: member.leaf,
        party_epoch: epoch,
        key_package: KeyPackageBytes::from_bytes(vec![1; 40]),
        reply_mailbox: MailboxId::from_bytes([3; 16]),
        meetup_subscription: None,
        signature: pingers_proto::KnockSignature::from_bytes([0; 64]),
    }
}

#[test]
fn knock_round_trips_from_knocker_to_poster() -> Result<(), TestError> {
    let mut rng = seeded_rng(40);
    let party = TestParty::new(&mut rng);
    let spot = party.inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let posted = post_record(&spot, &mut rng)?;
    let outgoing = knock(
        &Knocking {
            knocker: &bob,
            spot: &spot,
            matched: &posted.matched,
            class: SizeClass::Bytes1024,
        },
        &mut rng,
    )?;
    assert_eq!(outgoing.mailbox, posted.matched.beacon.knock_mailbox);
    assert_eq!(outgoing.post.len(), 1_024);

    let context = KnockContext {
        spot: &spot,
        start: now()?,
        own_record: posted.randomness,
    };
    let opened = open_knock(&context, &outgoing.post)?;
    let verified = verify_knock(&context, opened, &roster(&[&bob]))?;
    assert_eq!(verified.knocker, bob.leaf);
    assert_eq!(verified.knocker_key, bob.signing_key.public_key());
    assert_eq!(verified.knock.party_epoch, EPOCH);
    assert_eq!(verified.knock.matched_randomness, posted.randomness);
    Ok(())
}

#[test]
fn x_wing_party_knocks_fill_four_kib() -> Result<(), TestError> {
    let mut rng = seeded_rng(41);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let posted = post_record(&spot, &mut rng)?;
    let outgoing = knock(
        &Knocking {
            knocker: &bob,
            spot: &spot,
            matched: &posted.matched,
            class: SizeClass::Bytes4096,
        },
        &mut rng,
    )?;
    assert_eq!(outgoing.post.len(), 4_096);
    Ok(())
}

#[test]
fn knocking_with_keys_of_another_spot_or_epoch_is_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(42);
    let party = TestParty::new(&mut rng);
    let spot = party.inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let posted = post_record(&spot, &mut rng)?;
    for wrong in [
        party.inside(NEXT_EPOCH)?,
        party.other_spot(&mut rng).inside(EPOCH)?,
    ] {
        let result = knock(
            &Knocking {
                knocker: &bob,
                spot: &wrong,
                matched: &posted.matched,
                class: SizeClass::Bytes1024,
            },
            &mut rng,
        );
        assert!(matches!(
            result,
            Err(TestError::Presence(PresenceError::SpotMismatch))
        ));
    }
    Ok(())
}

#[test]
fn knock_for_one_record_does_not_open_under_another() -> Result<(), TestError> {
    let mut rng = seeded_rng(43);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let first = post_record(&spot, &mut rng)?;
    let second = post_record(&spot, &mut rng)?;
    let outgoing = knock(
        &Knocking {
            knocker: &bob,
            spot: &spot,
            matched: &first.matched,
            class: SizeClass::Bytes1024,
        },
        &mut rng,
    )?;
    let other = KnockContext {
        spot: &spot,
        start: now()?,
        own_record: second.randomness,
    };
    assert!(matches!(
        open_knock(&other, &outgoing.post),
        Err(PresenceError::Crypto(CryptoError::OpenFailed))
    ));
    Ok(())
}

#[test]
fn knock_naming_another_record_is_refused() -> Result<(), TestError> {
    // Sealed under our record's key, but it says it answers a different record.
    let mut rng = seeded_rng(44);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let ours = RecordRandomness::from_bytes([1; 16]);
    let post = seal_by_hand(
        &mut HandKnock {
            signer: &bob,
            spot: &spot,
            sealed_for: &ours,
            knock: hand_knock(&bob, RecordRandomness::from_bytes([2; 16]), EPOCH),
        },
        &mut rng,
    )?;
    let context = KnockContext {
        spot: &spot,
        start: now()?,
        own_record: ours,
    };
    let opened = open_knock(&context, &post)?;
    assert!(matches!(
        verify_knock(&context, opened, &roster(&[&bob])),
        Err(PresenceError::KnockForOtherRecord)
    ));
    Ok(())
}

#[test]
fn knock_naming_another_epoch_is_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(45);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let ours = RecordRandomness::from_bytes([1; 16]);
    let post = seal_by_hand(
        &mut HandKnock {
            signer: &bob,
            spot: &spot,
            sealed_for: &ours,
            knock: hand_knock(&bob, ours, NEXT_EPOCH),
        },
        &mut rng,
    )?;
    let context = KnockContext {
        spot: &spot,
        start: now()?,
        own_record: ours,
    };
    let opened = open_knock(&context, &post)?;
    let mut roster = roster(&[&bob]);
    roster.current = NEXT_EPOCH;
    assert!(matches!(
        verify_knock(&context, opened, &roster),
        Err(PresenceError::WrongEpoch)
    ));
    Ok(())
}

/// A knock from `knocker` on a record at [`EPOCH`], opened by the poster.
fn opened_knock_from(
    knocker: &Member,
    spot: &ActiveSpot,
    rng: &mut TestRng,
) -> Result<(KnockContextOwned, OpenedKnock), TestError> {
    let posted = post_record(spot, rng)?;
    let outgoing = knock(
        &Knocking {
            knocker,
            spot,
            matched: &posted.matched,
            class: SizeClass::Bytes1024,
        },
        rng,
    )?;
    let owned = KnockContextOwned {
        own_record: posted.randomness,
    };
    let opened = open_knock(&owned.context(spot)?, &outgoing.post)?;
    Ok((owned, opened))
}

struct KnockContextOwned {
    own_record: RecordRandomness,
}

impl KnockContextOwned {
    fn context<'spot>(&self, spot: &'spot ActiveSpot) -> Result<KnockContext<'spot>, TestError> {
        Ok(KnockContext {
            spot,
            start: now()?,
            own_record: self.own_record,
        })
    }
}

#[test]
fn knock_from_a_member_removed_since_is_refused() -> Result<(), TestError> {
    // Mallory, lagging at EPOCH, knocks a record the poster still holds EPOCH keys for (an
    // epoch grace); the poster has since processed the Commit that removed her.
    let mut rng = seeded_rng(46);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let mallory = Member::new(3, &mut rng);
    let (owned, opened) = opened_knock_from(&mallory, &spot, &mut rng)?;
    let roster = TestRoster {
        current: NEXT_EPOCH,
        members: vec![mallory.entry(Epoch::new(0), Some(NEXT_EPOCH))],
    };
    assert!(matches!(
        verify_knock(&owned.context(&spot)?, opened, &roster),
        Err(PresenceError::UnknownKnocker)
    ));
    Ok(())
}

#[test]
fn knock_from_a_leaf_now_held_by_someone_else_is_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(47);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let mallory = Member::new(3, &mut rng);
    let newcomer = Member::new(3, &mut rng);
    let (owned, opened) = opened_knock_from(&mallory, &spot, &mut rng)?;
    let roster = TestRoster {
        current: NEXT_EPOCH,
        members: vec![
            mallory.entry(Epoch::new(0), Some(NEXT_EPOCH)),
            newcomer.entry(NEXT_EPOCH, None),
        ],
    };
    assert!(matches!(
        verify_knock(&owned.context(&spot)?, opened, &roster),
        Err(PresenceError::UnknownKnocker)
    ));
    Ok(())
}

#[test]
fn knock_from_outside_the_roster_is_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(48);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let stranger = Member::new(9, &mut rng);
    let bob = Member::new(2, &mut rng);
    let (owned, opened) = opened_knock_from(&stranger, &spot, &mut rng)?;
    assert!(matches!(
        verify_knock(&owned.context(&spot)?, opened, &roster(&[&bob])),
        Err(PresenceError::UnknownKnocker)
    ));
    Ok(())
}

#[test]
fn knock_from_a_member_who_joined_after_its_epoch_is_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(49);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let carol = Member::new(4, &mut rng);
    let (owned, opened) = opened_knock_from(&carol, &spot, &mut rng)?;
    let roster = TestRoster {
        current: NEXT_EPOCH,
        members: vec![carol.entry(NEXT_EPOCH, None)],
    };
    assert!(matches!(
        verify_knock(&owned.context(&spot)?, opened, &roster),
        Err(PresenceError::UnknownKnocker)
    ));
    Ok(())
}

#[test]
fn knock_signed_by_someone_else_is_refused() -> Result<(), TestError> {
    // Mallory claims Bob's leaf but can only sign with her own key.
    let mut rng = seeded_rng(50);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let mallory = Member::new(3, &mut rng);
    let ours = RecordRandomness::from_bytes([1; 16]);
    let post = seal_by_hand(
        &mut HandKnock {
            signer: &mallory,
            spot: &spot,
            sealed_for: &ours,
            knock: hand_knock(&bob, ours, EPOCH),
        },
        &mut rng,
    )?;
    let context = KnockContext {
        spot: &spot,
        start: now()?,
        own_record: ours,
    };
    let opened = open_knock(&context, &post)?;
    assert!(matches!(
        verify_knock(&context, opened, &roster(&[&bob, &mallory])),
        Err(PresenceError::Crypto(CryptoError::BadSignature))
    ));
    Ok(())
}

#[test]
fn knock_with_a_field_changed_after_signing_is_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(51);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let (owned, mut opened) = opened_knock_from(&bob, &spot, &mut rng)?;
    opened.knock.reply_mailbox = MailboxId::from_bytes([0xee; 16]);
    assert!(matches!(
        verify_knock(&owned.context(&spot)?, opened, &roster(&[&bob])),
        Err(PresenceError::Crypto(CryptoError::BadSignature))
    ));
    Ok(())
}

#[test]
fn knock_checked_against_another_bucket_is_refused() -> Result<(), TestError> {
    // The signature covers the record's bucket: a knock verified as if for another bucket
    // (a replayed record) fails.
    let mut rng = seeded_rng(52);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let bob = Member::new(2, &mut rng);
    let (owned, opened) = opened_knock_from(&bob, &spot, &mut rng)?;
    let context = KnockContext {
        spot: &spot,
        start: now()?.next().ok_or(TestError::Malformed("bucket"))?,
        own_record: owned.own_record,
    };
    assert!(matches!(
        verify_knock(&context, opened, &roster(&[&bob])),
        Err(PresenceError::Crypto(CryptoError::BadSignature))
    ));
    Ok(())
}

#[test]
fn garbage_post_does_not_open() -> Result<(), TestError> {
    let mut rng = seeded_rng(53);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let context = KnockContext {
        spot: &spot,
        start: now()?,
        own_record: RecordRandomness::from_bytes([1; 16]),
    };
    let garbage: [u8; 1_024] = random_array(&mut rng);
    assert!(matches!(
        open_knock(&context, &garbage),
        Err(PresenceError::Crypto(CryptoError::OpenFailed))
    ));
    Ok(())
}
