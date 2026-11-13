// SPDX-License-Identifier: AGPL-3.0-only
//! Known-answer tests: every file in `vectors/presence/`.
//!
//! Each vector lists the randomness its computation consumes; [`FixedRng`] replays it, so the
//! public functions run unchanged and another implementation can reproduce the bytes.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot, RevokeKey};
use pingers_geo::PresenceState;
use pingers_proto::{
    BoardBeacon, BoardPage, Epoch, KeyPackageBytes, LeafIndex, MailboxId, MeetupHint, PartyId,
    Record, RecordRandomness, SegmentInfo, SizeClass, SpotId, WireEncode as _,
};
use pingers_testkit::{Vector, VectorFile, load_vectors};
use pingers_time::Bucket;
use zeroize::Zeroizing;

use crate::test_support::{FixedRng, RosterEntry, TestError, TestRoster};
use crate::{
    ActiveSpot, BoardMatch, KnockContext, KnockInputs, LeaseInputs, OwnRecords, PresenceError,
    RealSlot, SlotContent, SpotRef, SpotStatus, TrialMatcher, build_knock, build_lease, open_knock,
    verify_knock,
};

pub(crate) fn load(name: &str) -> Result<VectorFile, TestError> {
    let file = load_vectors(&format!("presence/{name}"))?;
    assert_eq!(file.name, format!("presence/{name}"));
    assert!(!file.vectors.is_empty());
    Ok(file)
}

/// Splits concatenated fixed-size values.
pub(crate) fn chunks<const N: usize>(bytes: &[u8]) -> Result<Vec<[u8; N]>, TestError> {
    let (chunks, rest) = bytes.as_chunks::<N>();
    if rest.is_empty() {
        Ok(chunks.to_vec())
    } else {
        Err(TestError::Malformed("concatenated field length"))
    }
}

fn optional_bytes(vector: &Vector, field: &str) -> Result<Option<Vec<u8>>, TestError> {
    let bytes = vector.input_bytes(field)?;
    Ok((!bytes.is_empty()).then_some(bytes))
}

fn numbers(vector: &Vector, field: &'static str) -> Result<Vec<u64>, TestError> {
    vector
        .expected
        .get(field)
        .and_then(|value| value.as_array())
        .ok_or(TestError::Malformed(field))?
        .iter()
        .map(|value| value.as_u64().ok_or(TestError::Malformed(field)))
        .collect()
}

fn bucket(vector: &Vector, field: &str) -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(vector.input_u64(field)?)?)
}

fn spot_ref(vector: &Vector, spot_id: [u8; 16]) -> Result<SpotRef, TestError> {
    Ok(SpotRef {
        party: PartyId::from_bytes(vector.input_array("party_id")?),
        spot: SpotId::from_bytes(spot_id),
    })
}

fn active(root: [u8; 32], spot: SpotRef, epoch: Epoch) -> Result<ActiveSpot, TestError> {
    Ok(ActiveSpot::derive(
        &PresenceRoot::from_bytes(Zeroizing::new(root)),
        &SpotStatus {
            spot,
            epoch,
            state: PresenceState::Inside,
        },
    )?)
}

/// What `build_lease` produced from a lease vector's inputs.
pub(crate) struct LeaseOutput {
    pub(crate) lease: Vec<u8>,
    pub(crate) real_positions: Vec<u64>,
    pub(crate) exhausted: bool,
}

pub(crate) fn compute_lease(vector: &Vector) -> Result<LeaseOutput, TestError> {
    let epoch = Epoch::new(vector.input_u64("epoch")?);
    let root = vector.input_array("presence_root")?;
    let spots: Vec<ActiveSpot> = chunks::<16>(&vector.input_bytes("spot_ids")?)?
        .into_iter()
        .map(|spot_id| active(root, spot_ref(vector, spot_id)?, epoch))
        .collect::<Result<_, _>>()?;
    let mailboxes = chunks::<16>(&vector.input_bytes("knock_mailboxes")?)?;
    let hint = optional_bytes(vector, "meetup_hint")?
        .map(|bytes| {
            bytes
                .try_into()
                .ok()
                .and_then(MeetupHint::from_wire)
                .ok_or(TestError::Malformed("meetup_hint"))
        })
        .transpose()?;
    let real = spots
        .iter()
        .zip(&mailboxes)
        .map(|(spot, mailbox)| RealSlot {
            spot,
            knock_mailbox: MailboxId::from_bytes(*mailbox),
            meetup_hint: hint,
        })
        .collect();
    let revoke_key = RevokeKey::from_bytes(Zeroizing::new(vector.input_array("revoke_key")?));
    let mut rng = FixedRng::new(&vector.input_bytes("randomness")?);
    let built = build_lease(
        &LeaseInputs {
            start: bucket(vector, "bucket")?,
            real,
            revoke_key: &revoke_key,
        },
        &mut rng,
    )?;
    let mut real_positions = Vec::new();
    for mailbox in &mailboxes {
        let position = built
            .slots
            .iter()
            .position(|slot| {
                matches!(slot.content, SlotContent::Real { knock_mailbox, .. }
                    if knock_mailbox.as_bytes() == mailbox)
            })
            .ok_or(TestError::Malformed("real slot missing"))?;
        real_positions
            .push(u64::try_from(position).map_err(|_e| TestError::Malformed("position"))?);
    }
    Ok(LeaseOutput {
        lease: built.lease.to_bytes().to_vec(),
        real_positions,
        exhausted: rng.exhausted_exactly(),
    })
}

#[test]
fn lease_vectors() -> Result<(), TestError> {
    for vector in &load("lease")?.vectors {
        let output = compute_lease(vector)?;
        assert_eq!(
            output.lease,
            vector.expected_bytes("lease")?,
            "{}",
            vector.name
        );
        assert_eq!(
            output.real_positions,
            numbers(vector, "real_positions")?,
            "{}",
            vector.name
        );
        assert!(
            output.exhausted,
            "{}: randomness not consumed exactly",
            vector.name
        );
    }
    Ok(())
}

/// What the matcher found in a trial-match vector.
pub(crate) struct MatchOutput {
    pub(crate) indexes: Vec<u64>,
    pub(crate) epochs: Vec<u64>,
    pub(crate) beacons: Vec<u8>,
}

pub(crate) fn compute_matches(vector: &Vector) -> Result<MatchOutput, TestError> {
    let epoch = Epoch::new(vector.input_u64("epoch")?);
    let spot = spot_ref(vector, vector.input_array("spot_id")?)?;
    let mut spots = vec![active(vector.input_array("presence_root")?, spot, epoch)?];
    if let Some(previous_root) = optional_bytes(vector, "previous_presence_root")? {
        let previous = epoch
            .get()
            .checked_sub(1)
            .map(Epoch::new)
            .ok_or(TestError::Malformed("epoch"))?;
        let root = previous_root
            .try_into()
            .map_err(|_e| TestError::Malformed("root"))?;
        spots.push(active(root, spot, previous)?);
    }
    let records: Vec<Record> = chunks::<{ Record::SIZE }>(&vector.input_bytes("records")?)?
        .iter()
        .map(Record::from_bytes)
        .collect();
    let own = OwnRecords::new(
        chunks::<16>(&vector.input_bytes("own_records")?)?
            .into_iter()
            .map(RecordRandomness::from_bytes)
            .collect(),
    );
    let page = BoardPage {
        segments: vec![SegmentInfo {
            start: bucket(vector, "segment_start")?,
            record_count: u32::try_from(records.len())
                .map_err(|_e| TestError::Malformed("count"))?,
            closed_hash: None,
        }],
        records: records.clone(),
        next: None,
    };
    let found = TrialMatcher::new(&spots, bucket(vector, "now")?).match_page(&page, &own)?;
    let index_of = |found: &BoardMatch| {
        records
            .iter()
            .position(|record| *record == found.record)
            .and_then(|index| u64::try_from(index).ok())
            .ok_or(TestError::Malformed("match not on page"))
    };
    Ok(MatchOutput {
        indexes: found.iter().map(index_of).collect::<Result<_, _>>()?,
        epochs: found.iter().map(|found| found.epoch.get()).collect(),
        beacons: found
            .iter()
            .flat_map(|found| found.beacon.to_bytes())
            .collect(),
    })
}

#[test]
fn trial_match_vectors() -> Result<(), TestError> {
    for vector in &load("trial-match")?.vectors {
        let output = compute_matches(vector)?;
        assert_eq!(
            output.indexes,
            numbers(vector, "matched_indexes")?,
            "{}",
            vector.name
        );
        assert_eq!(
            output.epochs,
            numbers(vector, "matched_epochs")?,
            "{}",
            vector.name
        );
        assert_eq!(
            output.beacons,
            vector.expected_bytes("beacons")?,
            "{}",
            vector.name
        );
    }
    Ok(())
}

/// What `build_knock` produced from a knock-build vector.
pub(crate) struct KnockOutput {
    pub(crate) encoded_knock: Vec<u8>,
    pub(crate) post: Vec<u8>,
    pub(crate) exhausted: bool,
}

pub(crate) fn compute_knock(vector: &Vector) -> Result<KnockOutput, TestError> {
    let epoch = Epoch::new(vector.input_u64("epoch")?);
    let spot_ref = spot_ref(vector, vector.input_array("spot_id")?)?;
    let spot = active(vector.input_array("presence_root")?, spot_ref, epoch)?;
    let start = bucket(vector, "bucket")?;
    let matched = BoardMatch {
        spot: spot_ref,
        epoch,
        start,
        record: Record {
            randomness: RecordRandomness::from_bytes(vector.input_array("matched_randomness")?),
            // Knocking uses only the record's randomness; tag and seal are checked before.
            tag: pingers_proto::RecordTag::from_bytes([0; 16]),
            sealed: pingers_proto::SealedBeacon::from_bytes([0; 64]),
        },
        beacon: BoardBeacon {
            knock_mailbox: MailboxId::from_bytes(vector.input_array("knock_mailbox")?),
            meetup_hint: None,
        },
    };
    let own_record = optional_bytes(vector, "own_randomness")?
        .map(|bytes| bytes.try_into().map(RecordRandomness::from_bytes))
        .transpose()
        .map_err(|_e| TestError::Malformed("own_randomness"))?;
    let class = match vector.input_u64("size_class")? {
        1_024 => SizeClass::Bytes1024,
        4_096 => SizeClass::Bytes4096,
        _ => return Err(TestError::Malformed("size_class")),
    };
    let signing_key =
        AccountSigningKey::from_bytes(Zeroizing::new(vector.input_array("signing_key")?));
    let mut rng = FixedRng::new(&vector.input_bytes("nonce")?);
    let outgoing = build_knock(
        &KnockInputs {
            matched: &matched,
            spot: &spot,
            own_record,
            own_leaf: LeafIndex::new(
                u32::try_from(vector.input_u64("leaf_index")?)
                    .map_err(|_e| TestError::Malformed("leaf_index"))?,
            ),
            key_package: KeyPackageBytes::from_bytes(vector.input_bytes("key_package")?),
            reply_mailbox: MailboxId::from_bytes(vector.input_array("reply_mailbox")?),
            meetup_subscription: None,
            signing_key: &signing_key,
            class,
        },
        &mut rng,
    )?;
    assert_eq!(outgoing.mailbox, matched.beacon.knock_mailbox);
    // Recover the encoded knock by opening it as the poster would.
    let opened = open_knock(
        &KnockContext {
            spot: &spot,
            start,
            own_record: matched.record.randomness,
        },
        &outgoing.post,
    )?;
    Ok(KnockOutput {
        encoded_knock: opened.knock.encode()?,
        post: outgoing.post,
        exhausted: rng.exhausted_exactly(),
    })
}

/// The verification outcome of a knock-verify vector: the knocker's leaf, or an error name.
pub(crate) fn compute_verify(vector: &Vector) -> Result<Result<u64, &'static str>, TestError> {
    let epoch = Epoch::new(vector.input_u64("epoch")?);
    let spot = active(
        vector.input_array("presence_root")?,
        spot_ref(vector, vector.input_array("spot_id")?)?,
        epoch,
    )?;
    let context = KnockContext {
        spot: &spot,
        start: bucket(vector, "bucket")?,
        own_record: RecordRandomness::from_bytes(vector.input_array("own_record")?),
    };
    let leaf = LeafIndex::new(
        u32::try_from(vector.input_u64("knocker_leaf")?)
            .map_err(|_e| TestError::Malformed("knocker_leaf"))?,
    );
    let removed = vector
        .inputs
        .get("knocker_removed")
        .map(|value| {
            value
                .as_u64()
                .map(Epoch::new)
                .ok_or(TestError::Malformed("knocker_removed"))
        })
        .transpose()?;
    let roster = TestRoster {
        current: Epoch::new(vector.input_u64("roster_current_epoch")?),
        members: vec![RosterEntry {
            leaf,
            key: AccountPublicKey::from_bytes(vector.input_array("knocker_key")?),
            joined: Epoch::new(vector.input_u64("knocker_joined")?),
            removed,
        }],
    };
    let outcome = open_knock(&context, &vector.input_bytes("post")?)
        .and_then(|opened| verify_knock(&context, opened, &roster));
    Ok(match outcome {
        Ok(verified) => Ok(u64::from(verified.knocker.get())),
        Err(error) => Err(error_kind(&error)),
    })
}

pub(crate) fn error_kind(error: &PresenceError) -> &'static str {
    match error {
        PresenceError::NotInside => "NotInside",
        PresenceError::TooManySlots => "TooManySlots",
        PresenceError::SpotMismatch => "SpotMismatch",
        PresenceError::KnockForOtherRecord => "KnockForOtherRecord",
        PresenceError::WrongEpoch => "WrongEpoch",
        PresenceError::UnknownKnocker => "UnknownKnocker",
        PresenceError::LeaseNotCurrent => "LeaseNotCurrent",
        PresenceError::DuplicateRecord => "DuplicateRecord",
        PresenceError::MailboxRule => "MailboxRule",
        PresenceError::Crypto(pingers_crypto::CryptoError::OpenFailed) => "OpenFailed",
        PresenceError::Crypto(pingers_crypto::CryptoError::BadSignature) => "BadSignature",
        PresenceError::Crypto(_) => "Crypto",
        PresenceError::Proto(_) => "Proto",
        PresenceError::Internal => "Internal",
    }
}

#[test]
fn knock_vectors() -> Result<(), TestError> {
    for vector in &load("knock")?.vectors {
        if vector.inputs.contains_key("post") {
            let outcome = compute_verify(vector)?;
            let expected = match vector
                .expected
                .get("error")
                .and_then(|value| value.as_str())
            {
                Some(kind) => Err(kind),
                None => Ok(vector.expected_u64("knocker_leaf")?),
            };
            assert_eq!(outcome, expected, "{}", vector.name);
        } else {
            let output = compute_knock(vector)?;
            assert_eq!(
                output.encoded_knock,
                vector.expected_bytes("encoded_knock")?,
                "{}",
                vector.name
            );
            assert_eq!(
                output.post,
                vector.expected_bytes("post")?,
                "{}",
                vector.name
            );
            assert!(
                output.exhausted,
                "{}: nonce not consumed exactly",
                vector.name
            );
        }
    }
    Ok(())
}
