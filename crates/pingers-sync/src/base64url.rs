// SPDX-License-Identifier: AGPL-3.0-only
//! Unpadded base64url (RFC 4648 §5), for the `PingersRln` authorization header (design D).
//! Twenty lines are cheaper than a dependency for one fixed-size header.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The 6-bit group of `chunk` starting at bit `shift` from the top of the 24-bit word.
fn symbol(word: u32, shift: u32) -> char {
    let index = usize::try_from((word >> shift) & 0x3f).unwrap_or(0);
    ALPHABET.get(index).copied().map_or('A', char::from)
}

/// Encodes `bytes` without padding.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |word, (position, byte)| {
                word | (u32::from(*byte) << (16_usize.saturating_sub(position.saturating_mul(8))))
            });
        // n bytes carry n + 1 symbols.
        for (shift, _) in [18, 12, 6, 0].iter().zip(0..=chunk.len()) {
            text.push(symbol(word, *shift));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::encode;

    #[test]
    fn matches_the_rfc_4648_vectors_without_padding() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg");
        assert_eq!(encode(b"fo"), "Zm8");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg");
        assert_eq!(encode(b"fooba"), "Zm9vYmE");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn uses_the_url_safe_alphabet() {
        assert_eq!(encode(&[0xfb, 0xff, 0xbf]), "-_-_");
        assert_eq!(encode(&[0x00, 0x10, 0x83]), "ABCD");
    }
}
