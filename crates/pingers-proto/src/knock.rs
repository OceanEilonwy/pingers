// SPDX-License-Identifier: AGPL-3.0-only
//! The knock a newcomer posts to a matched record's mailbox (design B.4.2, B.4.4).

use minicbor::Decoder;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{
    Epoch, KeyPackageBytes, LeafIndex, MailboxId, ProtoError, RecordRandomness, Subscription,
};

/// The knocker's Ed25519 leaf signature over the knock's signed fields (design B.4.2). Which
/// fields, in which order, is pingers-crypto's job; this type only carries the 64 bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KnockSignature([u8; 64]);

impl KnockSignature {
    /// Wraps signature bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// The signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl core::fmt::Debug for KnockSignature {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("KnockSignature(..)")
    }
}

/// The knock plaintext (design B.4.2). Encoded as CBOR, padded to 1 KiB (4 KiB for X-Wing
/// Parties) and sealed by pingers-crypto under `K_knock(b0, r)` of the matched record.
///
/// It names the knocker, to the poster only: the signature is by the knocker's leaf key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Knock {
    /// The record being answered; stops a knock being replayed against another record.
    pub matched_randomness: RecordRandomness,
    /// The knocker's own record for this Spot, if it has one, for tie-breaks.
    pub own_randomness: Option<RecordRandomness>,
    /// The knocker's leaf in the Party group.
    pub leaf_index: LeafIndex,
    /// The Party epoch the knocker signed under.
    pub party_epoch: Epoch,
    /// A fresh `KeyPackage` for the Meetup group, with the same credential as the Party leaf.
    pub key_package: KeyPackageBytes,
    /// The knocker's one-time mailbox for the Welcome.
    pub reply_mailbox: MailboxId,
    /// The knocker's sealed push subscription for the Meetup group (design B.4.9).
    pub meetup_subscription: Option<Subscription>,
    /// The knocker's leaf signature.
    pub signature: KnockSignature,
}

/// Layout: `[matched r, own r or null, leaf index, party epoch, key package, reply mailbox,
/// subscription or null, signature]`.
impl CborBody for Knock {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(8)?;
        writer.bytes(self.matched_randomness.as_bytes())?;
        writer.option(self.own_randomness.as_ref(), |writer, randomness| {
            writer.bytes(randomness.as_bytes())
        })?;
        writer.unsigned(self.leaf_index.get());
        writer.unsigned(self.party_epoch.get());
        writer.bytes(self.key_package.as_bytes())?;
        writer.bytes(self.reply_mailbox.as_bytes())?;
        writer.option(self.meetup_subscription.as_ref(), |writer, subscription| {
            subscription.encode_body(writer)
        })?;
        writer.bytes(self.signature.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 8)?;
        Ok(Self {
            matched_randomness: RecordRandomness::from_bytes(cbor::fixed_bytes(decoder)?),
            own_randomness: cbor::option(decoder, |decoder| {
                cbor::fixed_bytes(decoder).map(RecordRandomness::from_bytes)
            })?,
            leaf_index: LeafIndex::new(decoder.u32()?),
            party_epoch: Epoch::new(decoder.u64()?),
            key_package: KeyPackageBytes::from_bytes(decoder.bytes()?.to_vec()),
            reply_mailbox: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            meetup_subscription: cbor::option(decoder, Subscription::decode_body)?,
            signature: KnockSignature::from_bytes(cbor::fixed_bytes(decoder)?),
        })
    }
}

versioned_cbor!(Knock);
