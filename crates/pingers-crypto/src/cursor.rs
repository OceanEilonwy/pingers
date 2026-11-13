// SPDX-License-Identifier: AGPL-3.0-only
//! Board cursors (design B.4.3, D: "opaque, server-MACed"): the board store seals a reader's
//! position so only it can read or change it.
//!
//! `cursor = nonce (24, random) || XChaCha20-Poly1305(K_cursor, nonce,
//! aad = "pingers/v1/board-cursor", position)`. Authenticated encryption gives the MAC the
//! design asks for and keeps positions opaque. The label in the AAD keeps a cursor from ever
//! opening as a state snapshot (empty AAD) or the reverse, even under the same key bytes.

use pingers_proto::BoardCursor;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::primitives::{self, XNONCE_LENGTH};
use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels};

secret_key!(
    /// The board store's cursor key. Generated at start-up with `generate` and never stored:
    /// the board lives in memory, so cursors die with it. `from_bytes` and `expose_bytes` are
    /// public so pingers-board-store's tests can use a fixed key.
    BoardCursorKey,
    32,
    pub
);
random_secret!(BoardCursorKey);

/// Seals an encoded read position into an opaque cursor.
///
/// # Errors
///
/// [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_board_cursor(
    key: &BoardCursorKey,
    position: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<BoardCursor, CryptoError> {
    let mut nonce = [0; XNONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    let ciphertext =
        primitives::xchacha_seal(key.expose_bytes(), &nonce, labels::BOARD_CURSOR, position)?;
    Ok(BoardCursor::from_bytes(
        [nonce.as_slice(), &ciphertext].concat(),
    ))
}

/// Opens a cursor sealed by [`seal_board_cursor`] under the same key.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was tampered with, truncated, sealed under another key,
/// or is not a cursor.
pub fn open_board_cursor(
    key: &BoardCursorKey,
    cursor: &BoardCursor,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let (nonce, ciphertext) = cursor
        .as_bytes()
        .split_first_chunk::<XNONCE_LENGTH>()
        .ok_or(CryptoError::OpenFailed)?;
    primitives::xchacha_open(key.expose_bytes(), nonce, labels::BOARD_CURSOR, ciphertext)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::SealedSnapshot;
    use pingers_testkit::seeded_rng;

    use super::*;
    use crate::{StateKey, open_state_snapshot, seal_state_snapshot};

    fn key(byte: u8) -> BoardCursorKey {
        BoardCursorKey::from_bytes(Zeroizing::new([byte; 32]))
    }

    #[test]
    fn a_cursor_opens_only_under_its_own_key_and_untampered() -> Result<(), CryptoError> {
        let cursor = seal_board_cursor(&key(1), b"position", &mut seeded_rng(1))?;
        assert_eq!(*open_board_cursor(&key(1), &cursor)?, b"position");
        assert!(matches!(
            open_board_cursor(&key(2), &cursor),
            Err(CryptoError::OpenFailed)
        ));
        let mut tampered = cursor.as_bytes().to_vec();
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        assert!(matches!(
            open_board_cursor(&key(1), &BoardCursor::from_bytes(tampered)),
            Err(CryptoError::OpenFailed)
        ));
        assert!(matches!(
            open_board_cursor(&key(1), &BoardCursor::from_bytes(vec![0; 23])),
            Err(CryptoError::OpenFailed)
        ));
        Ok(())
    }

    #[test]
    fn cursors_and_snapshots_never_open_as_each_other() -> Result<(), CryptoError> {
        // The same 32 key bytes in both roles: the label in the AAD separates them.
        let cursor = seal_board_cursor(&key(7), b"position", &mut seeded_rng(1))?;
        let state_key = StateKey::from_bytes(Zeroizing::new([7; 32]));
        assert!(matches!(
            open_state_snapshot(
                &state_key,
                &SealedSnapshot::from_bytes(cursor.as_bytes().to_vec())
            ),
            Err(CryptoError::OpenFailed)
        ));
        let snapshot = seal_state_snapshot(&state_key, b"position", &mut seeded_rng(1))?;
        assert!(matches!(
            open_board_cursor(&key(7), &BoardCursor::from_bytes(snapshot.into_bytes())),
            Err(CryptoError::OpenFailed)
        ));
        Ok(())
    }
}
