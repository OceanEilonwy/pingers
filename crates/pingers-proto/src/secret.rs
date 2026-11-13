// SPDX-License-Identifier: AGPL-3.0-only
//! Secrets that travel on the wire: bearer tokens, registration codes, revoke keys, invite
//! secrets.
//!
//! Unlike [`crate::Redacted`], these are zeroed on drop, implement neither `PartialEq` nor
//! `Default`, and compare only through [`ConstantTimeEq`] (style guide §8), so a server cannot
//! check a presented secret with a timing-leaky `==`. `Debug` prints only the type.

use subtle::{Choice, ConstantTimeEq};
use zeroize::Zeroizing;

/// A fixed-length secret byte string, such as a revoke key or an invite secret.
#[derive(Clone)]
pub struct SecretBytes<const LENGTH: usize>(Zeroizing<[u8; LENGTH]>);

impl<const LENGTH: usize> SecretBytes<LENGTH> {
    /// Wraps secret bytes.
    #[must_use]
    pub fn new(bytes: [u8; LENGTH]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The secret bytes, to encode or to hand to pingers-crypto.
    #[must_use]
    pub fn expose(&self) -> &[u8; LENGTH] {
        &self.0
    }
}

impl<const LENGTH: usize> ConstantTimeEq for SecretBytes<LENGTH> {
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.as_slice().ct_eq(other.0.as_slice())
    }
}

impl<const LENGTH: usize> core::fmt::Debug for SecretBytes<LENGTH> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("SecretBytes(redacted)")
    }
}

/// A secret text, such as a bearer token or a registration code.
#[derive(Clone)]
pub struct SecretText(Zeroizing<String>);

impl SecretText {
    /// Wraps secret text.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    /// The secret text, to encode or to send.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Constant time in the contents; texts of different lengths are unequal, and their lengths
/// are not secret.
impl ConstantTimeEq for SecretText {
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.as_bytes().ct_eq(other.0.as_bytes())
    }
}

impl core::fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("SecretText(redacted)")
    }
}

/// `Option` of a secret, compared in constant time when both are present.
pub(crate) fn option_ct_eq<T: ConstantTimeEq>(left: Option<&T>, right: Option<&T>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => bool::from(left.ct_eq(right)),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_compare_by_content() {
        assert!(bool::from(
            SecretBytes::new([1; 4]).ct_eq(&SecretBytes::new([1; 4]))
        ));
        assert!(!bool::from(
            SecretBytes::new([1; 4]).ct_eq(&SecretBytes::new([2; 4]))
        ));
        let token = SecretText::new(String::from("bearer-token"));
        assert!(bool::from(
            token.ct_eq(&SecretText::new(String::from("bearer-token")))
        ));
        assert!(!bool::from(
            token.ct_eq(&SecretText::new(String::from("bearer-tokeN")))
        ));
        assert!(!bool::from(
            token.ct_eq(&SecretText::new(String::from("bearer")))
        ));
    }

    #[test]
    fn optional_secrets_compare_by_presence_then_content() {
        let one = SecretText::new(String::from("a"));
        let other = SecretText::new(String::from("b"));
        assert!(option_ct_eq::<SecretText>(None, None));
        assert!(option_ct_eq(Some(&one), Some(&one.clone())));
        assert!(!option_ct_eq(Some(&one), Some(&other)));
        assert!(!option_ct_eq(Some(&one), None));
        assert!(!option_ct_eq(None, Some(&one)));
    }

    #[test]
    fn debug_hides_secrets() {
        assert_eq!(
            format!("{:?}", SecretBytes::new([7; 32])),
            "SecretBytes(redacted)"
        );
        assert_eq!(
            format!("{:?}", SecretText::new(String::from("hunter2"))),
            "SecretText(redacted)"
        );
    }
}
