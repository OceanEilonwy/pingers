// SPDX-License-Identifier: AGPL-3.0-only
//! The shape every secret type in this crate shares.

/// Defines a secret key type over `Zeroizing<[u8; N]>`: no `Debug`, `Display`, `Clone` or
/// `Serialize` (style guide §8).
///
/// `$access` is the visibility of `from_bytes` and `expose_bytes`: `pub` for keys another
/// crate must store or hand to a library (each says why in its doc), `pub(crate)` otherwise.
macro_rules! secret_key {
    ($(#[$meta:meta])* $name:ident, $length:literal, $access:vis) => {
        $(#[$meta])*
        pub struct $name(zeroize::Zeroizing<[u8; $length]>);

        impl $name {
            /// Length in bytes.
            pub const LENGTH: usize = $length;

            /// Wraps key bytes, taking ownership so they are zeroed when dropped.
            #[must_use]
            $access fn from_bytes(bytes: zeroize::Zeroizing<[u8; $length]>) -> Self {
                Self(bytes)
            }

            /// The key bytes. See the type's documentation for who may call this and why.
            #[must_use]
            $access fn expose_bytes(&self) -> &[u8; $length] {
                &self.0
            }
        }
    };
}

/// Adds `generate` to a secret key type defined with [`secret_key!`].
macro_rules! random_secret {
    ($name:ident) => {
        impl $name {
            /// A fresh key from `rng`.
            #[must_use]
            pub fn generate(rng: &mut impl rand_core::CryptoRng) -> Self {
                let mut bytes = zeroize::Zeroizing::new([0_u8; Self::LENGTH]);
                rng.fill_bytes(bytes.as_mut_slice());
                Self(bytes)
            }
        }
    };
}

pub(crate) use {random_secret, secret_key};
