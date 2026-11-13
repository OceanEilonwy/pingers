// SPDX-License-Identifier: AGPL-3.0-only
//! The loader for the JSON known-answer tests in `vectors/` (format in `vectors/README.md`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Why a vector file or field could not be used.
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
    /// The file's `name` is not its file name without `.json` (`vectors/README.md`).
    #[error("vector file {path} is named `{name}`; the name must match the file name")]
    NameMismatch {
        /// The file.
        path: PathBuf,
        /// The name inside it.
        name: String,
    },
    /// Two vectors in one file have the same name.
    #[error("vector file {path} has two vectors named `{vector}`")]
    DuplicateVector {
        /// The file.
        path: PathBuf,
        /// The repeated name.
        vector: String,
    },
    /// No vector has the name asked for.
    #[error("vector file `{file}` has no vector `{vector}`")]
    MissingVector {
        /// The file's name.
        file: String,
        /// The vector asked for.
        vector: String,
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

/// One vector file: `vectors/<name>.json`.
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

impl VectorFile {
    /// The vector called `name`.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingVector`].
    pub fn vector(&self, name: &str) -> Result<&Vector, VectorError> {
        self.vectors
            .iter()
            .find(|vector| vector.name == name)
            .ok_or_else(|| VectorError::MissingVector {
                file: self.name.clone(),
                vector: name.to_owned(),
            })
    }
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

/// Which half of a vector a field is read from.
#[derive(Clone, Copy)]
enum Side {
    Inputs,
    Expected,
}

impl Vector {
    /// An input field holding bytes as lower-case hex.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn input_bytes(&self, field: &str) -> Result<Vec<u8>, VectorError> {
        self.bytes(Side::Inputs, field)
    }

    /// An expected field holding bytes as lower-case hex.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn expected_bytes(&self, field: &str) -> Result<Vec<u8>, VectorError> {
        self.bytes(Side::Expected, field)
    }

    /// An input field holding exactly `N` bytes as lower-case hex, for ids and keys.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`], or [`VectorError::WrongType`] if it is not hex or not
    /// `N` bytes long.
    pub fn input_array<const N: usize>(&self, field: &str) -> Result<[u8; N], VectorError> {
        self.array(Side::Inputs, field)
    }

    /// An expected field holding exactly `N` bytes as lower-case hex.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`], or [`VectorError::WrongType`] if it is not hex or not
    /// `N` bytes long.
    pub fn expected_array<const N: usize>(&self, field: &str) -> Result<[u8; N], VectorError> {
        self.array(Side::Expected, field)
    }

    /// An input field holding an unsigned integer.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn input_u64(&self, field: &str) -> Result<u64, VectorError> {
        self.unsigned(Side::Inputs, field)
    }

    /// An expected field holding an unsigned integer.
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn expected_u64(&self, field: &str) -> Result<u64, VectorError> {
        self.unsigned(Side::Expected, field)
    }

    /// An input field holding text (labels, enum names).
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn input_str(&self, field: &str) -> Result<&str, VectorError> {
        self.text(Side::Inputs, field)
    }

    /// An expected field holding text (for example an error name).
    ///
    /// # Errors
    ///
    /// [`VectorError::MissingField`] or [`VectorError::WrongType`].
    pub fn expected_str(&self, field: &str) -> Result<&str, VectorError> {
        self.text(Side::Expected, field)
    }

    fn bytes(&self, side: Side, field: &str) -> Result<Vec<u8>, VectorError> {
        let text = self
            .field(side, field)?
            .as_str()
            .ok_or_else(|| self.wrong_type(field, "a hex string"))?;
        if text.chars().any(|character| character.is_ascii_uppercase()) {
            return Err(self.wrong_type(field, "lower-case hex"));
        }
        hex::decode(text).map_err(|_invalid| self.wrong_type(field, "a hex string"))
    }

    fn array<const N: usize>(&self, side: Side, field: &str) -> Result<[u8; N], VectorError> {
        self.bytes(side, field)?
            .try_into()
            .map_err(|_wrong_length: Vec<u8>| self.wrong_type(field, "the expected byte length"))
    }

    fn unsigned(&self, side: Side, field: &str) -> Result<u64, VectorError> {
        self.field(side, field)?
            .as_u64()
            .ok_or_else(|| self.wrong_type(field, "an unsigned integer"))
    }

    fn text(&self, side: Side, field: &str) -> Result<&str, VectorError> {
        self.field(side, field)?
            .as_str()
            .ok_or_else(|| self.wrong_type(field, "a string"))
    }

    fn field(&self, side: Side, field: &str) -> Result<&serde_json::Value, VectorError> {
        let fields = match side {
            Side::Inputs => &self.inputs,
            Side::Expected => &self.expected,
        };
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

/// The workspace's `vectors/` directory.
fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vectors")
}

/// Loads `vectors/<name>.json` from the workspace root, wherever the calling test runs.
///
/// # Errors
///
/// As [`load_vectors_from`].
pub fn load_vectors(name: &str) -> Result<VectorFile, VectorError> {
    load_vectors_from(&vectors_dir().join(format!("{name}.json")))
}

/// Loads a vector file from `path` and checks that its `name` matches the file name and that
/// its vector names are unique.
///
/// # Errors
///
/// [`VectorError::Read`], [`VectorError::Parse`], [`VectorError::NameMismatch`] or
/// [`VectorError::DuplicateVector`].
pub fn load_vectors_from(path: &Path) -> Result<VectorFile, VectorError> {
    let text = fs::read_to_string(path).map_err(|source| VectorError::Read {
        path: path.to_owned(),
        source,
    })?;
    parse_vector_file(path, &text)
}

/// Parses and checks the contents of the vector file at `path`.
fn parse_vector_file(path: &Path, text: &str) -> Result<VectorFile, VectorError> {
    let file: VectorFile = serde_json::from_str(text).map_err(|source| VectorError::Parse {
        path: path.to_owned(),
        source,
    })?;
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(file.name.as_str()) {
        return Err(VectorError::NameMismatch {
            path: path.to_owned(),
            name: file.name,
        });
    }
    let mut seen = BTreeSet::new();
    if let Some(duplicate) = file
        .vectors
        .iter()
        .find(|vector| !seen.insert(vector.name.as_str()))
    {
        return Err(VectorError::DuplicateVector {
            path: path.to_owned(),
            vector: duplicate.name.clone(),
        });
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// A test-local error so fixture setup and vector access both work with `?`.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Vector(#[from] VectorError),
        #[error(transparent)]
        Json(#[from] serde_json::Error),
    }

    fn vector(inputs: &str, expected: &str) -> Result<Vector, TestError> {
        Ok(Vector {
            name: String::from("case"),
            inputs: serde_json::from_str(inputs)?,
            expected: serde_json::from_str(expected)?,
        })
    }

    #[test]
    fn example_vector_file_loads() -> Result<(), TestError> {
        let file = load_vectors("example")?;
        let vector = file.vector("sha256-abc")?;
        assert_eq!(vector.input_bytes("message")?, b"abc");
        assert_eq!(vector.expected_array::<32>("sha256")?.len(), 32);
        Ok(())
    }

    #[test]
    fn missing_file_is_a_read_error() {
        assert!(matches!(
            load_vectors("no-such-vector-file"),
            Err(VectorError::Read { .. })
        ));
    }

    #[test]
    fn malformed_file_is_a_parse_error() {
        assert!(matches!(
            parse_vector_file(Path::new("broken.json"), r#"{"name": "broken"}"#),
            Err(VectorError::Parse { .. })
        ));
    }

    #[test]
    fn name_must_match_file_name() {
        let text = r#"{"name": "other", "description": "d", "vectors": []}"#;
        assert!(matches!(
            parse_vector_file(Path::new("vectors/actual.json"), text),
            Err(VectorError::NameMismatch { name, .. }) if name == "other"
        ));
        assert!(parse_vector_file(Path::new("vectors/other.json"), text).is_ok());
    }

    #[test]
    fn duplicate_vector_names_are_rejected() {
        let case = r#"{"name": "twice", "inputs": {}, "expected": {}}"#;
        let other = r#"{"name": "once", "inputs": {}, "expected": {}}"#;
        let text = format!(
            r#"{{"name": "dup", "description": "d", "vectors": [{case}, {other}, {case}]}}"#
        );
        assert!(matches!(
            parse_vector_file(Path::new("dup.json"), &text),
            Err(VectorError::DuplicateVector { vector, .. }) if vector == "twice"
        ));
    }

    #[test]
    fn unknown_vector_name_is_reported() -> Result<(), TestError> {
        let file = load_vectors("example")?;
        assert!(matches!(
            file.vector("absent"),
            Err(VectorError::MissingVector { vector, .. }) if vector == "absent"
        ));
        Ok(())
    }

    #[test]
    fn upper_case_hex_is_rejected() -> Result<(), TestError> {
        let vector = vector(r#"{"bytes": "AB"}"#, "{}")?;
        assert!(matches!(
            vector.input_bytes("bytes"),
            Err(VectorError::WrongType {
                expected: "lower-case hex",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn odd_length_hex_is_rejected() -> Result<(), TestError> {
        let vector = vector(r#"{"bytes": "abc"}"#, "{}")?;
        assert!(matches!(
            vector.input_bytes("bytes"),
            Err(VectorError::WrongType {
                expected: "a hex string",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn array_of_the_wrong_length_is_rejected() -> Result<(), TestError> {
        let vector = vector(r#"{"id": "0102"}"#, r#"{"id": "010203"}"#)?;
        assert_eq!(vector.input_array::<2>("id")?, [1, 2]);
        assert!(matches!(
            vector.input_array::<3>("id"),
            Err(VectorError::WrongType { .. })
        ));
        assert!(matches!(
            vector.expected_array::<2>("id"),
            Err(VectorError::WrongType { .. })
        ));
        Ok(())
    }

    #[test]
    fn inputs_and_expected_are_read_from_their_own_side() -> Result<(), TestError> {
        let vector = vector(
            r#"{"n": 1, "label": "in", "b": "01"}"#,
            r#"{"n": 2, "label": "out", "b": "02"}"#,
        )?;
        assert_eq!(vector.input_u64("n")?, 1);
        assert_eq!(vector.expected_u64("n")?, 2);
        assert_eq!(vector.input_str("label")?, "in");
        assert_eq!(vector.expected_str("label")?, "out");
        assert_eq!(vector.input_bytes("b")?, [1]);
        assert_eq!(vector.expected_bytes("b")?, [2]);
        Ok(())
    }

    #[test]
    fn missing_and_mistyped_fields_name_the_field() -> Result<(), TestError> {
        let vector = vector(r#"{"n": "seven", "label": 7, "b": 1}"#, "{}")?;
        assert!(matches!(
            vector.input_u64("absent"),
            Err(VectorError::MissingField { field, .. }) if field == "absent"
        ));
        assert!(matches!(
            vector.input_u64("n"),
            Err(VectorError::WrongType { field, expected: "an unsigned integer", .. }) if field == "n"
        ));
        assert!(matches!(
            vector.input_str("label"),
            Err(VectorError::WrongType {
                expected: "a string",
                ..
            })
        ));
        assert!(matches!(
            vector.input_bytes("b"),
            Err(VectorError::WrongType {
                expected: "a hex string",
                ..
            })
        ));
        Ok(())
    }
}
