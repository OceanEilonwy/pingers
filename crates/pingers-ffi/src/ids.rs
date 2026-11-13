// SPDX-License-Identifier: AGPL-3.0-only
//! Typed identifiers for the app.
//!
//! Opaque ids cross as lowercase hex in a one-field record, so Kotlin gets a distinct type per
//! id with value equality (a `ByteArray` field would compare by reference). `Debug` never
//! prints them (style guide §9).

use core::fmt;

use crate::CoreError;

/// Defines an FFI record for one of pingers-proto's opaque byte ids.
macro_rules! hex_id {
    ($(#[$meta:meta])* $name:ident => $core:ty) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, uniffi::Record)]
        pub struct $name {
            /// The id's bytes, in lowercase hex.
            pub hex: String,
        }

        impl From<$core> for $name {
            fn from(id: $core) -> Self {
                Self { hex: hex::encode(id.as_bytes()) }
            }
        }

        impl TryFrom<&$name> for $core {
            type Error = CoreError;

            fn try_from(id: &$name) -> Result<Self, CoreError> {
                hex::decode(&id.hex)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(<$core>::from_bytes)
                    .ok_or_else(|| CoreError::invalid(concat!("not a ", stringify!($name))))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(redacted)"))
            }
        }
    };
}

hex_id!(
    /// A Party.
    PartyId => pingers_proto::PartyId
);
hex_id!(
    /// A Spot of a Party.
    SpotId => pingers_proto::SpotId
);
hex_id!(
    /// A Meetup (its MLS group).
    MeetupId => pingers_proto::GroupId
);
hex_id!(
    /// An invite this device made (its mailbox).
    InviteId => pingers_proto::MailboxId
);
hex_id!(
    /// A chat message.
    MessageId => pingers_proto::ChatMessageId
);
hex_id!(
    /// A request to see someone's exact location.
    LocationRequestId => pingers_proto::LocationRequestId
);

/// A member of a Party or Meetup: their leaf in its group. Not identifying, so it is a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct MemberId {
    /// The leaf index.
    pub leaf: u32,
}

impl From<pingers_proto::LeafIndex> for MemberId {
    fn from(leaf: pingers_proto::LeafIndex) -> Self {
        Self { leaf: leaf.get() }
    }
}

impl From<MemberId> for pingers_proto::LeafIndex {
    fn from(id: MemberId) -> Self {
        Self::new(id.leaf)
    }
}

/// An exclusion zone: a row id on this device only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct ZoneId {
    /// The row id.
    pub row: u32,
}

impl From<pingers_screens::snapshot::ZoneId> for ZoneId {
    fn from(id: pingers_screens::snapshot::ZoneId) -> Self {
        Self { row: id.get() }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::{PartyId, SpotId};

    #[test]
    fn ids_round_trip_through_hex_and_hide_in_debug() -> Result<(), crate::CoreError> {
        let core = pingers_proto::PartyId::from_bytes([0xab; 16]);
        let ffi = PartyId::from(core);
        assert_eq!(ffi.hex, "ab".repeat(16));
        assert_eq!(pingers_proto::PartyId::try_from(&ffi)?, core);
        assert_eq!(format!("{ffi:?}"), "PartyId(redacted)");
        let short = SpotId { hex: "abcd".into() };
        assert!(pingers_proto::SpotId::try_from(&short).is_err());
        Ok(())
    }
}
