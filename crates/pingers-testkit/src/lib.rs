// SPDX-License-Identifier: AGPL-3.0-only
//! Fake platform, harness, vectors.
//!
//! Shared test fixtures for every crate's tests (architecture §3, style guide §7): a fake
//! `Platform` (scripted fixes, DND, wakes), an in-process server with relays, a deterministic
//! clock and RNG, and the loader for the JSON test vectors in `vectors/`.
//!
//! Available now: [`seeded_rng`], [`ManualClock`] (re-exported from pingers-time) and the
//! vector loader ([`load_vectors`]). The fake platform and the in-process server come with the
//! crates they fake.
//!
//! Crates use this as a dev-dependency only; `cargo xtask layers` forbids production
//! dependencies on it.
#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

pub use pingers_time::ManualClock;
use rand_core::SeedableRng as _;
use serde::Deserialize;

/// A deterministic, cryptographically strong RNG for tests: the same seed gives the same dummy
/// slots, nonces and padding on every run (style guide §6).
#[must_use]
pub fn seeded_rng(seed: u64) -> rand_chacha::ChaCha20Rng {
    rand_chacha::ChaCha20Rng::seed_from_u64(seed)
}

/// Why a vector file could not be used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VectorError {
    /// The file could not be read.
    #[error("cannot read vector file {path}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid vector JSON.
    #[error("vector file {path} is malformed")]
    Parse {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
    /// A field is missing from a vector.
    #[error("vector `{vector}` has no field `{field}`")]
    MissingField {
        /// The vector's name.
        vector: String,
        /// The field asked for.
        field: String,
    },
    /// A field is not the type asked for (for example a bytes field that is not lower-case hex).
    #[error("vector `{vector}` field `{field}` is not {expected}")]
    WrongType {
        /// The vector's name.
        vector: String,
        /// The field.
        field: String,
        /// What was expected.
        expected: &'static str,
    },
}

/// One vector file: `vectors/<name>.json` (format in `vectors/README.md`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VectorFile {
    /// What the vectors cover, for example `presence-key-schedule`.
    pub name: String,
    /// Where in the design the derivation or format is defined.
    pub description: String,
    /// The vectors.
    pub vectors: Vec<Vector>,
}

/// One test case.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Vector {
    /// A unique, descriptive name within the file.
    pub name: String,
    /// The inputs, by field name.
    pub inputs: serde_json::Map<String, serde_json::Value>,
    /// The expected outputs, by field name.
    pub expected: serde_json::Map<String, serde_json::Value>,
}

impl Vector {
    /// An input field holding bytes as lower-case hex.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn input_bytes(&self, field: &str) -> Result<Vec<u8>, VectorError> {
        self.bytes(&self.inputs, field)
    }

    /// An expected field holding bytes as lower-case hex.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn expected_bytes(&self, field: &str) -> Result<Vec<u8>, VectorError> {
        self.bytes(&self.expected, field)
    }

    /// An input field holding an unsigned integer.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn input_u64(&self, field: &str) -> Result<u64, VectorError> {
        self.field(&self.inputs, field)?
            .as_u64()
            .ok_or_else(|| self.wrong_type(field, "an unsigned integer"))
    }

    fn bytes(
        &self,
        fields: &serde_json::Map<String, serde_json::Value>,
        field: &str,
    ) -> Result<Vec<u8>, VectorError> {
        let text = self
            .field(fields, field)?
            .as_str()
            .ok_or_else(|| self.wrong_type(field, "a hex string"))?;
        if text.chars().any(|character| character.is_ascii_uppercase()) {
            return Err(self.wrong_type(field, "lower-case hex"));
        }
        hex::decode(text).map_err(|_invalid| self.wrong_type(field, "a hex string"))
    }

    fn field<'vector>(
        &self,
        fields: &'vector serde_json::Map<String, serde_json::Value>,
        field: &str,
    ) -> Result<&'vector serde_json::Value, VectorError> {
        fields.get(field).ok_or_else(|| VectorError::MissingField {
            vector: self.name.clone(),
            field: field.to_owned(),
        })
    }

    fn wrong_type(&self, field: &str, expected: &'static str) -> VectorError {
        VectorError::WrongType {
            vector: self.name.clone(),
            field: field.to_owned(),
            expected,
        }
    }
}

/// Loads `vectors/<name>.json` from the workspace root.
///
/// # Errors
///
/// [`VectorError::Read`] or [`VectorError::Parse`].
pub fn load_vectors(name: &str) -> Result<VectorFile, VectorError> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vectors")
        .join(format!("{name}.json"));
    let text = fs::read_to_string(&path).map_err(|source| VectorError::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| VectorError::Parse { path, source })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use rand_core::Rng as _;

    use super::*;

    #[test]
    fn same_seed_gives_same_bytes() {
        let mut first = seeded_rng(7);
        let mut second = seeded_rng(7);
        assert_eq!(first.next_u64(), second.next_u64());
    }

    #[test]
    fn example_vector_file_loads() -> Result<(), VectorError> {
        let file = load_vectors("example")?;
        let vector = file
            .vectors
            .first()
            .ok_or_else(|| VectorError::MissingField {
                vector: String::from("example"),
                field: String::from("vectors[0]"),
            })?;
        assert_eq!(vector.input_bytes("message")?, b"abc");
        assert_eq!(vector.expected_bytes("sha256")?.len(), 32);
        Ok(())
    }

    #[test]
    fn upper_case_hex_is_rejected() {
        let vector = Vector {
            name: String::from("case"),
            inputs: serde_json::from_str(r#"{"bytes": "AB"}"#).unwrap_or_default(),
            expected: serde_json::Map::new(),
        };
        assert!(matches!(
            vector.input_bytes("bytes"),
            Err(VectorError::WrongType { .. })
        ));
    }
}
