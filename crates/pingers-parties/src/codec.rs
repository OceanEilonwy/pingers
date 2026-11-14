// SPDX-License-Identifier: AGPL-3.0-only
//! A small fixed-layout byte codec for this crate's local records (settings, the pending
//! commit, invites, roster history, subscriptions), which never leave the device. Wire values
//! (Party operations, the state snapshot) are pingers-proto's. Integers are big-endian;
//! variable-length fields carry a `be32` length.

use pingers_crypto::AccountPublicKey;

use crate::PartiesError;

pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn fixed(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(crate) fn variable(&mut self, value: &[u8]) -> Result<(), PartiesError> {
        self.count(value.len())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    pub(crate) fn count(&mut self, count: usize) -> Result<(), PartiesError> {
        self.u32(u32::try_from(count).map_err(|_| PartiesError::Internal)?);
        Ok(())
    }

    pub(crate) fn key(&mut self, key: &AccountPublicKey) {
        self.fixed(key.as_bytes());
    }
}

pub(crate) struct Reader<'bytes> {
    rest: &'bytes [u8],
    what: &'static str,
}

impl<'bytes> Reader<'bytes> {
    pub(crate) const fn new(bytes: &'bytes [u8], what: &'static str) -> Self {
        Self { rest: bytes, what }
    }

    pub(crate) const fn malformed(&self) -> PartiesError {
        PartiesError::Malformed { what: self.what }
    }

    /// Fails unless every byte was read.
    pub(crate) const fn end(&self) -> Result<(), PartiesError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(self.malformed())
        }
    }

    pub(crate) fn fixed<const N: usize>(&mut self) -> Result<[u8; N], PartiesError> {
        let (head, rest) = self
            .rest
            .split_first_chunk::<N>()
            .ok_or(PartiesError::Malformed { what: self.what })?;
        self.rest = rest;
        Ok(*head)
    }

    pub(crate) fn byte(&mut self) -> Result<u8, PartiesError> {
        self.fixed::<1>().map(|[byte]| byte)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, PartiesError> {
        self.fixed().map(u32::from_be_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, PartiesError> {
        self.fixed().map(u64::from_be_bytes)
    }

    pub(crate) fn count(&mut self) -> Result<usize, PartiesError> {
        usize::try_from(self.u32()?).map_err(|_| self.malformed())
    }

    pub(crate) fn variable(&mut self) -> Result<&'bytes [u8], PartiesError> {
        let length = self.count()?;
        let (head, rest) = self
            .rest
            .split_at_checked(length)
            .ok_or(PartiesError::Malformed { what: self.what })?;
        self.rest = rest;
        Ok(head)
    }

    pub(crate) fn text(&mut self) -> Result<String, PartiesError> {
        let bytes = self.variable()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| self.malformed())
    }

    pub(crate) fn key(&mut self) -> Result<AccountPublicKey, PartiesError> {
        self.fixed().map(AccountPublicKey::from_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_unknown_input_is_malformed() {
        assert!(Reader::new(&[0, 0, 0, 5, 1], "test").variable().is_err());
        assert!(Reader::new(&[0, 0, 0], "test").u32().is_err());
    }
}
