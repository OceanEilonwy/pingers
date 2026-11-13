// SPDX-License-Identifier: AGPL-3.0-only
//! Domain-separation labels. They exist only here (style guide §8).
//!
//! Labels the design states are used verbatim. Labels marked "decision" are not in the design
//! and are listed under Decisions in `CONTRACT.md`.

/// MLS exporter labels. pingers-mls passes these to the exporter, so they are public; every
/// other label in this module is private to the crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExporterLabel {
    /// `PR_e`, the Party presence root (design B.4.1), context `party_id`, 32 bytes.
    Presence,
    /// The state snapshot key (design B.2), context the epoch, 32 bytes.
    State,
    /// The log-auth key seed (design B.2), context the group id, 32 bytes.
    LogAuth,
}

impl ExporterLabel {
    /// The label string to give the MLS exporter.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Presence => "pingers/v1/presence",
            Self::State => "pingers/v1/state",
            Self::LogAuth => "pingers/v1/log-auth",
        }
    }

    /// The output length to ask the exporter for.
    #[must_use]
    pub const fn output_length(self) -> usize {
        32
    }
}

// HKDF-Expand info prefixes of the presence key schedule (design B.4.1).
pub(crate) const SPOT: &[u8] = b"spot";
pub(crate) const TAG: &[u8] = b"tag";
pub(crate) const SEAL: &[u8] = b"seal";
pub(crate) const PSK: &[u8] = b"psk";
pub(crate) const KNOCK: &[u8] = b"knock";

/// Associated-data prefix of the board beacon seal (design B.4.2).
pub(crate) const BOARD: &[u8] = b"pingers/v1/board";
/// First field of the knock signature (design B.4.2).
pub(crate) const KNOCK_SIGNATURE: &[u8] = b"pingers/v1/knock";
/// AEAD associated data of board cursors. Decision: the design says only "server-MACed".
pub(crate) const BOARD_CURSOR: &[u8] = b"pingers/v1/board-cursor";
/// Mailbox id derivation (design B.4.9).
pub(crate) const MAILBOX: &[u8] = b"pingers/v1/mbox";
/// HPKE info prefix of sealed push tokens (design B.4.9).
pub(crate) const PUSH: &[u8] = b"pingers/v1/push";
/// Push bind derivation for mailboxes (design B.4.9).
pub(crate) const PUSH_BIND: &[u8] = b"pingers/v1/push-bind";
/// Push bind derivation for group subscriptions. Decision: the design reuses the mailbox
/// label ("as for a mailbox"); a separate label keeps a subscription bind from ever equalling
/// a mailbox bind.
pub(crate) const SUBSCRIPTION_BIND: &[u8] = b"pingers/v1/sub-bind";
/// Keyed hash of a push token for the relay's rate limits. Decision: the design hashes the
/// token with plain SHA-256 and gives no label.
pub(crate) const RELAY_TOKEN: &[u8] = b"pingers/v1/relay-token";
/// Keyed hash of a server credential as the relay stores it. Decision: the design gives none.
pub(crate) const RELAY_CREDENTIAL: &[u8] = b"pingers/v1/relay-credential";
/// HPKE info prefix of location grants (design B.6.1).
pub(crate) const LOCATION_GRANT: &[u8] = b"pingers/v1/lk-grant";
/// HPKE info prefix of location asks. Decision: the design gives none.
pub(crate) const LOCATION_ASK: &[u8] = b"pingers/v1/lk-ask";
/// HKDF info for the invite join key (design D: `HKDF(invite_secret, "join")`).
pub(crate) const INVITE_JOIN: &[u8] = b"join";
/// Signature prefix for group log requests. Decision: the design gives none.
pub(crate) const GROUP_REQUEST: &[u8] = b"pingers/v1/grp-request";
/// Signature prefix for account registration. Decision: the design gives none.
pub(crate) const REGISTRATION: &[u8] = b"pingers/v1/register";
/// Prefix of the RLN external nullifier preimage (design B.4.8). Moved from pingers-anon.
pub(crate) const RLN_EXTERNAL_NULLIFIER: &[u8] = b"pingers/v1/rln";
/// Prefix of the RLN request digest `x` preimage (design B.4.8). Moved from pingers-anon.
pub(crate) const RLN_REQUEST_DIGEST: &[u8] = b"pingers/v1/rln/x";

/// Every private label, so tests can check they are distinct and vectors can name them.
#[cfg(test)]
pub(crate) const ALL: [&[u8]; 21] = [
    RLN_EXTERNAL_NULLIFIER,
    RLN_REQUEST_DIGEST,
    SPOT,
    TAG,
    SEAL,
    PSK,
    KNOCK,
    BOARD,
    KNOCK_SIGNATURE,
    BOARD_CURSOR,
    MAILBOX,
    PUSH,
    PUSH_BIND,
    SUBSCRIPTION_BIND,
    RELAY_TOKEN,
    RELAY_CREDENTIAL,
    LOCATION_GRANT,
    LOCATION_ASK,
    INVITE_JOIN,
    GROUP_REQUEST,
    REGISTRATION,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exporter_labels_are_the_design_strings() {
        // Every member's MLS exporter must be asked for exactly these (design B.2, B.4.1).
        assert_eq!(ExporterLabel::Presence.label(), "pingers/v1/presence");
        assert_eq!(ExporterLabel::State.label(), "pingers/v1/state");
        assert_eq!(ExporterLabel::LogAuth.label(), "pingers/v1/log-auth");
        assert_eq!(ExporterLabel::Presence.output_length(), 32);
    }

    #[test]
    fn labels_are_distinct() {
        for (index, label) in ALL.iter().enumerate() {
            assert!(
                !ALL.iter()
                    .skip(index.saturating_add(1))
                    .any(|other| other == label)
            );
        }
    }
}
