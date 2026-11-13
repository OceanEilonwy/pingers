// SPDX-License-Identifier: AGPL-3.0-only
//! Knocks: built by the newcomer, opened and verified by the poster (design B.4.2, B.4.4).

use pingers_crypto::{AccountPublicKey, AccountSigningKey};
use pingers_proto::{
    Epoch, KeyPackageBytes, Knock, LeafIndex, MailboxId, RecordRandomness, SizeClass, Subscription,
};
use pingers_time::Bucket;
use rand_core::CryptoRng;

use crate::stub::not_implemented;
use crate::{ActiveSpot, BoardMatch, PresenceError};

/// Looks up a Party member's signature key by leaf at an epoch. pingers-parties implements it
/// from the MLS roster; this crate cannot depend on pingers-mls (same layer).
pub trait RosterLookup {
    /// The signature key of the member at `leaf` in the Party's roster at `epoch`, if any.
    fn signature_key(&self, leaf: LeafIndex, epoch: Epoch) -> Option<AccountPublicKey>;
}

/// Everything a knock needs.
pub struct KnockInputs<'knock> {
    /// The matched record.
    pub matched: &'knock BoardMatch,
    /// The keys of the Spot it matched (at the matched epoch).
    pub spot: &'knock ActiveSpot,
    /// The knocker's own live record for this Spot, if it has one (for tie-breaks).
    pub own_record: Option<RecordRandomness>,
    /// The knocker's leaf in the Party group.
    pub own_leaf: LeafIndex,
    /// A fresh Meetup `KeyPackage` from pingers-mls.
    pub key_package: KeyPackageBytes,
    /// The knocker's one-time reply mailbox for the Welcome.
    pub reply_mailbox: MailboxId,
    /// The knocker's sealed subscription for the Meetup group, unless wake-up pushes are off.
    pub meetup_subscription: Option<Subscription>,
    /// The knocker's account key, which signs the knock.
    pub signing_key: &'knock AccountSigningKey,
    /// 1 KiB for classical Parties, 4 KiB for X-Wing Parties.
    pub class: SizeClass,
}

/// A sealed knock, ready to post anonymously.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingKnock {
    /// Where to post it: the matched record's knock mailbox.
    pub mailbox: MailboxId,
    /// The mailbox post body, exactly one size class long.
    pub post: Vec<u8>,
}

/// Signs, encodes, pads and seals a knock under `K_knock(b0, r)` of the matched record.
///
/// # Errors
///
/// [`PresenceError::Crypto`] or [`PresenceError::Proto`] if sealing or encoding fails
/// (including a `KeyPackage` too large for the class).
pub fn build_knock(
    inputs: &KnockInputs<'_>,
    rng: &mut impl CryptoRng,
) -> Result<OutgoingKnock, PresenceError> {
    not_implemented((inputs, rng))
}

/// The poster's side: which of its records a mailbox post may be answering.
pub struct KnockContext<'spot> {
    /// The keys of the Spot the record was for.
    pub spot: &'spot ActiveSpot,
    /// The record's start bucket.
    pub start: Bucket,
    /// The record's randomness.
    pub own_record: RecordRandomness,
}

/// A knock that opened under one of the poster's records but is not yet verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedKnock {
    /// The decoded knock.
    pub knock: Knock,
}

/// Opens a post read from a knock mailbox.
///
/// # Errors
///
/// [`PresenceError::Crypto`] if it does not open under this record's knock key;
/// [`PresenceError::Proto`] if the plaintext is not a knock.
pub fn open_knock(context: &KnockContext<'_>, post: &[u8]) -> Result<OpenedKnock, PresenceError> {
    not_implemented((context, post))
}

/// A knock whose signature verified against the Party roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedKnock {
    /// The knocker's leaf.
    pub knocker: LeafIndex,
    /// The knocker's signature key, which the Meetup roster check also uses.
    pub knocker_key: AccountPublicKey,
    /// The knock.
    pub knock: Knock,
}

/// Verifies an opened knock: it answers `context.own_record`, its signer is in the roster at
/// the knock's epoch, and the signature covers the Party, Spot and bucket of the record.
///
/// # Errors
///
/// [`PresenceError::KnockForOtherRecord`], [`PresenceError::UnknownKnocker`],
/// [`PresenceError::Crypto`] (bad signature).
pub fn verify_knock(
    context: &KnockContext<'_>,
    opened: OpenedKnock,
    roster: &impl RosterLookup,
) -> Result<VerifiedKnock, PresenceError> {
    if opened.knock.matched_randomness != context.own_record {
        return Err(PresenceError::KnockForOtherRecord);
    }
    let knock = &opened.knock;
    let Some(knocker_key) = roster.signature_key(knock.leaf_index, knock.party_epoch) else {
        return Err(PresenceError::UnknownKnocker);
    };
    not_implemented((context, knocker_key, opened))
}
