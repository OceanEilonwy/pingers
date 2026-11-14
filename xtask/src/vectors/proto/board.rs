// SPDX-License-Identifier: AGPL-3.0-only
//! Fixed-layout board formats (records, leases, beacons, RLN auth, the sealed push plaintext)
//! and the board page.

use anyhow::Result;

use super::super::cbor::{NULL, array, bytes, uint, versioned};
use super::super::json::object;
use super::super::{VectorFile, case, drop_last, error, fill, hex, prefix, ramp, suffix};
use super::{proto_file, record_randomness, record_tag};

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![
        record()?,
        lease()?,
        board_beacon()?,
        rln_auth()?,
        push_token_plaintext()?,
        board_page()?,
    ])
}

fn record() -> Result<VectorFile> {
    let (randomness, tag, sealed) = (record_randomness(), record_tag(), ramp(0x20, 64));
    proto_file(
        "record-encoding",
        "Board record: r || tag || sealed, 96 bytes (design B.4.2)",
        vec![case(
            "ramp",
            object! { "randomness": hex(&randomness), "tag": hex(&tag), "sealed": hex(&sealed) },
            object! { "record": hex(&[randomness, tag, sealed].concat()) },
        )],
    )
}

fn lease() -> Result<VectorFile> {
    let records: Vec<u8> = (0..4_usize)
        .flat_map(|index| ramp(index.saturating_mul(96), 96))
        .collect();
    let commitment = fill(0xcc, 32);
    let lease = [&1_957_000_u64.to_be_bytes()[..], &records, &commitment].concat();
    let bad_bucket = [&u64::MAX.to_be_bytes()[..], &records, &commitment].concat();
    proto_file(
        "lease-encoding",
        "Lease: be64(b0) || 4 records || revoke commitment, 424 bytes (design B.4.2)",
        vec![
            case(
                "four-records",
                object! {
                    "start_bucket": 1_957_000,
                    "records": hex(&records),
                    "revoke_commitment": hex(&commitment),
                },
                object! { "lease": hex(&lease) },
            ),
            case(
                "one-byte-short",
                object! { "lease": hex(&drop_last(&lease)?) },
                error("WrongLength"),
            ),
            case(
                "bucket-beyond-range",
                object! { "lease": hex(&bad_bucket) },
                error("Bucket"),
            ),
        ],
    )
}

fn board_beacon() -> Result<VectorFile> {
    let (mailbox, hint) = (ramp(0x40, 16), ramp(0x80, 16));
    proto_file(
        "board-beacon-encoding",
        "Board beacon plaintext: knock mailbox || 16 zero bytes || meetup hint, 48 bytes (design B.4.2)",
        vec![
            case(
                "with-meetup-hint",
                object! { "knock_mailbox": hex(&mailbox), "meetup_hint": hex(&hint) },
                object! { "beacon": hex(&[mailbox.clone(), vec![0; 16], hint].concat()) },
            ),
            case(
                "no-meetup",
                object! { "knock_mailbox": hex(&mailbox), "meetup_hint": "" },
                object! { "beacon": hex(&[mailbox.clone(), vec![0; 32]].concat()) },
            ),
            case(
                "reserved-byte-set",
                object! { "beacon": hex(&[mailbox, vec![1], vec![0; 31]].concat()) },
                error("ReservedNotZero"),
            ),
        ],
    )
}

fn rln_auth() -> Result<VectorFile> {
    let (root, x, y, nullifier) = (fill(1, 32), fill(3, 32), fill(4, 32), fill(5, 32));
    let proof = ramp(0, 128);
    let encoding = [
        &root[..],
        &[1],
        &1_957_001_u64.to_be_bytes(),
        &x,
        &y,
        &nullifier,
        &proof,
    ]
    .concat();
    proto_file(
        "rln-auth-encoding",
        "RlnAuth: root || scope || be64(bucket) || x || y || nullifier || proof, 265 bytes (design D)",
        vec![
            case(
                "mailbox-scope",
                object! {
                    "root": hex(&root),
                    "scope": 1,
                    "bucket": 1_957_001,
                    "x": hex(&x),
                    "y": hex(&y),
                    "nullifier": hex(&nullifier),
                    "proof": hex(&proof),
                },
                object! { "encoding": hex(&encoding) },
            ),
            case(
                "unknown-scope",
                object! { "encoding": hex(&[root, vec![3], suffix(&encoding, 33)?].concat()) },
                error("OutOfRange"),
            ),
            case(
                "one-byte-long",
                object! { "encoding": hex(&[encoding, vec![0]].concat()) },
                error("WrongLength"),
            ),
        ],
    )
}

/// The sealed push plaintext: version 1, platform, class, be64(not after), be16(token length),
/// token, zero-padded to 272 bytes.
fn push_plaintext(platform: u8, class: u8, token: &[u8], not_after: u64) -> Result<Vec<u8>> {
    let token_len = u16::try_from(token.len())?;
    let mut body = [
        &[1, platform, class][..],
        &not_after.to_be_bytes(),
        &token_len.to_be_bytes(),
        token,
    ]
    .concat();
    body.resize(272, 0);
    Ok(body)
}

fn push_token_plaintext() -> Result<VectorFile> {
    let fcm_token = [&b"fcm-token:"[..], &ramp(0x30, 20)].concat();
    let unified_push_token = b"https://up.example/push/abc";
    let minimal = push_plaintext(1, 0, b"t", 0)?;
    proto_file(
        "push-token-plaintext",
        "Sealed push plaintext: version || platform || class || be64(not_after) || be16(len) || token, zero-padded to 272 bytes (design B.4.9)",
        vec![
            case(
                "fcm-standard",
                object! {
                    "platform": "fcm",
                    "class": "standard",
                    "token": hex(&fcm_token),
                    "not_after": 1_790_000_000,
                },
                object! { "padded": hex(&push_plaintext(1, 0, &fcm_token, 1_790_000_000)?) },
            ),
            case(
                "unified-push-location",
                object! {
                    "platform": "unified_push",
                    "class": "location",
                    "token": hex(unified_push_token),
                    "not_after": 1_790_000_900,
                },
                object! {
                    "padded": hex(&push_plaintext(2, 1, unified_push_token, 1_790_000_900)?),
                },
            ),
            case(
                "nonzero-padding",
                object! { "padded": hex(&[drop_last(&minimal)?, vec![1]].concat()) },
                error("BadPadding"),
            ),
            case(
                "unknown-platform",
                object! { "padded": hex(&[vec![1, 9], suffix(&minimal, 2)?].concat()) },
                error("OutOfRange"),
            ),
            case(
                "unknown-version",
                object! { "padded": hex(&[vec![2], suffix(&minimal, 1)?].concat()) },
                error("UnsupportedVersion"),
            ),
        ],
    )
}

fn board_page() -> Result<VectorFile> {
    let records: Vec<u8> = (0..3).flat_map(|start| ramp(start, 96)).collect();
    let hash = fill(0xab, 32);
    let cursor = ramp(0xc0, 24);
    let partial = |record_count: u64, len: usize| -> Result<Vec<u8>> {
        Ok(versioned(array([
            array([array([uint(1), uint(record_count), NULL.to_vec()])]),
            bytes(&prefix(&records, len)?),
            NULL.to_vec(),
        ])))
    };
    proto_file(
        "board-page-encoding",
        "Board page as canonical CBOR: segments, raw concatenated records, cursor (design D)",
        vec![
            case(
                "closed-and-open-segments",
                object! {
                    "first_start_bucket": 1_956_999,
                    "first_record_count": 1,
                    "first_hash": hex(&hash),
                    "second_start_bucket": 1_957_000,
                    "second_record_count": 2,
                    "records": hex(&records),
                    "next": hex(&cursor),
                },
                object! {
                    "encoding": hex(&versioned(array([
                        array([
                            array([uint(1_956_999), uint(1), bytes(&hash)]),
                            array([uint(1_957_000), uint(2), NULL.to_vec()]),
                        ]),
                        bytes(&records),
                        bytes(&cursor),
                    ]))),
                },
            ),
            case(
                "counts-disagree",
                object! { "encoding": hex(&partial(2, 96)?) },
                error("OutOfRange"),
            ),
            case(
                "partial-record",
                object! { "encoding": hex(&partial(1, 95)?) },
                error("WrongLength"),
            ),
        ],
    )
}
