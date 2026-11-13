// SPDX-License-Identifier: AGPL-3.0-only
//! Schema-level privacy checks: no location history (design C.2, I.2 #11).

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::StoreError;
use crate::test_support::{Fixture, TestError};

/// Columns that hold a position, and why each is not history.
const POSITION_COLUMNS: [&str; 8] = [
    // A Spot's shape: where it is, not where anyone was.
    "spot.geometry",
    // An exclusion zone's shape (local only).
    "exclusion_zone.geometry",
    // A granted peer's latest decrypted position: one row per peer, overwritten by each
    // frame, cleared with the peer's key, deleted when the Meetup ends.
    "meetup_peer.location_lat_e7",
    "meetup_peer.location_lon_e7",
    "meetup_peer.location_accuracy_m",
    "meetup_peer.location_heading_deg",
    "meetup_peer.location_speed_cms",
    "meetup_peer.location_taken_at",
];

/// Columns whose names look positional but hold no position.
const NOT_POSITIONS: [&str; 2] = [
    // When the last fix was processed; the fix itself is held only in memory.
    "presence.last_fix_at",
    // A setting.
    "party.location_visibility",
];

/// Name fragments that suggest a column stores where someone is or was.
const POSITIONAL_FRAGMENTS: [&str; 6] = ["lat", "lon", "geometry", "location", "coord", "fix"];

struct Column {
    table: String,
    name: String,
    primary_key_part: bool,
}

fn columns(connection: &Connection) -> Result<Vec<Column>, StoreError> {
    let mut tables = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut all = Vec::new();
    for table in names {
        let mut info = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = info.query_map([], |row| {
            Ok(Column {
                table: table.clone(),
                name: row.get(1)?,
                primary_key_part: row.get::<_, i64>(5)? > 0,
            })
        })?;
        for column in rows {
            all.push(column?);
        }
    }
    Ok(all)
}

#[test]
fn no_table_stores_positions_over_time() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let columns = fixture.session.with(columns)?;
    assert!(columns.len() > 100, "the schema walk found too few columns");

    // Every positional-looking column is accounted for, so a new one (a fix log, a trail
    // table) fails here until someone justifies it.
    let positional: BTreeSet<String> = columns
        .iter()
        .filter(|column| {
            POSITIONAL_FRAGMENTS
                .iter()
                .any(|fragment| column.name.contains(fragment))
        })
        .map(|column| format!("{}.{}", column.table, column.name))
        .collect();
    let accounted: BTreeSet<String> = POSITION_COLUMNS
        .iter()
        .chain(NOT_POSITIONS.iter())
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(positional, accounted);

    // A table that holds a position has one row per thing (a Spot, a zone, a peer), never
    // per moment: no time column is part of its key.
    let position_tables: BTreeSet<&str> = POSITION_COLUMNS
        .iter()
        .filter_map(|name| name.split('.').next())
        .collect();
    for table in position_tables {
        let key: Vec<&str> = columns
            .iter()
            .filter(|column| column.table == table && column.primary_key_part)
            .map(|column| column.name.as_str())
            .collect();
        let expected: &[&str] = match table {
            "spot" => &["spot_id"],
            "exclusion_zone" => &["zone_id"],
            "meetup_peer" => &["group_id", "leaf"],
            // Fails the comparison below: a new position table needs an entry here.
            _ => &["(no expected key)"],
        };
        assert_eq!(key, expected, "primary key of {table}");
    }
    Ok(())
}
