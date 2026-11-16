// SPDX-License-Identifier: AGPL-3.0-only
//! The key a group subscription is bound to (design D, "Subscription owner").
//!
//! Every member can read every other member's subscription id (Meetup leaves carry it), and
//! every member holds the group's log-auth key, so the group signature alone would let any
//! member replace, delete or take over another's subscription. The server therefore binds each
//! subscription to the owner key it carries when first stored, and a change to it needs a
//! second signature, by that key, over the same request.

use ed25519_dalek::SigningKey;
use pingers_proto::{SubscriptionId, SubscriptionOwner};

use crate::group::{GroupRequest, request_message};
use crate::identity::AccountSigningKey;
use crate::secret::secret_key;
use crate::{CryptoError, Signature, labels, primitives};

secret_key!(
    /// The seed of the Ed25519 key one subscription is bound to:
    /// `HKDF-Expand(account key seed, "pingers/v1/subscription-owner" || sub_id, 32)`. Derived
    /// again whenever needed, so nothing new is stored; it links to neither the account key
    /// nor another subscription.
    SubscriptionOwnerKey,
    32,
    pub(crate)
);

impl SubscriptionOwnerKey {
    /// The owner key of subscription `id` for the account `account`.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if the derivation fails.
    pub fn derive(account: &AccountSigningKey, id: &SubscriptionId) -> Result<Self, CryptoError> {
        let seed = primitives::hkdf_expand_32(
            account.expose_bytes(),
            &[labels::SUBSCRIPTION_OWNER, id.as_bytes()],
        )?;
        Ok(Self::from_bytes(seed))
    }

    /// The public half, carried in the subscription.
    #[must_use]
    pub fn public_key(&self) -> SubscriptionOwner {
        SubscriptionOwner::from_bytes(
            SigningKey::from_bytes(self.expose_bytes())
                .verifying_key()
                .to_bytes(),
        )
    }
}

/// Signs a group request with a subscription's owner key, as the second signature on a
/// request that changes or speaks for that subscription.
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the method or path is longer than 2³² − 1 bytes.
pub fn sign_subscription_request(
    key: &SubscriptionOwnerKey,
    request: &GroupRequest<'_>,
) -> Result<Signature, CryptoError> {
    let message = request_message(labels::SUBSCRIPTION_REQUEST, request)?;
    Ok(Signature::from_bytes(primitives::ed25519_sign(
        key.expose_bytes(),
        &message,
    )))
}

/// Verifies a subscription owner's signature over a group request (server side).
///
/// # Errors
///
/// [`CryptoError::BadSignature`] if it does not verify;
/// [`CryptoError::WrongLength`] if the method or path is longer than 2³² − 1 bytes.
pub fn verify_subscription_request(
    owner: &SubscriptionOwner,
    request: &GroupRequest<'_>,
    signature: &Signature,
) -> Result<(), CryptoError> {
    let message = request_message(labels::SUBSCRIPTION_REQUEST, request)?;
    primitives::ed25519_verify(owner.as_bytes(), &message, signature.as_bytes())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::UnixSeconds;
    use zeroize::Zeroizing;

    use super::*;
    use crate::group::sign_group_request;
    use crate::{LogAuthKeyPair, verify_group_request};

    fn request() -> GroupRequest<'static> {
        GroupRequest {
            method: "PUT",
            path: "/v1/groups/00/subs/01",
            body: b"body",
            timestamp: UnixSeconds::new(1_800_000_000),
        }
    }

    #[test]
    fn each_subscription_has_its_own_owner_key_and_only_it_verifies() -> Result<(), CryptoError> {
        let account = AccountSigningKey::from_bytes(Zeroizing::new([1; 32]));
        let first = SubscriptionOwnerKey::derive(&account, &SubscriptionId::from_bytes([2; 16]))?;
        let again = SubscriptionOwnerKey::derive(&account, &SubscriptionId::from_bytes([2; 16]))?;
        let other = SubscriptionOwnerKey::derive(&account, &SubscriptionId::from_bytes([3; 16]))?;
        assert_eq!(first.public_key(), again.public_key());
        assert_ne!(first.public_key(), other.public_key());
        assert_ne!(
            first.public_key().as_bytes(),
            account.public_key().as_bytes()
        );
        let signature = sign_subscription_request(&first, &request())?;
        verify_subscription_request(&first.public_key(), &request(), &signature)?;
        assert!(verify_subscription_request(&other.public_key(), &request(), &signature).is_err());
        let changed = GroupRequest {
            body: b"other",
            ..request()
        };
        assert!(verify_subscription_request(&first.public_key(), &changed, &signature).is_err());
        Ok(())
    }

    #[test]
    fn an_owner_signature_is_not_a_group_signature() -> Result<(), CryptoError> {
        // The same seed as a log-auth key: the two signatures still differ by their label.
        let owner = SubscriptionOwnerKey::from_bytes(Zeroizing::new([4; 32]));
        let group = LogAuthKeyPair::from_bytes(Zeroizing::new([4; 32]));
        let signature = sign_subscription_request(&owner, &request())?;
        assert!(verify_group_request(&group.public_key(), &request(), &signature).is_err());
        let group_signature = sign_group_request(&group, &request())?;
        assert!(
            verify_subscription_request(&owner.public_key(), &request(), &group_signature).is_err()
        );
        Ok(())
    }
}
