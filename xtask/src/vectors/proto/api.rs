// SPDX-License-Identifier: AGPL-3.0-only
//! HTTP API bodies: mailbox registration, the knock plaintext, the push relay's request and
//! configuration, and Privacy Pass issuance.

use anyhow::Result;

use super::super::cbor::{NULL, array, bytes, text, uint, versioned};
use super::super::json::object;
use super::super::{VectorFile, case, error, fill, hex, prefix, ramp};
use super::{
    listed_sealed_push, proto_file, push_blob, push_handle, record_randomness, record_tag,
};

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![
        mailbox_spec()?,
        knock()?,
        push_request()?,
        push_relay_config()?,
        scheduled_push_relay_config()?,
        mailbox_push()?,
        privacy_pass_issue_request()?,
        privacy_pass_issue_response()?,
    ])
}

fn mailbox_spec() -> Result<VectorFile> {
    let mailbox = ramp(0x11, 16);
    let (blob, handle) = (push_blob(), push_handle());
    let custom_hash = fill(0x77, 32);
    let custom = array([
        array([uint(1), text("https://push.example"), bytes(&custom_hash)]),
        uint(7),
        bytes(&blob),
        uint(0),
        bytes(&handle),
    ]);
    let registration =
        |ttl: u64, push: Vec<u8>| versioned(array([bytes(&mailbox), uint(ttl), push]));
    proto_file(
        "mailbox-spec-encoding",
        "POST /v1/mailboxes body as canonical CBOR (design B.4.9, D)",
        vec![
            case(
                "listed-relay-quiet",
                object! {
                    "mailbox_id": hex(&mailbox),
                    "ttl_seconds": 3600,
                    "relay_id": 2,
                    "relay_url": "",
                    "key_config_hash": "",
                    "key_id": 1,
                    "blob": hex(&blob),
                    "urgency": "quiet",
                    "handle": hex(&handle),
                },
                object! { "encoding": hex(&registration(3600, listed_sealed_push())) },
            ),
            case(
                "custom-relay-alert",
                object! {
                    "mailbox_id": hex(&mailbox),
                    "ttl_seconds": 2_592_000,
                    "relay_id": "",
                    "relay_url": "https://push.example",
                    "key_config_hash": hex(&custom_hash),
                    "key_id": 7,
                    "blob": hex(&blob),
                    "urgency": "alert",
                    "handle": hex(&handle),
                },
                object! { "encoding": hex(&registration(2_592_000, custom)) },
            ),
            case(
                "no-push",
                object! {
                    "mailbox_id": hex(&mailbox),
                    "ttl_seconds": 60,
                    "relay_id": "",
                    "relay_url": "",
                    "key_config_hash": "",
                    "key_id": "",
                    "blob": "",
                    "urgency": "",
                    "handle": "",
                },
                object! { "encoding": hex(&registration(60, NULL.to_vec())) },
            ),
            case(
                "zero-ttl",
                object! { "encoding": hex(&registration(0, NULL.to_vec())) },
                error("OutOfRange"),
            ),
            case(
                "relay-key-id-over-255",
                object! {
                    "encoding": hex(&registration(60, array([
                        array([uint(0), uint(2)]),
                        uint(256),
                        bytes(&blob),
                        uint(1),
                        bytes(&handle),
                    ]))),
                },
                error("Cbor"),
            ),
            case(
                "sealed-push-without-key-id",
                object! {
                    "encoding": hex(&registration(60, array([
                        array([uint(0), uint(2)]),
                        bytes(&blob),
                        uint(1),
                        bytes(&handle),
                    ]))),
                },
                error("Cbor"),
            ),
        ],
    )
}

fn knock() -> Result<VectorFile> {
    let (matched, own) = (record_randomness(), record_tag());
    let key_package = ramp(0x33, 70);
    let signature = fill(0x99, 64);
    let reply = ramp(0x55, 16);
    let subscription_id = ramp(0x66, 16);
    let subscription = array([bytes(&subscription_id), listed_sealed_push(), uint(900)]);
    proto_file(
        "knock-encoding",
        "Knock plaintext as canonical CBOR, before padding and sealing (design B.4.2)",
        vec![
            case(
                "with-subscription",
                object! {
                    "matched_randomness": hex(&matched),
                    "own_randomness": hex(&own),
                    "leaf_index": 5,
                    "party_epoch": 12,
                    "key_package": hex(&key_package),
                    "reply_mailbox": hex(&reply),
                    "subscription_id": hex(&subscription_id),
                    "subscription_ttl_seconds": 900,
                    "signature": hex(&signature),
                },
                object! {
                    "encoding": hex(&versioned(array([
                        bytes(&matched),
                        bytes(&own),
                        uint(5),
                        uint(12),
                        bytes(&key_package),
                        bytes(&reply),
                        subscription,
                        bytes(&signature),
                    ]))),
                },
            ),
            case(
                "first-arrival",
                object! {
                    "matched_randomness": hex(&matched),
                    "own_randomness": "",
                    "leaf_index": 0,
                    "party_epoch": 1,
                    "key_package": hex(&key_package),
                    "reply_mailbox": hex(&reply),
                    "subscription_id": "",
                    "subscription_ttl_seconds": 0,
                    "signature": hex(&signature),
                },
                object! {
                    "encoding": hex(&versioned(array([
                        bytes(&matched),
                        NULL.to_vec(),
                        uint(0),
                        uint(1),
                        bytes(&key_package),
                        bytes(&reply),
                        NULL.to_vec(),
                        bytes(&signature),
                    ]))),
                },
            ),
        ],
    )
}

fn push_request() -> Result<VectorFile> {
    let (blob, handle) = (push_blob(), push_handle());
    let bind = ramp(0xb0, 32);
    let request = |key_id: u64, bind: &[u8], urgency: u64| {
        versioned(array([
            uint(key_id),
            bytes(&blob),
            bytes(bind),
            uint(urgency),
            bytes(&handle),
        ]))
    };
    proto_file(
        "push-request-encoding",
        "POST /v1/push body (push relay API) as canonical CBOR: [key id, blob, bind, urgency, handle] (design D, PushReq)",
        vec![
            case(
                "alert",
                object! {
                    "key_id": 1,
                    "blob": hex(&blob),
                    "bind": hex(&bind),
                    "urgency": "alert",
                    "handle": hex(&handle),
                },
                object! { "encoding": hex(&request(1, &bind, 0)) },
            ),
            case(
                "quiet-key-200",
                object! {
                    "key_id": 200,
                    "blob": hex(&blob),
                    "bind": hex(&bind),
                    "urgency": "quiet",
                    "handle": hex(&handle),
                },
                object! { "encoding": hex(&request(200, &bind, 1)) },
            ),
            case(
                "short-bind",
                object! { "encoding": hex(&request(1, &prefix(&bind, 31)?, 0)) },
                error("WrongLength"),
            ),
            case(
                "key-id-over-255",
                object! { "encoding": hex(&request(256, &bind, 0)) },
                error("Cbor"),
            ),
        ],
    )
}

/// An RFC 9458 key config: key id, KEM X25519 (0x0020), the public key, and the suites
/// (4 bytes of HKDF-SHA256 with AES-128-GCM unless `aead` says otherwise).
fn key_config(key_id: u8, public_key: &[u8], aead: u8) -> Vec<u8> {
    [
        &[key_id, 0x00, 0x20][..],
        public_key,
        &[0x00, 0x04, 0x00, 0x01, 0x00, aead],
    ]
    .concat()
}

/// AES-128-GCM, the only AEAD the push relay accepts.
const AES_128_GCM: u8 = 0x01;
/// ChaCha20-Poly1305, refused.
const CHACHA20_POLY1305: u8 = 0x03;

fn push_relay_config() -> Result<VectorFile> {
    let public_key = ramp(0x60, 32);
    let standard = |key_id: u8| key_config(key_id, &public_key, AES_128_GCM);
    let refused = |config: &[u8], platform: u64, ttl: u64| {
        versioned(array([
            bytes(config),
            array([uint(platform)]),
            uint(ttl),
            text("x"),
        ]))
    };
    proto_file(
        "push-relay-config-encoding",
        "GET /v1/config body (push relay API) as canonical CBOR: [key config (RFC 9458, 41 bytes), [platform...], max ttl seconds, operator] (design D, RelayConfig)",
        vec![
            case(
                "both-platforms",
                object! {
                    "key_id": 2,
                    "public_key": hex(&public_key),
                    "platforms": "fcm,unifiedpush",
                    "max_ttl_seconds": 2_592_000,
                    "operator": "Example Relay Ltd",
                },
                object! {
                    "key_config": hex(&standard(2)),
                    "encoding": hex(&versioned(array([
                        bytes(&standard(2)),
                        array([uint(1), uint(2)]),
                        uint(2_592_000),
                        text("Example Relay Ltd"),
                    ]))),
                },
            ),
            case(
                "unifiedpush-only",
                object! {
                    "key_id": 1,
                    "public_key": hex(&public_key),
                    "platforms": "unifiedpush",
                    "max_ttl_seconds": 3600,
                    "operator": "Ünïcode Relay",
                },
                object! {
                    "key_config": hex(&standard(1)),
                    "encoding": hex(&versioned(array([
                        bytes(&standard(1)),
                        array([uint(2)]),
                        uint(3600),
                        text("Ünïcode Relay"),
                    ]))),
                },
            ),
            case(
                "chacha-suite",
                object! {
                    "encoding": hex(&refused(&key_config(1, &public_key, CHACHA20_POLY1305), 2, 3600)),
                },
                error("OutOfRange"),
            ),
            case(
                "unknown-platform",
                object! { "encoding": hex(&refused(&standard(1), 3, 3600)) },
                error("OutOfRange"),
            ),
            case(
                "zero-ttl",
                object! { "encoding": hex(&refused(&standard(1), 2, 0)) },
                error("OutOfRange"),
            ),
            case(
                "short-key-config",
                object! { "encoding": hex(&refused(&prefix(&standard(1), 40)?, 2, 3600)) },
                error("WrongLength"),
            ),
        ],
    )
}

fn scheduled_push_relay_config() -> Result<VectorFile> {
    let public_key = ramp(0x60, 32);
    let standard = |key_id: u8| key_config(key_id, &public_key, AES_128_GCM);
    let config = |key_id: u8, platforms: Vec<u8>, ttl: u64, operator: &str| {
        array([
            bytes(&standard(key_id)),
            platforms,
            uint(ttl),
            text(operator),
        ])
    };
    let placeholder = || config(1, array([uint(2)]), 3600, "Ünïcode Relay");
    proto_file(
        "scheduled-push-relay-config-encoding",
        "GET /v1/config/next body (push relay API) as canonical CBOR: [config (as in push-relay-config-encoding), activates at (Unix seconds)] (design B.4.9, C.1a, D)",
        vec![
            case(
                "next-key-both-platforms",
                object! {
                    "key_id": 2,
                    "public_key": hex(&public_key),
                    "platforms": "fcm,unifiedpush",
                    "max_ttl_seconds": 2_592_000,
                    "operator": "Example Relay Ltd",
                    "activates_at": 1_790_000_000,
                },
                object! {
                    "key_config": hex(&standard(2)),
                    "encoding": hex(&versioned(array([
                        config(2, array([uint(1), uint(2)]), 2_592_000, "Example Relay Ltd"),
                        uint(1_790_000_000),
                    ]))),
                },
            ),
            case(
                "far-future-placeholder",
                object! {
                    "key_id": 1,
                    "public_key": hex(&public_key),
                    "platforms": "unifiedpush",
                    "max_ttl_seconds": 3600,
                    "operator": "Ünïcode Relay",
                    "activates_at": 4_102_444_800_i64,
                },
                object! {
                    "key_config": hex(&standard(1)),
                    "encoding": hex(&versioned(array([placeholder(), uint(4_102_444_800)]))),
                },
            ),
            case(
                "missing-activation",
                object! { "encoding": hex(&versioned(array([placeholder()]))) },
                error("Cbor"),
            ),
        ],
    )
}

fn mailbox_push() -> Result<VectorFile> {
    let (blob, handle) = (push_blob(), push_handle());
    proto_file(
        "mailbox-push-encoding",
        "PUT /v1/mailboxes/{id}/push body as canonical CBOR: [sealed push or null], the mailbox's new sealed push (design B.4.9, D)",
        vec![
            case(
                "listed-relay-quiet",
                object! {
                    "relay_id": 2,
                    "relay_url": "",
                    "key_config_hash": "",
                    "key_id": 1,
                    "blob": hex(&blob),
                    "urgency": "quiet",
                    "handle": hex(&handle),
                },
                object! { "encoding": hex(&versioned(array([listed_sealed_push()]))) },
            ),
            case(
                "no-push",
                object! {
                    "relay_id": "",
                    "relay_url": "",
                    "key_config_hash": "",
                    "key_id": "",
                    "blob": "",
                    "urgency": "",
                    "handle": "",
                },
                object! { "encoding": hex(&versioned(array([NULL.to_vec()]))) },
            ),
            case(
                "two-elements",
                object! { "encoding": hex(&versioned(array([NULL.to_vec(), NULL.to_vec()]))) },
                error("Cbor"),
            ),
        ],
    )
}

/// The size of one blinded or evaluated element.
const ELEMENT_LEN: usize = 49;
/// The largest token request: token type, truncated key id, 2-byte length, 300 elements.
const MAX_REQUEST_LEN: usize = 14_705;
/// The largest token response: 2-byte length, 300 elements, the 96-byte proof.
const MAX_RESPONSE_LEN: usize = 14_798;

/// An RFC 9578 batched token request: type 0x0001, truncated key id, `count` elements.
fn token_request(truncated_key_id: u8, count: usize) -> Result<Vec<u8>> {
    let elements: Vec<u8> = (0..count)
        .flat_map(|index| ramp(index.saturating_add(0x02), ELEMENT_LEN))
        .collect();
    let elements_len = u16::try_from(elements.len())?;
    Ok([
        &[0x00, 0x01, truncated_key_id][..],
        &elements_len.to_be_bytes(),
        &elements,
    ]
    .concat())
}

/// An RFC 9578 batched token response: `count` elements and the proof.
fn token_response(count: usize) -> Result<Vec<u8>> {
    let elements: Vec<u8> = (0..count)
        .flat_map(|index| ramp(index.saturating_add(0x30), ELEMENT_LEN))
        .collect();
    let elements_len = u16::try_from(elements.len())?;
    Ok([&elements_len.to_be_bytes()[..], &elements, &ramp(0x90, 96)].concat())
}

/// `[1, [bytes]]`, the shape of both issuance bodies.
fn issue_body(token: &[u8]) -> Vec<u8> {
    versioned(array([bytes(token)]))
}

fn privacy_pass_issue_request() -> Result<VectorFile> {
    let two = token_request(0x7f, 2)?;
    let full = fill(0x01, MAX_REQUEST_LEN);
    let over = fill(0x01, MAX_REQUEST_LEN.saturating_add(1));
    proto_file(
        "privacy-pass-issue-request-encoding",
        "POST /v1/pp/issue body as canonical CBOR: [token request], the token request being pingers-anon's RFC 9578 batched request (token_type || truncated_key_id || blinded_elements<2-byte length>), opaque here and at most 5 + 300 * 49 bytes (design B.4.8, D)",
        vec![
            case(
                "two-tokens",
                object! { "token_request": hex(&two) },
                object! { "encoding": hex(&issue_body(&two)) },
            ),
            case(
                "full-batch",
                object! { "token_request": hex(&full) },
                object! { "encoding": hex(&issue_body(&full)) },
            ),
            case(
                "over-a-batch",
                object! { "encoding": hex(&issue_body(&over)) },
                error("OutOfRange"),
            ),
            case(
                "empty",
                object! { "encoding": hex(&issue_body(&[])) },
                error("OutOfRange"),
            ),
        ],
    )
}

fn privacy_pass_issue_response() -> Result<VectorFile> {
    let two = token_response(2)?;
    let full = fill(0x02, MAX_RESPONSE_LEN);
    let over = fill(0x02, MAX_RESPONSE_LEN.saturating_add(1));
    proto_file(
        "privacy-pass-issue-response-encoding",
        "POST /v1/pp/issue answer as canonical CBOR: [token response], the token response being pingers-anon's RFC 9578 batched response (evaluated_elements<2-byte length> || proof(96)), opaque here and at most 2 + 300 * 49 + 96 bytes (design B.4.8, D)",
        vec![
            case(
                "two-tokens",
                object! { "token_response": hex(&two) },
                object! { "encoding": hex(&issue_body(&two)) },
            ),
            case(
                "full-batch",
                object! { "token_response": hex(&full) },
                object! { "encoding": hex(&issue_body(&full)) },
            ),
            case(
                "over-a-batch",
                object! { "encoding": hex(&issue_body(&over)) },
                error("OutOfRange"),
            ),
            case(
                "text-instead-of-bytes",
                object! { "encoding": hex(&versioned(array([text("token")]))) },
                error("Cbor"),
            ),
        ],
    )
}
