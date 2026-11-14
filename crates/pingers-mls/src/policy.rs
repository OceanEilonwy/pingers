// SPDX-License-Identifier: AGPL-3.0-only
//! Fixed group configuration (design B.2, B.5, B.6, B.8) and the wrapper's own encodings.

use openmls::prelude::{
    Capabilities, Ciphersuite, CredentialType, Extension, ExtensionType, Extensions, LeafNode,
    MlsGroupJoinConfig, PURE_CIPHERTEXT_WIRE_FORMAT_POLICY, RequiredCapabilitiesExtension,
    SenderRatchetConfiguration, UnknownExtension,
};
use pingers_crypto::{AccountPublicKey, GrantPublicKey};
use pingers_proto::{GroupId, LeafIndex, SubscriptionId};

use crate::{CipherSuitePolicy, MeetupPskId, MlsError, MlsStorage, Roster, RosterEntry};

/// The `pingers_grant_key` leaf-node extension type (design B.6.1): a private-use code point
/// (RFC 9420 §17.3 reserves 0xF000-0xFFFF), chosen so it is not a GREASE value.
pub(crate) const GRANT_KEY_EXTENSION: u16 = 0xF0A1;

/// The `pingers_meetup_subscription` leaf-node extension type: the member's sealed-push
/// subscription id in the Meetup group (16 bytes), present when it has one. Every member reads
/// it from the leaf, so whoever commits a member's removal lists that subscription in
/// `subscriptions_removed` (design D, G.6). The id is the server's already; members learn
/// nothing new about each other from it.
pub(crate) const SUBSCRIPTION_EXTENSION: u16 = 0xF0A2;

/// How many generations back a Meetup member can still decrypt (design B.6).
const MEETUP_OUT_OF_ORDER_TOLERANCE: u32 = 50;
/// OpenMLS's default, kept for Party groups whose traffic is ordered by the group log.
const PARTY_OUT_OF_ORDER_TOLERANCE: u32 = 5;
/// How far ahead of the last seen generation a message may be.
const MAXIMUM_FORWARD_DISTANCE: u32 = 1000;

/// Which kind of group a configuration is for. `pub` only because the sealed `GroupKind`
/// trait names it; the module is private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindTag {
    Party,
    Meetup,
}

/// The OpenMLS ciphersuite for a policy (design B.2, B.8).
pub(crate) fn ciphersuite(policy: CipherSuitePolicy) -> Result<Ciphersuite, MlsError> {
    match policy {
        CipherSuitePolicy::Classical => {
            Ok(Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519)
        }
        CipherSuitePolicy::QuantumResistant => Err(MlsError::UnsupportedSuite),
    }
}

/// Whether a big-endian suite code from the wire is one of the suites this build can run.
pub(crate) fn is_supported_suite_code(code: &[u8]) -> bool {
    [
        CipherSuitePolicy::Classical,
        CipherSuitePolicy::QuantumResistant,
    ]
    .into_iter()
    .filter_map(|policy| ciphersuite(policy).ok())
    .any(|suite| u16::from(suite).to_be_bytes() == code)
}

/// Leaf capabilities: the group's suite, basic credentials and, in Meetups, the grant-key
/// extension.
pub(crate) fn capabilities(kind: KindTag, suite: Ciphersuite) -> Capabilities {
    let grant = [
        ExtensionType::Unknown(GRANT_KEY_EXTENSION),
        ExtensionType::Unknown(SUBSCRIPTION_EXTENSION),
    ];
    let extensions: &[ExtensionType] = match kind {
        KindTag::Party => &[],
        KindTag::Meetup => &grant,
    };
    Capabilities::new(
        None,
        Some(&[suite]),
        Some(extensions),
        Some(&[]),
        Some(&[CredentialType::Basic]),
    )
}

/// The required-capabilities group context extension of a Meetup: every member must support
/// the grant-key extension. Party groups have none, which is how the two are told apart.
pub(crate) fn meetup_context_extensions()
-> Result<Extensions<openmls::prelude::GroupContext>, MlsError> {
    let required = RequiredCapabilitiesExtension::new(
        &[ExtensionType::Unknown(GRANT_KEY_EXTENSION)],
        &[],
        &[],
    );
    Extensions::single(Extension::RequiredCapabilities(required)).map_err(|_| MlsError::Internal)
}

/// Whether a group's context marks it as a Meetup.
pub(crate) fn is_meetup(context_extensions: &Extensions<openmls::prelude::GroupContext>) -> bool {
    context_extensions
        .required_capabilities()
        .is_some_and(|required| {
            required
                .extension_types()
                .contains(&ExtensionType::Unknown(GRANT_KEY_EXTENSION))
        })
}

/// The leaf-node extensions of a Meetup member: its grant key and, if it has one, its
/// subscription id.
pub(crate) fn meetup_leaf_extensions(
    grant_key: &GrantPublicKey,
    subscription: Option<&SubscriptionId>,
) -> Result<Extensions<LeafNode>, MlsError> {
    let grant = Extension::Unknown(
        GRANT_KEY_EXTENSION,
        UnknownExtension(grant_key.as_bytes().to_vec()),
    );
    let subscription = subscription.map(|subscription| {
        Extension::Unknown(
            SUBSCRIPTION_EXTENSION,
            UnknownExtension(subscription.as_bytes().to_vec()),
        )
    });
    Extensions::from_vec(core::iter::once(grant).chain(subscription).collect())
        .map_err(|_| MlsError::Internal)
}

/// The subscription id in a leaf, if it carries a well-formed one.
pub(crate) fn leaf_subscription(leaf: &LeafNode) -> Option<SubscriptionId> {
    let bytes = leaf.extensions().unknown(SUBSCRIPTION_EXTENSION)?;
    <[u8; SubscriptionId::LENGTH]>::try_from(bytes.0.as_slice())
        .ok()
        .map(SubscriptionId::from_bytes)
}

/// The grant key in a leaf, if it carries the extension.
pub(crate) fn leaf_grant_key(leaf: &LeafNode) -> Option<GrantPublicKey> {
    leaf.extensions()
        .unknown(GRANT_KEY_EXTENSION)
        .map(|extension| GrantPublicKey::from_bytes(extension.0.clone()))
}

/// The account key a leaf signs with, or `None` if it is not an Ed25519 key.
pub(crate) fn leaf_account_key(signature_key: &[u8]) -> Option<AccountPublicKey> {
    <[u8; 32]>::try_from(signature_key)
        .ok()
        .map(AccountPublicKey::from_bytes)
}

/// The join configuration (design B.2): handshakes and application data only as
/// `PrivateMessage` (so external commits and proposals, which are `PublicMessage`s, are
/// refused), the ratchet tree in the Welcome, no past-epoch secrets kept.
pub(crate) fn join_config(kind: KindTag) -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .max_past_epochs(0)
        .sender_ratchet_configuration(sender_ratchet(kind))
        .build()
}

/// The sender ratchet tolerance per kind.
pub(crate) fn sender_ratchet(kind: KindTag) -> SenderRatchetConfiguration {
    let tolerance = match kind {
        KindTag::Party => PARTY_OUT_OF_ORDER_TOLERANCE,
        KindTag::Meetup => MEETUP_OUT_OF_ORDER_TOLERANCE,
    };
    SenderRatchetConfiguration::new(tolerance, MAXIMUM_FORWARD_DISTANCE)
}

/// The external PSK id of a Meetup (design B.5):
/// `party_id (16) || be64(epoch) || spot_id (16) || be64(bucket)`, 48 bytes.
pub(crate) fn psk_id_bytes(id: &MeetupPskId) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(48);
    bytes.extend_from_slice(id.party.as_bytes());
    bytes.extend_from_slice(&id.epoch.get().to_be_bytes());
    bytes.extend_from_slice(id.spot.as_bytes());
    bytes.extend_from_slice(&id.bucket.index().to_be_bytes());
    bytes
}

/// Storage key of the Party roster a Meetup group checks adds against: under the group's
/// prefix, so deleting the group deletes it. The label cannot clash with OpenMLS's, which have
/// no `-`.
fn roster_key(group: &GroupId) -> Result<Vec<u8>, MlsError> {
    let mut key = crate::group_storage_prefix(group)?;
    key.extend_from_slice(b"pingers-meetup-roster");
    Ok(key)
}

/// One roster entry on disk: `be32(leaf) || signature key (32)`.
const ROSTER_ENTRY_LENGTH: usize = 36;

/// Stores the Party roster a Meetup group checks adds against.
pub(crate) fn store_party_roster(
    storage: &impl MlsStorage,
    group: &GroupId,
    roster: &Roster,
) -> Result<(), MlsError> {
    let mut bytes = Vec::with_capacity(roster.entries().len().saturating_mul(ROSTER_ENTRY_LENGTH));
    for entry in roster.entries() {
        bytes.extend_from_slice(&entry.leaf.get().to_be_bytes());
        bytes.extend_from_slice(entry.signature_key.as_bytes());
    }
    storage.write(&roster_key(group)?, &bytes)?;
    Ok(())
}

/// Loads the Party roster stored for a Meetup group.
pub(crate) fn load_party_roster(
    storage: &impl MlsStorage,
    group: &GroupId,
) -> Result<Roster, MlsError> {
    let bytes = storage
        .read(&roster_key(group)?)?
        .ok_or(crate::StorageError::Corrupt)?;
    let (chunks, remainder) = bytes.as_chunks::<ROSTER_ENTRY_LENGTH>();
    if !remainder.is_empty() {
        return Err(crate::StorageError::Corrupt.into());
    }
    let mut entries = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let (leaf, key) = chunk
            .split_first_chunk::<4>()
            .ok_or(crate::StorageError::Corrupt)?;
        let key = <[u8; 32]>::try_from(key).map_err(|_| crate::StorageError::Corrupt)?;
        entries.push(RosterEntry {
            leaf: LeafIndex::new(u32::from_be_bytes(*leaf)),
            signature_key: AccountPublicKey::from_bytes(key),
        });
    }
    Ok(Roster::new(entries))
}

#[cfg(test)]
mod tests {
    use super::is_supported_suite_code;

    #[test]
    fn only_the_suites_this_build_runs_are_supported() {
        // 0x0003: X25519, ChaCha20-Poly1305, Ed25519.
        assert!(is_supported_suite_code(&[0x00, 0x03]));
        // 0x0002: P-256, whose verification code RUSTSEC-2026-0387 concerns.
        assert!(!is_supported_suite_code(&[0x00, 0x02]));
        assert!(!is_supported_suite_code(&[0x00]));
    }
}
