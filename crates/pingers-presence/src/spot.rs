// SPDX-License-Identifier: AGPL-3.0-only
//! Spot keys that exist only while the device is Inside (design B.4.3).
use pingers_crypto::{
    MeetupPsk, PresenceRoot, SpotKey, TagKey, derive_meetup_psk, derive_spot_key, derive_tag_key,
};
use pingers_geo::PresenceState;
use pingers_proto::{Epoch, PartyId, SpotId};
use pingers_time::Bucket;

use crate::PresenceError;

/// A Spot of a Party.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpotRef {
    /// The Party.
    pub party: PartyId,
    /// The Spot.
    pub spot: SpotId,
}

/// A Spot, the Party epoch its presence root belongs to, and its presence state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpotStatus {
    /// Which Spot.
    pub spot: SpotRef,
    /// The epoch of the presence root.
    pub epoch: Epoch,
    /// The Spot's state in pingers-geo.
    pub state: PresenceState,
}

/// The keys of one Spot at one Party epoch, held only while the device is `Inside` the Spot.
///
/// The only constructor checks the Spot's presence state, so an honest core cannot match,
/// post real records or knock for a Spot it is not in. Drop it (which zeroes the keys) when
/// the Spot leaves `Inside`. During the epoch grace period of design B.4.7 the caller holds
/// one for the current and one for the previous epoch.
pub struct ActiveSpot {
    spot: SpotRef,
    epoch: Epoch,
    spot_key: SpotKey,
    tag_key: TagKey,
}

impl ActiveSpot {
    /// Derives `K_spot` and `K_tag` for a Spot from the Party's presence root at that epoch.
    ///
    /// # Errors
    ///
    /// [`PresenceError::NotInside`] unless the Spot's state is [`PresenceState::Inside`];
    /// [`PresenceError::Crypto`] if derivation fails.
    pub fn derive(root: &PresenceRoot, status: &SpotStatus) -> Result<Self, PresenceError> {
        if status.state != PresenceState::Inside {
            return Err(PresenceError::NotInside);
        }
        let spot_key = derive_spot_key(root, &status.spot.spot)?;
        let tag_key = derive_tag_key(&spot_key)?;
        Ok(Self {
            spot: status.spot,
            epoch: status.epoch,
            spot_key,
            tag_key,
        })
    }

    /// Which Spot.
    #[must_use]
    pub const fn spot(&self) -> SpotRef {
        self.spot
    }

    /// Which Spot, borrowed (for structures that hold references to its ids).
    pub(crate) const fn spot_ref(&self) -> &SpotRef {
        &self.spot
    }

    /// Which Party epoch the keys belong to.
    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// `K_spot`, for deriving seal keys, knock keys and the meetup PSK.
    pub(crate) const fn spot_key(&self) -> &SpotKey {
        &self.spot_key
    }

    /// `K_tag`, for tags and trial matching.
    pub(crate) const fn tag_key(&self) -> &TagKey {
        &self.tag_key
    }

    /// The external PSK of a Meetup formed on a record of this Spot that starts in bucket
    /// `start`: `PSK_meet(b) = HKDF-Expand(K_spot, "psk" || be64(b), 32)` with id
    /// `(party_id, epoch, spot_id, b)` (design B.5).
    ///
    /// `b` is the start bucket of the record the knock answered. The poster (who creates the
    /// group) and the knocker (who joins it) both know it from that record, so they derive the
    /// same PSK in different wakes without agreeing on a clock. This is how
    /// pingers-meetup-session gets the PSK without ever holding `K_spot`, and because only an
    /// `ActiveSpot` makes one, only a device `Inside` the Spot can create or join its Meetup.
    ///
    /// # Errors
    ///
    /// [`PresenceError::Crypto`] if derivation fails.
    pub fn meetup_key(&self, start: Bucket) -> Result<MeetupKey, PresenceError> {
        Ok(MeetupKey {
            id: MeetupKeyId {
                spot: self.spot,
                epoch: self.epoch,
                bucket: start,
            },
            psk: derive_meetup_psk(&self.spot_key, start)?,
        })
    }
}

/// The id of a Meetup's external PSK: `(party_id, epoch, spot_id, b)` (design B.5). It goes
/// into the MLS PSK proposal and the Welcome's group secrets, both encrypted, so the server
/// never sees which Party or Spot a Meetup group belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeetupKeyId {
    /// The Party and Spot.
    pub spot: SpotRef,
    /// The Party epoch of the Spot keys.
    pub epoch: Epoch,
    /// The start bucket of the record the knock answered.
    pub bucket: Bucket,
}

/// `PSK_meet(b)` with its id: what pingers-meetup-session needs to create or join a Meetup
/// group (design B.5). Made only by [`ActiveSpot::meetup_key`], so the value and the id it is
/// bound under cannot drift apart. Not `Clone` or `Debug`: it holds key material, zeroed on
/// drop (style guide §8).
pub struct MeetupKey {
    id: MeetupKeyId,
    psk: MeetupPsk,
}

impl MeetupKey {
    /// The PSK id.
    #[must_use]
    pub const fn id(&self) -> MeetupKeyId {
        self.id
    }

    /// `PSK_meet(b)`, for pingers-mls to hand to `OpenMLS`.
    #[must_use]
    pub const fn psk(&self) -> &MeetupPsk {
        &self.psk
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_testkit::load_vectors;
    use zeroize::Zeroizing;

    use super::*;
    use crate::test_support::TestError;

    /// The PSK matches design B.5's `PSK_meet(b) = HKDF-Expand(K_spot, "psk" || be64(b), 32)`,
    /// using the shared key-schedule vectors (`vectors/crypto/presence-key-schedule.json`).
    #[test]
    fn the_meetup_psk_follows_the_design_derivation() -> Result<(), TestError> {
        for vector in load_vectors("crypto/presence-key-schedule")?.vectors {
            let root =
                PresenceRoot::from_bytes(Zeroizing::new(vector.input_array("presence_root")?));
            let status = SpotStatus {
                spot: SpotRef {
                    party: PartyId::from_bytes([1; 16]),
                    spot: SpotId::from_bytes(vector.input_array("spot_id")?),
                },
                epoch: Epoch::new(3),
                state: PresenceState::Inside,
            };
            let active = ActiveSpot::derive(&root, &status)?;
            let bucket = Bucket::from_index(vector.input_u64("bucket")?)?;
            let key = active.meetup_key(bucket)?;
            assert_eq!(
                key.psk().expose_bytes().as_slice(),
                vector.expected_bytes("meetup_psk")?,
                "{}",
                vector.name
            );
            assert_eq!(
                key.id(),
                MeetupKeyId {
                    spot: status.spot,
                    epoch: status.epoch,
                    bucket,
                },
                "the id names the Party, epoch, Spot and bucket the PSK was derived for"
            );
            let next = bucket.next().ok_or(TestError::Malformed("bucket"))?;
            assert_ne!(
                active.meetup_key(next)?.psk().expose_bytes(),
                key.psk().expose_bytes(),
                "each bucket has its own PSK"
            );
        }
        Ok(())
    }
}
