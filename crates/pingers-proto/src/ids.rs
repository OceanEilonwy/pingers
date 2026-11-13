// SPDX-License-Identifier: AGPL-3.0-only
//! Identifiers and counters that appear on the wire.

use core::fmt;

/// Defines a fixed-size opaque identifier whose `Debug` does not print it (style guide §9).
macro_rules! opaque_bytes {
    ($(#[$meta:meta])* $name:ident, $length:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $length]);

        impl $name {
            /// Length in bytes on the wire.
            pub const LENGTH: usize = $length;

            /// Wraps bytes read from the wire or produced by a random generator.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; $length]) -> Self {
                Self(bytes)
            }

            /// The bytes, for encoding and key derivation.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; $length] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(redacted)"))
            }
        }
    };
}

/// Defines a counter or index newtype. These are not identifying, so `Debug` prints them.
macro_rules! counter {
    ($(#[$meta:meta])* $name:ident, $inner:ty) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($inner);

        impl $name {
            /// Wraps a value read from the wire.
            #[must_use]
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            /// The value, for encoding.
            #[must_use]
            pub const fn get(self) -> $inner {
                self.0
            }

            /// The next value, or `None` on overflow.
            #[must_use]
            pub const fn checked_next(self) -> Option<Self> {
                match self.0.checked_add(1) {
                    Some(next) => Some(Self(next)),
                    None => None,
                }
            }
        }
    };
}

opaque_bytes!(
    /// A Party: a long-lived group of friends with its own MLS group. 16 random bytes, used as
    /// the context of the presence exporter (design B.4.1). Not the MLS group id.
    PartyId,
    16
);
opaque_bytes!(
    /// A Spot: a named place of a Party. 16 random bytes (design B.4.1).
    SpotId,
    16
);
opaque_bytes!(
    /// An MLS group on the delivery service, Party or Meetup alike: 32 random bytes (design D).
    /// Ordered lexicographically, which the duplicate-Meetup rule relies on (design B.5).
    GroupId,
    32
);
opaque_bytes!(
    /// An anonymous mailbox: `SHA-256("pingers/v1/mbox" || read_key)[..16]` (design B.4.9).
    /// Knowing it lets anyone post; reading needs the read key.
    MailboxId,
    16
);
opaque_bytes!(
    /// An account: `SHA-256(signature public key)[..16]` (design B.1).
    AccountId,
    16
);
opaque_bytes!(
    /// A sealed-push subscription to a group's log (design D). Random.
    SubscriptionId,
    16
);
opaque_bytes!(
    /// A lease on the board, assigned by the server in the board sync response.
    LeaseId,
    16
);
opaque_bytes!(
    /// The random handle a device puts in a sealed push so the content-free push tells it which
    /// mailbox or group to read (design B.7).
    PushHandle,
    8
);
opaque_bytes!(
    /// A chat message in a Meetup, chosen at random by its sender (design B.6).
    ChatMessageId,
    16
);
opaque_bytes!(
    /// A request to see a member's exact location (design B.6.1), chosen at random by the asker.
    LocationRequestId,
    16
);
opaque_bytes!(
    /// The fresh randomness `r` of one board record (design B.4.1). It makes every record
    /// unlinkable, derives the beacon nonce and identifies the record in a knock.
    RecordRandomness,
    16
);

counter!(
    /// An MLS epoch of a group.
    Epoch,
    u64
);
counter!(
    /// A member's leaf in an MLS group's ratchet tree.
    LeafIndex,
    u32
);
counter!(
    /// A Lamport clock value, for last-writer-wins state such as the meet point (design B.6).
    Lamport,
    u64
);
counter!(
    /// The generation `lk_gen` of a member's location key (design B.6.1); bumped on rotation.
    LocationGeneration,
    u32
);
counter!(
    /// A message's position in a mailbox or group log, assigned by the server.
    SequenceNumber,
    u64
);
counter!(
    /// A push relay in the server's advertised list (design D, `push_relay.relay_id`).
    RelayId,
    u16
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_redacted_in_debug_output() {
        let printed = format!("{:?}", SpotId::from_bytes([0xab; 16]));
        assert_eq!(printed, "SpotId(redacted)");
    }

    #[test]
    fn group_ids_order_lexicographically() {
        let mut low = [0xff; 32];
        low[0] = 0;
        assert!(GroupId::from_bytes(low) < GroupId::from_bytes([1; 32]));
    }

    #[test]
    fn counter_overflow_is_none() {
        assert_eq!(Epoch::new(u64::MAX).checked_next(), None);
        assert_eq!(Epoch::new(1).checked_next(), Some(Epoch::new(2)));
    }
}
