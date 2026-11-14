// SPDX-License-Identifier: AGPL-3.0-only
//! Group and meetup frames: meetup messages, the sealed location's plaintext and the group
//! message envelope.

use anyhow::Result;

use super::super::cbor::{NULL, array, bytes, int, text, uint, versioned};
use super::super::json::object;
use super::super::{VectorFile, case, error, fill, hex, prefix, ramp};
use super::proto_file;

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![
        meetup_message()?,
        location_plaintext()?,
        app_message()?,
    ])
}

#[expect(
    clippy::too_many_lines,
    reason = "one vector per frame kind, each a few lines; splitting would scatter the file"
)]
fn meetup_message() -> Result<VectorFile> {
    let (chat_id, parent) = (ramp(0xa0, 16), ramp(0xb0, 16));
    let segment_hash = fill(0x5e, 32);
    let keep_alive = versioned(array([uint(7)]));
    let chat = |id: &[u8], body: &str| {
        versioned(array([
            uint(3),
            bytes(id),
            NULL.to_vec(),
            text(body),
            uint(0),
        ]))
    };
    let vectors = vec![
        case(
            "keep-alive",
            object! { "kind": "keep_alive" },
            object! { "encoding": hex(&keep_alive) },
        ),
        case(
            "chat-reply",
            object! {
                "kind": "chat",
                "id": hex(&chat_id),
                "parent": hex(&parent),
                "body": "On my way ☕",
                "written_at": 1_790_000_000,
            },
            object! {
                "encoding": hex(&versioned(array([
                    uint(3),
                    bytes(&chat_id),
                    bytes(&parent),
                    text("On my way ☕"),
                    uint(1_790_000_000),
                ]))),
            },
        ),
        case(
            "chat-root",
            object! {
                "kind": "chat",
                "id": hex(&chat_id),
                "parent": "",
                "body": "hi",
                "written_at": 0,
            },
            object! { "encoding": hex(&chat(&chat_id, "hi")) },
        ),
        case(
            "sealed-location",
            object! {
                "kind": "sealed_location",
                "generation": 3,
                "nonce": hex(&ramp(0, 12)),
                "ciphertext": hex(&ramp(0x60, 40)),
            },
            object! {
                "encoding": hex(&versioned(array([
                    uint(0),
                    uint(3),
                    bytes(&ramp(0, 12)),
                    bytes(&ramp(0x60, 40)),
                ]))),
            },
        ),
        case(
            "meet-point-fixed-southern-hemisphere",
            object! {
                "kind": "meet_point_fixed",
                "latitude": -338_688_000,
                "longitude": 1_512_093_000,
                "label": "Opera House steps",
                "lamport": 42,
            },
            object! {
                "encoding": hex(&versioned(array([
                    uint(4),
                    array([
                        uint(0),
                        int(-338_688_000),
                        int(1_512_093_000),
                        text("Opera House steps"),
                        uint(42),
                    ]),
                ]))),
            },
        ),
        case(
            "meet-point-at-member",
            object! { "kind": "meet_point_at_member", "leaf": 2, "lamport": 300 },
            object! {
                "encoding": hex(&versioned(array([
                    uint(4),
                    array([uint(1), uint(2), uint(300)]),
                ]))),
            },
        ),
        case(
            "board-seen",
            object! { "kind": "board_seen", "start_bucket": 1_957_000, "hash": hex(&segment_hash) },
            object! {
                "encoding": hex(&versioned(array([
                    uint(6),
                    array([array([uint(1_957_000), bytes(&segment_hash)])]),
                ]))),
            },
        ),
        case(
            "long-form-kind",
            object! { "encoding": hex(&[0x82, 0x01, 0x81, 0x18, 0x07]) },
            error("NonCanonical"),
        ),
        case(
            "indefinite-array",
            object! { "encoding": hex(&[0x82, 0x01, 0x9f, 0x07, 0xff]) },
            error("NonCanonical"),
        ),
        case(
            "trailing-byte",
            object! { "encoding": hex(&[keep_alive, vec![0]].concat()) },
            error("TrailingBytes"),
        ),
        case(
            "version-two",
            object! { "encoding": hex(&array([uint(2), array([uint(7)])])) },
            error("UnsupportedVersion"),
        ),
        case(
            "unknown-kind",
            object! { "encoding": hex(&versioned(array([uint(8)]))) },
            error("OutOfRange"),
        ),
        case(
            "empty-chat-body",
            object! { "encoding": hex(&chat(&chat_id, "")) },
            error("OutOfRange"),
        ),
        case(
            "short-chat-id",
            object! { "encoding": hex(&chat(&prefix(&chat_id, 15)?, "hi")) },
            error("WrongLength"),
        ),
    ];
    proto_file(
        "meetup-message-encoding",
        "Meetup frames as canonical CBOR [1, [kind, fields...]] (design B.6, B.6.1)",
        vectors,
    )
}

fn location_plaintext() -> Result<VectorFile> {
    proto_file(
        "location-plaintext-encoding",
        "Inner plaintext of SealedLocation as canonical CBOR (design B.6)",
        vec![
            case(
                "with-heading-no-speed",
                object! {
                    "latitude": 515_007_000,
                    "longitude": -1_246_000,
                    "accuracy": 8,
                    "heading": 270,
                    "speed": "",
                    "taken_at": 1_790_000_000,
                },
                object! {
                    "encoding": hex(&versioned(array([
                        int(515_007_000),
                        int(-1_246_000),
                        uint(8),
                        uint(270),
                        NULL.to_vec(),
                        uint(1_790_000_000),
                    ]))),
                },
            ),
            case(
                "heading-360",
                object! {
                    "encoding": hex(&versioned(array([
                        uint(0),
                        uint(0),
                        uint(8),
                        uint(360),
                        NULL.to_vec(),
                        uint(0),
                    ]))),
                },
                error("OutOfRange"),
            ),
        ],
    )
}

fn app_message() -> Result<VectorFile> {
    let body = ramp(0x01, 50);
    proto_file(
        "app-message-encoding",
        "Group message envelope (POST /v1/groups/{id}/msgs) as canonical CBOR (design D)",
        vec![
            case(
                "stored-alert",
                object! { "epoch": 7, "body": hex(&body), "delivery": "stored", "wake": "alert" },
                object! {
                    "encoding": hex(&versioned(array([uint(7), bytes(&body), uint(0), uint(0)]))),
                },
            ),
            case(
                "ephemeral-no-wake",
                object! {
                    "epoch": 70_000,
                    "body": hex(&body),
                    "delivery": "ephemeral",
                    "wake": "",
                },
                object! {
                    "encoding": hex(&versioned(array([
                        uint(70_000),
                        bytes(&body),
                        uint(1),
                        NULL.to_vec(),
                    ]))),
                },
            ),
            case(
                "unknown-delivery",
                object! {
                    "encoding": hex(&versioned(array([
                        uint(7),
                        bytes(&body),
                        uint(2),
                        NULL.to_vec(),
                    ]))),
                },
                error("OutOfRange"),
            ),
        ],
    )
}
