// SPDX-License-Identifier: AGPL-3.0-only
//! Knocks: built by the newcomer, opened and verified by the poster (design B.4.2, B.4.4).

use pingers_crypto::{
    AccountPublicKey, AccountSigningKey, KnockKey, KnockPlaintext, KnockSignedFields,
    derive_knock_key, derive_seal_key, seal_knock, sign_knock,
};
use pingers_proto::{
    Epoch, KeyPackageBytes, Knock, LeafIndex, MailboxId, RecordRandomness, SizeClass, Subscription,
    WireDecode as _, WireEncode as _,
};
use pingers_time::Bucket;
use rand_core::CryptoRng;

use crate::{ActiveSpot, BoardMatch, PresenceError};

/// Looks up Party members' signature keys in the MLS roster. pingers-parties implements it;
/// this crate cannot depend on pingers-mls (same layer).
pub trait RosterLookup {
    /// The signature key of the member at `leaf` in the Party's roster at `epoch`, if any.
    fn signature_key(&self, leaf: LeafIndex, epoch: Epoch) -> Option<AccountPublicKey>;

    /// The newest Party epoch this device has processed. A knocker must still hold its leaf,
    /// with the same key, at this epoch, so a member removed since the knock's epoch is
    /// rejected (design A.3, "Removed member").
    fn current_epoch(&self) -> Epoch;
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
/// The knock's `party_epoch` is the epoch of the keys it matched under (`inputs.spot`): the
/// poster opens it with that epoch's keys and checks the roster at that epoch.
///
/// # Errors
///
/// [`PresenceError::SpotMismatch`] if `inputs.spot` is not the Spot and epoch of the match;
/// [`PresenceError::Crypto`] or [`PresenceError::Proto`] if sealing or encoding fails
/// (including a `KeyPackage` too large for the class).
pub fn build_knock(
    inputs: &KnockInputs<'_>,
    rng: &mut impl CryptoRng,
) -> Result<OutgoingKnock, PresenceError> {
    let matched = inputs.matched;
    let spot = inputs.spot;
    if spot.spot() != matched.spot || spot.epoch() != matched.epoch {
        return Err(PresenceError::SpotMismatch);
    }
    let record = RecordRef {
        spot,
        start: matched.start,
        randomness: &matched.record.randomness,
    };
    let signature = sign_knock(
        inputs.signing_key,
        &record.signed_fields(&KnockBody {
            key_package: &inputs.key_package,
            reply_mailbox: &inputs.reply_mailbox,
            meetup_subscription: inputs.meetup_subscription.as_ref(),
        }),
    )?;
    let knock = Knock {
        matched_randomness: matched.record.randomness,
        own_randomness: inputs.own_record,
        leaf_index: inputs.own_leaf,
        party_epoch: spot.epoch(),
        key_package: inputs.key_package.clone(),
        reply_mailbox: inputs.reply_mailbox,
        meetup_subscription: inputs.meetup_subscription.clone(),
        signature,
    };
    let encoded = knock.encode()?;
    let post = seal_knock(
        &record.knock_key()?,
        &KnockPlaintext {
            encoded: &encoded,
            class: inputs.class,
        },
        rng,
    )?;
    Ok(OutgoingKnock {
        mailbox: matched.beacon.knock_mailbox,
        post,
    })
}

/// One record, as both sides of a knock name it.
struct RecordRef<'record> {
    spot: &'record ActiveSpot,
    start: Bucket,
    randomness: &'record RecordRandomness,
}

/// The parts of a knock its signature covers besides the record.
struct KnockBody<'knock> {
    key_package: &'knock KeyPackageBytes,
    reply_mailbox: &'knock MailboxId,
    meetup_subscription: Option<&'knock Subscription>,
}

impl<'record> RecordRef<'record> {
    fn signed_fields(&self, body: &KnockBody<'record>) -> KnockSignedFields<'record> {
        let spot = self.spot.spot_ref();
        KnockSignedFields {
            party: &spot.party,
            epoch: self.spot.epoch(),
            spot: &spot.spot,
            start: self.start,
            matched: self.randomness,
            key_package: body.key_package,
            reply_mailbox: body.reply_mailbox,
            meetup_subscription: body.meetup_subscription,
        }
    }

    /// `K_knock(b0, r)`.
    fn knock_key(&self) -> Result<KnockKey, PresenceError> {
        let seal_key = derive_seal_key(self.spot.spot_key(), self.start)?;
        Ok(derive_knock_key(&seal_key, self.randomness)?)
    }
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

impl KnockContext<'_> {
    const fn record(&self) -> RecordRef<'_> {
        RecordRef {
            spot: self.spot,
            start: self.start,
            randomness: &self.own_record,
        }
    }
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
    let encoded = pingers_crypto::open_knock(&context.record().knock_key()?, post)?;
    Ok(OpenedKnock {
        knock: Knock::decode(&encoded)?,
    })
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

/// Verifies an opened knock: it answers `context.own_record`, it names the epoch of the
/// record's keys, its signer is in the roster at that epoch and still holds the same leaf and
/// key at the roster's current epoch, and the signature covers the Party, Spot and bucket of
/// the record.
///
/// # Errors
///
/// [`PresenceError::KnockForOtherRecord`], [`PresenceError::WrongEpoch`],
/// [`PresenceError::UnknownKnocker`], [`PresenceError::Crypto`] (bad signature).
pub fn verify_knock(
    context: &KnockContext<'_>,
    opened: OpenedKnock,
    roster: &impl RosterLookup,
) -> Result<VerifiedKnock, PresenceError> {
    if opened.knock.matched_randomness != context.own_record {
        return Err(PresenceError::KnockForOtherRecord);
    }
    let knock = &opened.knock;
    if knock.party_epoch != context.spot.epoch() {
        return Err(PresenceError::WrongEpoch);
    }
    let Some(knocker_key) = roster.signature_key(knock.leaf_index, knock.party_epoch) else {
        return Err(PresenceError::UnknownKnocker);
    };
    if roster.signature_key(knock.leaf_index, roster.current_epoch()) != Some(knocker_key) {
        return Err(PresenceError::UnknownKnocker);
    }
    let fields = context.record().signed_fields(&KnockBody {
        key_package: &knock.key_package,
        reply_mailbox: &knock.reply_mailbox,
        meetup_subscription: knock.meetup_subscription.as_ref(),
    });
    pingers_crypto::verify_knock(&knocker_key, &fields, &knock.signature)?;
    Ok(VerifiedKnock {
        knocker: knock.leaf_index,
        knocker_key,
        knock: opened.knock,
    })
}
