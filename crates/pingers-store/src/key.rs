// SPDX-License-Identifier: AGPL-3.0-only
//! The database key.

use core::fmt::Write as _;

use zeroize::Zeroizing;

/// The 32-byte SQLCipher key (design C.2, A.4).
///
/// Generated on first run by the shell, which keeps it wrapped by the Android Keystore and
/// hands it to the core on every start; the store never sees a platform API. Used as a raw key
/// (`x'...'`), so SQLCipher skips its password KDF: the key is already uniformly random.
/// No `Debug`, `Clone` or `Display` (style guide §8).
pub struct DatabaseKey(Zeroizing<[u8; 32]>);

impl DatabaseKey {
    /// Length in bytes.
    pub const LENGTH: usize = 32;

    /// Wraps the key bytes, taking ownership so they are zeroed when dropped.
    #[must_use]
    pub fn from_bytes(bytes: Zeroizing<[u8; 32]>) -> Self {
        Self(bytes)
    }

    /// The `PRAGMA key` statement, in memory that is zeroed on drop.
    pub(crate) fn pragma(&self) -> Zeroizing<String> {
        let mut statement = Zeroizing::new(String::with_capacity(80));
        statement.push_str("PRAGMA key = \"x'");
        for byte in self.0.iter() {
            // Writing to a String cannot fail.
            let _ = write!(statement, "{byte:02x}");
        }
        statement.push_str("'\";");
        statement
    }

    /// A copy for a new session. Kept crate-private so the key never leaves the store.
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pragma_is_a_raw_hex_key() {
        let mut bytes = [0_u8; 32];
        bytes[0] = 0xab;
        bytes[31] = 0x01;
        let key = DatabaseKey::from_bytes(Zeroizing::new(bytes));
        let expected = format!("PRAGMA key = \"x'ab{}01'\";", "00".repeat(30));
        assert_eq!(key.pragma().as_str(), expected);
    }
}
