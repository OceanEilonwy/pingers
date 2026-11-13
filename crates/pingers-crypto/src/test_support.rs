// SPDX-License-Identifier: AGPL-3.0-only
//! Test helpers: an RNG that replays vector bytes, and vector field accessors.

use core::convert::Infallible;

use pingers_testkit::{Vector, VectorError};

/// Why a test failed before reaching an assertion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    /// A vector file or field was unusable.
    #[error(transparent)]
    Vector(#[from] VectorError),
    /// The code under test failed.
    #[error(transparent)]
    Crypto(#[from] crate::CryptoError),
    /// A domain value from a vector was out of range.
    #[error(transparent)]
    Time(#[from] pingers_time::TimeError),
    /// A domain value from a vector was out of range.
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    /// A vector field had the wrong length or form.
    #[error("vector field `{0}` is malformed")]
    Field(&'static str),
}

/// An RNG that returns exactly the bytes it was given, so a vector can list the randomness a
/// computation consumes (`vectors/README.md`, "Randomness"). Reading past the end returns
/// zeros and is recorded, so tests can assert every byte was used and no more.
pub(crate) struct FixedRng {
    bytes: Vec<u8>,
    position: usize,
    overrun: bool,
}

impl FixedRng {
    pub(crate) fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            position: 0,
            overrun: false,
        }
    }

    /// Whether every byte was consumed and nothing more was asked for.
    pub(crate) fn exhausted_exactly(&self) -> bool {
        !self.overrun && self.position == self.bytes.len()
    }
}

impl rand_core::TryRng for FixedRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
        for byte in destination {
            if let Some(source) = self.bytes.get(self.position) {
                *byte = *source;
            } else {
                *byte = 0;
                self.overrun = true;
            }
            self.position = self.position.saturating_add(1);
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for FixedRng {}

/// A fixed-size input field.
pub(crate) fn input_array<const N: usize>(
    vector: &Vector,
    field: &'static str,
) -> Result<[u8; N], TestError> {
    vector
        .input_bytes(field)?
        .try_into()
        .map_err(|_length| TestError::Field(field))
}

/// A text input field.
pub(crate) fn input_text<'vector>(
    vector: &'vector Vector,
    field: &'static str,
) -> Result<&'vector str, TestError> {
    vector
        .inputs
        .get(field)
        .and_then(|value| value.as_str())
        .ok_or(TestError::Field(field))
}

/// The error kind a failure vector expects, if it is one.
pub(crate) fn expected_error(vector: &Vector) -> Option<&str> {
    vector
        .expected
        .get("error")
        .and_then(|value| value.as_str())
}

/// The variant name of an error, as failure vectors spell it.
pub(crate) fn error_kind<T>(result: &Result<T, crate::CryptoError>) -> Option<&'static str> {
    result.as_ref().err().map(|error| match error {
        crate::CryptoError::OpenFailed => "OpenFailed",
        crate::CryptoError::BadSignature => "BadSignature",
        crate::CryptoError::InvalidKey => "InvalidKey",
        crate::CryptoError::WrongLength { .. } => "WrongLength",
        crate::CryptoError::Encoding(_) => "Encoding",
        crate::CryptoError::Internal => "Internal",
    })
}
