// SPDX-License-Identifier: AGPL-3.0-only
//! A small fixed-layout byte codec for this crate's local records and Party operations.
//!
//! Interim: the Party state operations and the state snapshot are wire values and belong in
//! pingers-proto (style guide §12); the crate's CONTRACT.md asks for them there. Until they
//! exist, this module is the single place their bytes are written and read. Integers are
//! big-endian; variable-length fields carry a `be32` length.

use pingers_crypto::AccountPublicKey;
use pingers_geo::{Circle, Coordinate, Fence, Meters, Polygon};

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

    pub(crate) fn fence(&mut self, fence: &Fence) -> Result<(), PartiesError> {
        match fence {
            Fence::Circle(circle) => {
                self.byte(0);
                self.coordinate(circle.center());
                self.u64(circle.radius().get().to_bits());
            }
            Fence::Polygon(polygon) => {
                self.byte(1);
                self.count(polygon.vertices().len())?;
                for vertex in polygon.vertices() {
                    self.coordinate(*vertex);
                }
            }
        }
        Ok(())
    }

    fn coordinate(&mut self, coordinate: Coordinate) {
        self.u64(coordinate.latitude().to_bits());
        self.u64(coordinate.longitude().to_bits());
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

    /// A fence, re-validated through pingers-geo's constructors (a polygon from another member
    /// is checked exactly as one drawn on this device).
    pub(crate) fn fence(&mut self) -> Result<Fence, PartiesError> {
        match self.byte()? {
            0 => {
                let center = self.coordinate()?;
                let radius = Meters::new(f64::from_bits(self.u64()?))?;
                Ok(Fence::Circle(Circle::new(center, radius)?))
            }
            1 => {
                let count = self.count()?;
                // Bounded by the bytes present, so a hostile count cannot allocate much.
                let mut vertices = Vec::with_capacity(count.min(self.rest.len()));
                for _ in 0..count {
                    vertices.push(self.coordinate()?);
                }
                Ok(Fence::Polygon(Polygon::new(vertices)?))
            }
            _ => Err(self.malformed()),
        }
    }

    fn coordinate(&mut self) -> Result<Coordinate, PartiesError> {
        let latitude = f64::from_bits(self.u64()?);
        let longitude = f64::from_bits(self.u64()?);
        Ok(Coordinate::new(latitude, longitude)?)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn fences_round_trip_and_reject_trailing_bytes() -> Result<(), TestError> {
        let polygon = Fence::Polygon(Polygon::new(vec![
            Coordinate::new(51.5, -0.1)?,
            Coordinate::new(51.501, -0.1)?,
            Coordinate::new(51.501, -0.098)?,
        ])?);
        let circle = Fence::Circle(Circle::new(
            Coordinate::new(1.0, 2.0)?,
            Meters::new(150.0)?,
        )?);
        for fence in [polygon, circle] {
            let mut writer = Writer::new();
            writer.fence(&fence)?;
            let bytes = writer.finish();
            let mut reader = Reader::new(&bytes, "test");
            assert_eq!(reader.fence()?, fence);
            reader.end()?;
            let mut longer = bytes.clone();
            longer.push(0);
            let mut reader = Reader::new(&longer, "test");
            reader.fence()?;
            assert!(reader.end().is_err());
        }
        Ok(())
    }

    #[test]
    fn short_and_unknown_input_is_malformed() {
        assert!(Reader::new(&[2], "test").fence().is_err());
        assert!(Reader::new(&[0, 0, 0, 5, 1], "test").variable().is_err());
        assert!(Reader::new(&[0, 0, 0], "test").u32().is_err());
    }
}
