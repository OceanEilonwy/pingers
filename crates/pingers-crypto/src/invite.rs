// SPDX-License-Identifier: AGPL-3.0-only
//! Invite join requests (design D, invite flow).

use pingers_proto::SizeClass;
use rand_core::CryptoRng;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::primitives::{self, XNONCE_LENGTH};
use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels, padded};

secret_key!(
    /// The 32-byte secret in an invite link's fragment. Possession authenticates a join
    /// request. `from_bytes` and `expose_bytes` are public because pingers-parties puts it
    /// into and reads it from the link, and pingers-store keeps it until the invite expires.
    InviteSecret,
    32,
    pub
);
random_secret!(InviteSecret);

/// Seals a join request to an invite, padded inside the AEAD to the smallest mailbox size
/// class that fits (1, 4, 8 or 32 KiB): `nonce (24, random) ||
/// XChaCha20-Poly1305(HKDF(invite_secret, "join"), be16(len) || request || zero padding)`.
/// The post is exactly one class long, so it reveals only the class, not the request size.
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the request does not fit 32 KiB;
/// [`CryptoError::Internal`] if HKDF or the AEAD fails.
pub fn seal_join_request(
    secret: &InviteSecret,
    encoded_request: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let class = SizeClass::MAILBOX_POST
        .into_iter()
        .find(|class| encoded_request.len() <= padded::capacity(*class))
        .ok_or(CryptoError::WrongLength {
            expected: padded::capacity(SizeClass::Bytes32768),
            actual: encoded_request.len(),
        })?;
    seal_join_request_in_class(secret, encoded_request, class, rng)
}

/// As [`seal_join_request`], padded to `class`, for callers that want every join request of a
/// Party to look the same (for example 4 KiB when X-Wing key packages are possible).
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the request does not fit `class`;
/// [`CryptoError::Internal`] if HKDF or the AEAD fails.
pub fn seal_join_request_in_class(
    secret: &InviteSecret,
    encoded_request: &[u8],
    class: SizeClass,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0; XNONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    padded::seal(&*join_key(secret)?, &nonce, encoded_request, class)
}

/// `HKDF-Expand(invite_secret, "join", 32)`: the invite secret is already uniformly random,
/// so it serves as the PRK, as the Spot keys do.
pub(crate) fn join_key(secret: &InviteSecret) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    primitives::hkdf_expand_32(secret.expose_bytes(), &[labels::INVITE_JOIN])
}

/// Opens a join request posted to the invite mailbox and strips its padding.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was not sealed with this invite's secret;
/// [`CryptoError::WrongLength`] if the padding inside is malformed.
pub fn open_join_request(
    secret: &InviteSecret,
    sealed: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    padded::open(&*join_key(secret)?, sealed)
}

/// How many decimal digits a [`JoinCode`] has.
const JOIN_CODE_DIGITS: usize = 16;
/// `10^16`: the join code is the MAC's first eight bytes modulo this (a bias below 2^-10
/// towards low codes, which costs a guesser nothing: one chance in about `10^16` stays).
const JOIN_CODE_MODULUS: u64 = 10_000_000_000_000_000;

/// The 16-digit code both phones show while someone asks to join (design D, invite flow): the
/// joiner reads it to the inviter, who lets them in only if it matches. It binds the request
/// to the link's secret and to the joiner's key package, so someone else who opened the link
/// gets a different code.
///
/// `Display` shows four groups of four digits ("4821 0937 5512 6604"); `Debug` shows nothing.
/// Equality is constant-time, as for any MAC.
#[derive(Clone, Copy)]
pub struct JoinCode(u64);

impl JoinCode {
    /// Rebuilds a code from its value, as stored. `None` if it has more than 16 digits.
    #[must_use]
    pub const fn from_value(value: u64) -> Option<Self> {
        if value < JOIN_CODE_MODULUS {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The code as a number below `10^16`, for storage.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// The 16 digits, most significant first.
    #[must_use]
    pub fn digits(self) -> [u8; JOIN_CODE_DIGITS] {
        let mut digits = [0; JOIN_CODE_DIGITS];
        let mut rest = self.0;
        for slot in digits.iter_mut().rev() {
            // Below 10, so the conversion cannot fail.
            *slot = u8::try_from(rest % 10).unwrap_or(0);
            rest /= 10;
        }
        digits
    }
}

impl PartialEq for JoinCode {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for JoinCode {}

impl core::fmt::Display for JoinCode {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (index, digit) in self.digits().iter().enumerate() {
            if index > 0 && index % 4 == 0 {
                formatter.write_str(" ")?;
            }
            write!(formatter, "{digit}")?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for JoinCode {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("JoinCode(redacted)")
    }
}

/// The key packages a join request carries: a classical one always, and an X-Wing one when the
/// joiner's build can make it (the link does not say which suite the Party uses).
#[derive(Clone, Copy, Debug)]
pub struct JoinKeyPackages<'packages> {
    /// The classical key package, as carried.
    pub classical: &'packages [u8],
    /// The X-Wing key package, as carried, or `None`.
    pub quantum_resistant: Option<&'packages [u8]>,
}

/// The join code for a request: `be64(HMAC-SHA256(HKDF-Expand(invite_secret,
/// "pingers/v1/join-code", 32), be32(len(classical)) || classical || be32(len(x_wing)) ||
/// x_wing)[..8]) mod 10^16`, where `x_wing` is empty when the request carries none.
///
/// The joiner computes it when it builds the request and the inviter when it opens it, so both
/// show it before anyone is let in. It covers both key packages, so whichever one the Party's
/// suite makes the inviter add is covered. Each carries the joiner's signature and init keys and
/// is signed by the former, so the code names exactly the leaf the inviter would add.
/// Someone who intercepted the link cannot read the real joiner's request (only the inviter
/// holds the invite mailbox's read key), so they can only guess: one chance in `10^16`.
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if a key package is 4 GiB or longer; [`CryptoError::Internal`]
/// if HKDF or HMAC fails.
pub fn join_code(
    secret: &InviteSecret,
    key_packages: JoinKeyPackages<'_>,
) -> Result<JoinCode, CryptoError> {
    let key = join_code_key(secret)?;
    let classical = key_packages.classical;
    let quantum_resistant = key_packages.quantum_resistant.unwrap_or_default();
    let mac = primitives::hmac_sha256(
        &key,
        &[
            &length_prefix(classical)?,
            classical,
            &length_prefix(quantum_resistant)?,
            quantum_resistant,
        ],
    )?;
    let mut first = [0; 8];
    for (target, source) in first.iter_mut().zip(mac.iter()) {
        *target = *source;
    }
    Ok(JoinCode(u64::from_be_bytes(first) % JOIN_CODE_MODULUS))
}

/// `be32(len(bytes))`.
fn length_prefix(bytes: &[u8]) -> Result<[u8; 4], CryptoError> {
    u32::try_from(bytes.len())
        .map(u32::to_be_bytes)
        .map_err(|_too_long| CryptoError::WrongLength {
            expected: usize::try_from(u32::MAX).unwrap_or(usize::MAX),
            actual: bytes.len(),
        })
}

/// `HKDF-Expand(invite_secret, "pingers/v1/join-code", 32)`.
pub(crate) fn join_code_key(secret: &InviteSecret) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    primitives::hkdf_expand_32(secret.expose_bytes(), &[labels::JOIN_CODE])
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn stored_codes_must_have_sixteen_digits() {
        assert_eq!(
            JoinCode::from_value(9_999_999_999_999_999).map(JoinCode::value),
            Some(9_999_999_999_999_999)
        );
        assert!(JoinCode::from_value(JOIN_CODE_MODULUS).is_none());
        assert_eq!(
            JoinCode::from_value(7).map(JoinCode::digits),
            Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7])
        );
    }

    fn classical(key_package: &[u8]) -> JoinKeyPackages<'_> {
        JoinKeyPackages {
            classical: key_package,
            quantum_resistant: None,
        }
    }

    #[test]
    fn codes_differ_by_key_package_and_secret_and_hide_in_debug() -> Result<(), CryptoError> {
        let secret = InviteSecret::from_bytes(Zeroizing::new([1; 32]));
        let other = InviteSecret::from_bytes(Zeroizing::new([2; 32]));
        let code = join_code(&secret, classical(b"key package"))?;
        assert_eq!(code, join_code(&secret, classical(b"key package"))?);
        assert_ne!(code, join_code(&secret, classical(b"key packagf"))?);
        assert_ne!(code, join_code(&other, classical(b"key package"))?);
        // The X-Wing key package is covered too, and the length prefixes keep the boundary
        // between the two from sliding.
        let both = |x_wing: &'static [u8]| JoinKeyPackages {
            classical: b"key package",
            quantum_resistant: Some(x_wing),
        };
        assert_ne!(code, join_code(&secret, both(b"x-wing"))?);
        assert_ne!(
            join_code(&secret, both(b"x-wing"))?,
            join_code(&secret, both(b"x-winh"))?
        );
        let slid = JoinKeyPackages {
            classical: b"key packagex",
            quantum_resistant: Some(b"-wing"),
        };
        assert_ne!(
            join_code(&secret, both(b"x-wing"))?,
            join_code(&secret, slid)?
        );
        assert_eq!(format!("{code:?}"), "JoinCode(redacted)");
        Ok(())
    }
}
