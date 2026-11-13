// SPDX-License-Identifier: AGPL-3.0-only
//! Looking at what the server kept: raw database bytes, mailbox rows, delivery tables, and
//! byte patterns (coordinates, addresses, tokens) to search for.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::E2eError;

/// Whether `needle` occurs in `haystack` (an empty needle never does).
#[must_use]
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Every byte of an SQLite database as it sits on disk: the file and its WAL and shared-memory
/// files, if present. Deleted rows the database has not overwritten would show up here.
///
/// # Errors
///
/// [`E2eError::Io`] if the main file cannot be read.
pub fn database_bytes(path: &Path) -> Result<Vec<u8>, E2eError> {
    let mut bytes = std::fs::read(path)?;
    for suffix in ["-wal", "-shm"] {
        let mut companion = path.as_os_str().to_owned();
        companion.push(suffix);
        if let Ok(more) = std::fs::read(Path::new(&companion)) {
            bytes.extend_from_slice(&more);
        }
    }
    Ok(bytes)
}

fn open(path: &Path) -> Result<Connection, E2eError> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// The names of the tables in a database.
///
/// # Errors
///
/// [`E2eError::Sqlite`] if the database cannot be read.
pub fn tables(path: &Path) -> Result<Vec<String>, E2eError> {
    let connection = open(path)?;
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// The column names of `table`.
///
/// # Errors
///
/// [`E2eError::Sqlite`] if the database cannot be read.
pub fn columns(path: &Path, table: &str) -> Result<Vec<String>, E2eError> {
    let connection = open(path)?;
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// How many rows `table` holds.
///
/// # Errors
///
/// [`E2eError::Sqlite`] if the database cannot be read.
pub fn row_count(path: &Path, table: &str) -> Result<u64, E2eError> {
    let connection = open(path)?;
    let count: i64 =
        connection.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
            row.get(0)
        })?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Every value in every row of every table, as bytes (text as UTF-8, integers as decimal
/// text): what a query over the live database can see, without deleted pages.
///
/// # Errors
///
/// [`E2eError::Sqlite`] if the database cannot be read.
pub fn all_values(path: &Path) -> Result<Vec<Vec<u8>>, E2eError> {
    let connection = open(path)?;
    let mut values = Vec::new();
    for table in tables(path)? {
        let mut statement = connection.prepare(&format!("SELECT * FROM \"{table}\""))?;
        let width = statement.column_count();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for index in 0..width {
                let value = match row.get_ref(index)? {
                    rusqlite::types::ValueRef::Null => continue,
                    rusqlite::types::ValueRef::Integer(number) => number.to_string().into_bytes(),
                    rusqlite::types::ValueRef::Real(number) => number.to_string().into_bytes(),
                    rusqlite::types::ValueRef::Text(text)
                    | rusqlite::types::ValueRef::Blob(text) => text.to_vec(),
                };
                values.push(value);
            }
        }
    }
    Ok(values)
}

/// The byte patterns a coordinate could leave behind if it were ever sent: IEEE 754 double and
/// single precision in both byte orders, fixed-point degrees (×10⁷, ×10⁶, ×10⁵) as 32-bit
/// integers in both orders, and decimal text with four and five decimals.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "deliberately reproducing lossy encodings a careless client might use"
)]
pub fn coordinate_patterns(latitude: f64, longitude: f64) -> Vec<Vec<u8>> {
    let mut patterns = Vec::new();
    for value in [latitude, longitude] {
        patterns.push(value.to_be_bytes().to_vec());
        patterns.push(value.to_le_bytes().to_vec());
        let single = value as f32;
        patterns.push(single.to_be_bytes().to_vec());
        patterns.push(single.to_le_bytes().to_vec());
        for scale in [1e7, 1e6, 1e5] {
            let fixed = (value * scale).round() as i32;
            patterns.push(fixed.to_be_bytes().to_vec());
            patterns.push(fixed.to_le_bytes().to_vec());
        }
        patterns.push(format!("{value:.4}").into_bytes());
        patterns.push(format!("{value:.5}").into_bytes());
    }
    patterns
}

/// The byte patterns a loopback address could leave behind: dotted text and the four raw
/// bytes, for the device address and the relay's.
#[must_use]
pub fn address_patterns() -> Vec<Vec<u8>> {
    let mut patterns = Vec::new();
    for address in [crate::DEVICE_ADDRESS, crate::RELAY_EGRESS] {
        patterns.push(address.to_string().into_bytes());
        if let std::net::IpAddr::V4(v4) = address {
            patterns.push(v4.octets().to_vec());
        }
    }
    patterns
}
