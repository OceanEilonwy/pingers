// SPDX-License-Identifier: AGPL-3.0-only
//! Party state (design B.2): the operations members send as Party group application messages,
//! and the state snapshot a committer seals for new members.

use anyhow::Result;

use super::super::cbor::{array, array_of, bytes, double, optional, text, uint, versioned};
use super::super::json::object;
use super::super::{VectorFile, case, error, hex, ramp};
use super::proto_file;

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![party_operation()?, party_snapshot()?])
}

fn spot_id() -> Vec<u8> {
    ramp(0, 16)
}

fn member_key() -> Vec<u8> {
    ramp(0x20, 32)
}

fn subscription() -> Vec<u8> {
    ramp(0x61, 16)
}

fn party_id() -> Vec<u8> {
    ramp(0x40, 16)
}

/// `[name, subscription id or null]`.
fn profile(name: &str, subscription: Option<&[u8]>) -> Vec<u8> {
    array([text(name), optional(subscription, bytes)])
}

/// `[0, latitude, longitude, radius]`, each an eight-byte float.
fn circle(latitude: f64, longitude: f64, radius: f64) -> Vec<u8> {
    array([uint(0), double(latitude), double(longitude), double(radius)])
}

/// `[1, [[latitude, longitude], ...]]`.
fn polygon(points: &[(f64, f64)]) -> Vec<u8> {
    array([
        uint(1),
        array_of(
            points
                .iter()
                .map(|(latitude, longitude)| array([double(*latitude), double(*longitude)]))
                .collect(),
        ),
    ])
}

/// `[spot id, name, fence, created by, lamport, deleted]`.
fn spot(name: &str, fence: Vec<u8>, lamport: u64, deleted: u64) -> Vec<u8> {
    array([
        bytes(&spot_id()),
        text(name),
        fence,
        bytes(&member_key()),
        uint(lamport),
        uint(deleted),
    ])
}

/// The Spot "Pub": a 150 m circle at 51.5, -0.1, Lamport 3, active.
fn pub_spot() -> Vec<u8> {
    spot("Pub", circle(51.5, -0.1, 150.0), 3, 0)
}

fn party_operation() -> Result<VectorFile> {
    let operation = |kind: u64, body: Vec<u8>| versioned(array([uint(kind), body]));
    let pub_with = |fence: Vec<u8>, deleted: u64| operation(1, spot("Pub", fence, 3, deleted));
    proto_file(
        "party-operation-encoding",
        "Party state operation (design B.2, Party group application message before padding) as \
         canonical CBOR: [1, [0, [name, subscription or null]]] for a profile, [1, [1, [spot id, \
         name, fence, created by, lamport, deleted]]] for a Spot; fences [0, lat, lon, radius] \
         or [1, [[lat, lon], ...]] with eight-byte floats",
        vec![
            case(
                "profile-with-subscription",
                object! {
                    "kind": "profile",
                    "name": "Bob",
                    "subscription": hex(&subscription()),
                },
                object! { "encoding": hex(&operation(0, profile("Bob", Some(&subscription())))) },
            ),
            case(
                "profile-without-subscription",
                object! { "kind": "profile", "name": "Bob", "subscription": "" },
                object! { "encoding": hex(&operation(0, profile("Bob", None))) },
            ),
            case(
                "circle-spot",
                object! {
                    "kind": "circle",
                    "spot": hex(&spot_id()),
                    "name": "Pub",
                    "latitude": 51.5,
                    "longitude": -0.1,
                    "radius": 150.0,
                    "created_by": hex(&member_key()),
                    "lamport": 3_i64,
                    "deleted": 0_i64,
                },
                object! { "encoding": hex(&operation(1, pub_spot())) },
            ),
            case(
                "deleted-polygon-spot",
                object! {
                    "kind": "polygon",
                    "spot": hex(&spot_id()),
                    "name": "Park",
                    "created_by": hex(&member_key()),
                    "lamport": 300_i64,
                    "deleted": 1_i64,
                },
                object! {
                    "encoding": hex(&operation(
                        1,
                        spot("Park", polygon(&[(1.0, 2.0), (1.5, 2.0), (1.5, 2.5)]), 300, 1),
                    )),
                },
            ),
            case(
                "nan-latitude",
                object! { "encoding": hex(&pub_with(circle(f64::NAN, -0.1, 150.0), 0)) },
                error("OutOfRange"),
            ),
            case(
                "half-float-radius",
                object! {
                    "encoding": hex(&pub_with(
                        [
                            vec![0x84, 0x00],
                            double(51.5),
                            double(-0.1),
                            vec![0xf9, 0x58, 0xb0],
                        ]
                        .concat(),
                        0,
                    )),
                },
                error("Cbor"),
            ),
            case(
                "unknown-operation",
                object! { "encoding": hex(&operation(2, profile("Bob", None))) },
                error("OutOfRange"),
            ),
            case(
                "unknown-spot-life",
                object! { "encoding": hex(&pub_with(circle(51.5, -0.1, 150.0), 2)) },
                error("OutOfRange"),
            ),
        ],
    )
}

fn party_snapshot() -> Result<VectorFile> {
    let snapshot = |name: &str, members: Vec<Vec<u8>>, spots: Vec<Vec<u8>>| {
        versioned(array([
            bytes(&party_id()),
            text(name),
            array_of(members),
            array_of(spots),
        ]))
    };
    let alice = array([
        bytes(&member_key()),
        profile("Alice", Some(&subscription())),
    ]);
    proto_file(
        "party-snapshot-encoding",
        "Party state snapshot (design B.2, sealed under the next epoch's state key) as canonical \
         CBOR: [1, [party id, name, [[member key, [name, subscription or null]], ...], [spot, \
         ...]]], spots as in party-operation-encoding",
        vec![
            case(
                "one-member-one-spot",
                object! {
                    "party": hex(&party_id()),
                    "name": "Friends",
                    "member_key": hex(&member_key()),
                    "member_name": "Alice",
                    "member_subscription": hex(&subscription()),
                },
                object! { "encoding": hex(&snapshot("Friends", vec![alice], vec![pub_spot()])) },
            ),
            case(
                "empty-party-name",
                object! { "encoding": hex(&snapshot("", Vec::new(), Vec::new())) },
                error("OutOfRange"),
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_are_always_nine_bytes() {
        assert_eq!(double(150.0), [0xfb, 0x40, 0x62, 0xc0, 0, 0, 0, 0, 0]);
    }
}
