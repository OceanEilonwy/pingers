// SPDX-License-Identifier: AGPL-3.0-only
//! Spot keys that exist only while the device is Inside (design B.4.3).
use pingers_crypto::{PresenceRoot, SpotKey, TagKey, derive_spot_key, derive_tag_key};
use pingers_geo::PresenceState;
use pingers_proto::{Epoch, PartyId, SpotId};

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
}
