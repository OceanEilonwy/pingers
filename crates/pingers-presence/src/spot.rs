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

    /// `PSK_meet(b) = HKDF-Expand(K_spot, "psk" || be64(b), 32)`, the external PSK that binds
    /// a Meetup group created in bucket `bucket` to this Party and Spot (design B.5). This is
    /// how pingers-meetup-session gets the PSK without ever holding `K_spot`; the result is a
    /// zeroising secret type.
    ///
    /// # Errors
    ///
    /// [`PresenceError::Crypto`] if derivation fails.
    pub fn meetup_psk(&self, bucket: Bucket) -> Result<MeetupPsk, PresenceError> {
        Ok(derive_meetup_psk(&self.spot_key, bucket)?)
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
            assert_eq!(
                active.meetup_psk(bucket)?.expose_bytes().as_slice(),
                vector.expected_bytes("meetup_psk")?,
                "{}",
                vector.name
            );
            let next = bucket.next().ok_or(TestError::Malformed("bucket"))?;
            assert_ne!(
                active.meetup_psk(next)?.expose_bytes(),
                active.meetup_psk(bucket)?.expose_bytes(),
                "each bucket has its own PSK"
            );
        }
        Ok(())
    }
}
