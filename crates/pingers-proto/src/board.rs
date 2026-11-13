// SPDX-License-Identifier: AGPL-3.0-only
//! The blind board: records, beacons, leases and pages (design B.4.2, B.4.3, D).

use pingers_time::Bucket;

use crate::stub::not_implemented;
use crate::{MailboxId, ProtoError, RecordRandomness, WireDecode, WireEncode};

/// Bytes in the reserved half of a beacon's `reply` field, which must be zero.
const RESERVED_REPLY_BYTES: usize = 16;

/// A record's keyed tag, `HMAC-SHA256(K_tag, be64(b0) || r)[..16]`, or 16 random bytes in a
/// dummy (design B.4.1). Compare tags with pingers-crypto's constant-time check, not `==`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordTag([u8; 16]);

impl RecordTag {
    /// Wraps a computed tag or tag bytes read from a record.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The tag bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for RecordTag {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("RecordTag(redacted)")
    }
}

/// A sealed board beacon: 48 bytes of [`BoardBeacon`] plus a 16-byte AEAD tag, or 64 random
/// bytes in a dummy (design B.4.2).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SealedBeacon([u8; 64]);

impl SealedBeacon {
    /// Wraps sealed bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// The sealed bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl core::fmt::Debug for SealedBeacon {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("SealedBeacon(redacted)")
    }
}

/// One 96-byte record on the board: `r || tag || sealed` (design B.4.2).
///
/// Real and dummy records are indistinguishable without the Spot key, and the type does not
/// say which a record is: that knowledge stays in the client's lease table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Record {
    /// Fresh randomness `r`.
    pub randomness: RecordRandomness,
    /// `tag(b0, r)`, or random.
    pub tag: RecordTag,
    /// The sealed board beacon, or random.
    pub sealed: SealedBeacon,
}

impl Record {
    /// Encoded size in bytes, for every MLS ciphersuite.
    pub const SIZE: usize = 96;

    /// The fixed 96-byte layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        let (randomness, rest) = bytes.split_at_mut(RecordRandomness::LENGTH);
        let (tag, sealed) = rest.split_at_mut(16);
        randomness.copy_from_slice(self.randomness.as_bytes());
        tag.copy_from_slice(self.tag.as_bytes());
        sealed.copy_from_slice(self.sealed.as_bytes());
        bytes
    }

    /// Splits 96 bytes into their fields. Every 96-byte string is a valid record.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        let (randomness, rest) = bytes.split_first_chunk::<16>().unwrap_or((&[0; 16], &[]));
        let (tag, sealed) = rest.split_first_chunk::<16>().unwrap_or((&[0; 16], &[]));
        let sealed: [u8; 64] = sealed.try_into().unwrap_or([0; 64]);
        Self {
            randomness: RecordRandomness::from_bytes(*randomness),
            tag: RecordTag::from_bytes(*tag),
            sealed: SealedBeacon::from_bytes(sealed),
        }
    }

    /// Splits raw concatenated records, as a board page carries them, into records.
    ///
    /// # Errors
    ///
    /// [`ProtoError::WrongLength`] if the length is not a multiple of [`Record::SIZE`].
    pub fn parse_concatenated(bytes: &[u8]) -> Result<Vec<Self>, ProtoError> {
        let (records, remainder) = bytes.as_chunks::<{ Self::SIZE }>();
        if !remainder.is_empty() {
            let whole = bytes.len().saturating_sub(remainder.len());
            return Err(ProtoError::WrongLength {
                expected: whole.saturating_add(Self::SIZE),
                actual: bytes.len(),
            });
        }
        Ok(records.iter().map(Self::from_bytes).collect())
    }
}

/// The first 16 bytes of a Meetup's group id, carried in a beacon so knockers prefer records
/// of a Meetup already under way (design B.4.2, B.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeetupHint([u8; 16]);

impl MeetupHint {
    /// The hint for a Meetup group.
    #[must_use]
    pub fn for_group(group: &crate::GroupId) -> Self {
        let (prefix, _rest) = group
            .as_bytes()
            .split_first_chunk::<16>()
            .unwrap_or((&[0; 16], &[]));
        Self(*prefix)
    }

    /// The hint in 16 wire bytes; all zero means "no Meetup" and gives `None`.
    #[must_use]
    pub fn from_wire(bytes: [u8; 16]) -> Option<Self> {
        if bytes == [0; 16] {
            None
        } else {
            Some(Self(bytes))
        }
    }

    /// The hint bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for MeetupHint {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("MeetupHint(redacted)")
    }
}

/// The 48-byte plaintext inside a record's sealed beacon (design B.4.2).
///
/// Layout: `knock mailbox id (16) || 16 reserved zero bytes || meetup hint (16, zero = none)`.
/// It does not name the poster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardBeacon {
    /// The one-time mailbox to post a knock to.
    pub knock_mailbox: MailboxId,
    /// The Meetup the poster is already in, if any.
    pub meetup_hint: Option<MeetupHint>,
}

impl BoardBeacon {
    /// Plaintext size in bytes.
    pub const SIZE: usize = 48;

    /// The fixed 48-byte layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        let (mailbox, rest) = bytes.split_at_mut(MailboxId::LENGTH);
        let (_reserved, hint) = rest.split_at_mut(RESERVED_REPLY_BYTES);
        mailbox.copy_from_slice(self.knock_mailbox.as_bytes());
        if let Some(meetup_hint) = &self.meetup_hint {
            hint.copy_from_slice(meetup_hint.as_bytes());
        }
        bytes
    }

    /// Parses the fixed layout.
    ///
    /// # Errors
    ///
    /// [`ProtoError::ReservedNotZero`] if the reserved bytes are not zero.
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Result<Self, ProtoError> {
        let (mailbox, rest) = bytes
            .split_first_chunk::<16>()
            .ok_or(ProtoError::BadPadding)?;
        let (reserved, hint) = rest
            .split_first_chunk::<16>()
            .ok_or(ProtoError::BadPadding)?;
        if *reserved != [0; RESERVED_REPLY_BYTES] {
            return Err(ProtoError::ReservedNotZero);
        }
        let hint: [u8; 16] = hint.try_into().map_err(|_length| ProtoError::BadPadding)?;
        Ok(Self {
            knock_mailbox: MailboxId::from_bytes(*mailbox),
            meetup_hint: MeetupHint::from_wire(hint),
        })
    }
}

/// `SHA-256(revoke_key)`: lets the poster, and only the poster, revoke or replace its lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RevokeCommitment([u8; 32]);

impl RevokeCommitment {
    /// Wraps a commitment computed by pingers-crypto or read from the wire.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The commitment bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One upload to the board: exactly four records, live for the start bucket and the next
/// three (design B.4.2). 424 bytes: `be64(b0) || 4 × record || revoke commitment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The bucket the lease starts in (public: it is just the time).
    pub start: Bucket,
    /// The records, already shuffled by the client.
    pub records: [Record; Lease::SLOTS],
    /// Commitment to the key that revokes this lease.
    pub revoke_commitment: RevokeCommitment,
}

impl Lease {
    /// Records per lease: every upload has exactly this many.
    pub const SLOTS: usize = 4;

    /// Encoded size in bytes.
    pub const SIZE: usize = 424;

    /// The fixed 424-byte layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(&self.start.to_be_bytes());
        for record in &self.records {
            bytes.extend_from_slice(&record.to_bytes());
        }
        bytes.extend_from_slice(self.revoke_commitment.as_bytes());
        bytes.try_into().unwrap_or([0; Self::SIZE])
    }

    /// Parses the fixed layout.
    ///
    /// # Errors
    ///
    /// [`ProtoError::WrongLength`] unless `bytes` is exactly [`Lease::SIZE`] long;
    /// [`ProtoError::Bucket`] if the start bucket is out of range.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let wrong_length = ProtoError::WrongLength {
            expected: Self::SIZE,
            actual: bytes.len(),
        };
        if bytes.len() != Self::SIZE {
            return Err(wrong_length);
        }
        let (start, rest) = bytes
            .split_first_chunk::<8>()
            .ok_or(ProtoError::BadPadding)?;
        let start = Bucket::from_index(u64::from_be_bytes(*start)).map_err(ProtoError::Bucket)?;
        let (records, commitment) = rest
            .split_at_checked(Self::SLOTS.saturating_mul(Record::SIZE))
            .ok_or(ProtoError::BadPadding)?;
        let records: [Record; Self::SLOTS] = Record::parse_concatenated(records)?
            .try_into()
            .map_err(|_count| ProtoError::BadPadding)?;
        let commitment: [u8; 32] = commitment
            .try_into()
            .map_err(|_length| ProtoError::BadPadding)?;
        Ok(Self {
            start,
            records,
            revoke_commitment: RevokeCommitment::from_bytes(commitment),
        })
    }
}

/// SHA-256 over a closed board segment, for the equivocation check (design B.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SegmentHash([u8; 32]);

impl SegmentHash {
    /// Wraps a hash computed by pingers-crypto or read from the wire.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One segment's slice of a board page (design D, `SegmentInfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentInfo {
    /// The segment's start bucket.
    pub start: Bucket,
    /// How many of the page's records belong to this segment (a count at the wire boundary).
    pub record_count: u32,
    /// The segment's hash, present once the segment is closed.
    pub closed_hash: Option<SegmentHash>,
}

/// A server-MACed position in the board snapshot, valid for 10 minutes. Opaque to clients.
#[derive(Clone, PartialEq, Eq)]
pub struct BoardCursor(Vec<u8>);

impl BoardCursor {
    /// Wraps cursor bytes from the server (or, on the server, freshly authenticated bytes).
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The cursor bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for BoardCursor {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("BoardCursor(redacted)")
    }
}

/// One page of the live board, oldest segment first, at most 2,730 records (design D).
///
/// On the wire the records are raw concatenated bytes with no per-record framing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardPage {
    /// The segments this page covers, in order.
    pub segments: Vec<SegmentInfo>,
    /// The records, in segment order.
    pub records: Vec<Record>,
    /// Where the next page starts, if there is one.
    pub next: Option<BoardCursor>,
}

impl BoardPage {
    /// The most records one page carries (256 KiB).
    pub const MAX_RECORDS: usize = 2_730;
}

impl WireEncode for BoardPage {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        not_implemented(self)
    }
}

impl WireDecode for BoardPage {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn record(fill: u8) -> Record {
        Record {
            randomness: RecordRandomness::from_bytes([fill; 16]),
            tag: RecordTag::from_bytes([fill.wrapping_add(1); 16]),
            sealed: SealedBeacon::from_bytes([fill.wrapping_add(2); 64]),
        }
    }

    #[test]
    fn record_layout_is_randomness_tag_sealed() {
        let bytes = record(1).to_bytes();
        assert_eq!(bytes.first(), Some(&1));
        assert_eq!(bytes.get(16), Some(&2));
        assert_eq!(bytes.get(32), Some(&3));
        assert_eq!(Record::from_bytes(&bytes), record(1));
    }

    #[test]
    fn concatenated_records_must_be_whole() {
        assert!(matches!(
            Record::parse_concatenated(&[0; 100]),
            Err(ProtoError::WrongLength {
                expected: 192,
                actual: 100
            })
        ));
    }

    #[test]
    fn beacon_with_reserved_bytes_set_is_rejected() {
        let mut bytes = [0; BoardBeacon::SIZE];
        bytes[20] = 1;
        assert!(matches!(
            BoardBeacon::from_bytes(&bytes),
            Err(ProtoError::ReservedNotZero)
        ));
    }

    #[test]
    fn beacon_with_zero_hint_has_no_meetup() -> Result<(), ProtoError> {
        let beacon = BoardBeacon::from_bytes(&[0; BoardBeacon::SIZE])?;
        assert_eq!(beacon.meetup_hint, None);
        Ok(())
    }

    #[test]
    fn lease_round_trips_at_424_bytes() -> Result<(), ProtoError> {
        let lease = Lease {
            start: Bucket::from_index(1_234).map_err(ProtoError::Bucket)?,
            records: [record(1), record(2), record(3), record(4)],
            revoke_commitment: RevokeCommitment::from_bytes([9; 32]),
        };
        let bytes = lease.to_bytes();
        assert_eq!(bytes.len(), 424);
        assert_eq!(Lease::from_bytes(&bytes)?, lease);
        Ok(())
    }

    #[test]
    fn lease_of_wrong_length_is_rejected() {
        assert!(matches!(
            Lease::from_bytes(&[0; 423]),
            Err(ProtoError::WrongLength {
                expected: 424,
                actual: 423
            })
        ));
    }
}
