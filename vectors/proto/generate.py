#!/usr/bin/env python3
"""Generates vectors/proto/*.json with a CBOR encoder written independently of pingers-proto.

Usage: python3 vectors/proto/generate.py vectors/proto

In inputs, an optional field that is absent is written as "" (empty string).
"""
import json
import struct
import sys
from pathlib import Path

OUT = Path(sys.argv[1])


def head(major, value):
    m = major << 5
    if value < 24:
        return bytes([m | value])
    if value < 0x100:
        return bytes([m | 24, value])
    if value < 0x10000:
        return bytes([m | 25]) + struct.pack(">H", value)
    if value < 0x100000000:
        return bytes([m | 26]) + struct.pack(">I", value)
    return bytes([m | 27]) + struct.pack(">Q", value)


def u(value):
    return head(0, value)


def i(value):
    return head(0, value) if value >= 0 else head(1, -1 - value)


def b(data):
    return head(2, len(data)) + data


def t(text):
    data = text.encode()
    return head(3, len(data)) + data


def a(*items):
    return head(4, len(items)) + b"".join(items)


NULL = b"\xf6"


def opt(value, enc):
    return NULL if value is None else enc(value)


def versioned(body):
    return a(u(1), body)


def h(data):
    return data.hex()


def fill(byte, n):
    return bytes([byte]) * n


def ramp(start, n):
    return bytes((start + k) % 256 for k in range(n))


def write(name, description, vectors):
    doc = {"name": name, "description": description, "vectors": vectors}
    path = OUT / f"{name}.json"
    path.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n")


# --- record ---
r, tag, sealed = ramp(0, 16), ramp(0x10, 16), ramp(0x20, 64)
write(
    "record-encoding",
    "Board record: r || tag || sealed, 96 bytes (design B.4.2)",
    [
        {
            "name": "ramp",
            "inputs": {"randomness": h(r), "tag": h(tag), "sealed": h(sealed)},
            "expected": {"record": h(r + tag + sealed)},
        }
    ],
)

# --- lease ---
records = [ramp(k * 96, 96) for k in range(4)]
commit = fill(0xcc, 32)
lease = struct.pack(">Q", 1_957_000) + b"".join(records) + commit
bad_bucket = struct.pack(">Q", 2**64 - 1) + b"".join(records) + commit
write(
    "lease-encoding",
    "Lease: be64(b0) || 4 records || revoke commitment, 424 bytes (design B.4.2)",
    [
        {
            "name": "four-records",
            "inputs": {
                "start_bucket": 1_957_000,
                "records": h(b"".join(records)),
                "revoke_commitment": h(commit),
            },
            "expected": {"lease": h(lease)},
        },
        {
            "name": "one-byte-short",
            "inputs": {"lease": h(lease[:-1])},
            "expected": {"error": "WrongLength"},
        },
        {
            "name": "bucket-beyond-range",
            "inputs": {"lease": h(bad_bucket)},
            "expected": {"error": "Bucket"},
        },
    ],
)

# --- board beacon ---
mailbox, hint = ramp(0x40, 16), ramp(0x80, 16)
write(
    "board-beacon-encoding",
    "Board beacon plaintext: knock mailbox || 16 zero bytes || meetup hint, 48 bytes (design B.4.2)",
    [
        {
            "name": "with-meetup-hint",
            "inputs": {"knock_mailbox": h(mailbox), "meetup_hint": h(hint)},
            "expected": {"beacon": h(mailbox + bytes(16) + hint)},
        },
        {
            "name": "no-meetup",
            "inputs": {"knock_mailbox": h(mailbox), "meetup_hint": ""},
            "expected": {"beacon": h(mailbox + bytes(32))},
        },
        {
            "name": "reserved-byte-set",
            "inputs": {"beacon": h(mailbox + b"\x01" + bytes(31))},
            "expected": {"error": "ReservedNotZero"},
        },
    ],
)

# --- rln auth ---
root, x, y, nul, proof = fill(1, 32), fill(3, 32), fill(4, 32), fill(5, 32), ramp(0, 128)
rln = root + bytes([1]) + struct.pack(">Q", 1_957_001) + x + y + nul + proof
write(
    "rln-auth-encoding",
    "RlnAuth: root || scope || be64(bucket) || x || y || nullifier || proof, 265 bytes (design D)",
    [
        {
            "name": "mailbox-scope",
            "inputs": {
                "root": h(root),
                "scope": 1,
                "bucket": 1_957_001,
                "x": h(x),
                "y": h(y),
                "nullifier": h(nul),
                "proof": h(proof),
            },
            "expected": {"encoding": h(rln)},
        },
        {
            "name": "unknown-scope",
            "inputs": {"encoding": h(root + bytes([3]) + rln[33:])},
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "one-byte-long",
            "inputs": {"encoding": h(rln + b"\x00")},
            "expected": {"error": "WrongLength"},
        },
    ],
)

# --- push token plaintext ---
def push_plain(platform, klass, token, not_after):
    body = bytes([1, platform, klass]) + struct.pack(">QH", not_after, len(token)) + token
    return body + bytes(272 - len(body))


fcm_token = b"fcm-token:" + ramp(0x30, 20)
up_token = b"https://up.example/push/abc"
write(
    "push-token-plaintext",
    "Sealed push plaintext: version || platform || class || be64(not_after) || be16(len) || token, zero-padded to 272 bytes (design B.4.9)",
    [
        {
            "name": "fcm-standard",
            "inputs": {
                "platform": "fcm",
                "class": "standard",
                "token": h(fcm_token),
                "not_after": 1_790_000_000,
            },
            "expected": {"padded": h(push_plain(1, 0, fcm_token, 1_790_000_000))},
        },
        {
            "name": "unified-push-location",
            "inputs": {
                "platform": "unified_push",
                "class": "location",
                "token": h(up_token),
                "not_after": 1_790_000_900,
            },
            "expected": {"padded": h(push_plain(2, 1, up_token, 1_790_000_900))},
        },
        {
            "name": "nonzero-padding",
            "inputs": {"padded": h(push_plain(1, 0, b"t", 0)[:-1] + b"\x01")},
            "expected": {"error": "BadPadding"},
        },
        {
            "name": "unknown-platform",
            "inputs": {"padded": h(bytes([1, 9]) + push_plain(1, 0, b"t", 0)[2:])},
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "unknown-version",
            "inputs": {"padded": h(bytes([2]) + push_plain(1, 0, b"t", 0)[1:])},
            "expected": {"error": "UnsupportedVersion"},
        },
    ],
)

# --- meetup message ---
chat_id, parent = ramp(0xa0, 16), ramp(0xb0, 16)
seg_hash = fill(0x5e, 32)
meetup = [
    {
        "name": "keep-alive",
        "inputs": {"kind": "keep_alive"},
        "expected": {"encoding": h(versioned(a(u(7))))},
    },
    {
        "name": "chat-reply",
        "inputs": {
            "kind": "chat",
            "id": h(chat_id),
            "parent": h(parent),
            "body": "On my way ☕",
            "written_at": 1_790_000_000,
        },
        "expected": {
            "encoding": h(
                versioned(a(u(3), b(chat_id), b(parent), t("On my way ☕"), u(1_790_000_000)))
            )
        },
    },
    {
        "name": "chat-root",
        "inputs": {
            "kind": "chat",
            "id": h(chat_id),
            "parent": "",
            "body": "hi",
            "written_at": 0,
        },
        "expected": {"encoding": h(versioned(a(u(3), b(chat_id), NULL, t("hi"), u(0))))},
    },
    {
        "name": "sealed-location",
        "inputs": {
            "kind": "sealed_location",
            "generation": 3,
            "nonce": h(ramp(0, 12)),
            "ciphertext": h(ramp(0x60, 40)),
        },
        "expected": {
            "encoding": h(versioned(a(u(0), u(3), b(ramp(0, 12)), b(ramp(0x60, 40)))))
        },
    },
    {
        "name": "meet-point-fixed-southern-hemisphere",
        "inputs": {
            "kind": "meet_point_fixed",
            "latitude": -338_688_000,
            "longitude": 1_512_093_000,
            "label": "Opera House steps",
            "lamport": 42,
        },
        "expected": {
            "encoding": h(
                versioned(
                    a(
                        u(4),
                        a(u(0), i(-338_688_000), i(1_512_093_000), t("Opera House steps"), u(42)),
                    )
                )
            )
        },
    },
    {
        "name": "meet-point-at-member",
        "inputs": {"kind": "meet_point_at_member", "leaf": 2, "lamport": 300},
        "expected": {"encoding": h(versioned(a(u(4), a(u(1), u(2), u(300)))))},
    },
    {
        "name": "board-seen",
        "inputs": {"kind": "board_seen", "start_bucket": 1_957_000, "hash": h(seg_hash)},
        "expected": {"encoding": h(versioned(a(u(6), a(a(u(1_957_000), b(seg_hash))))))},
    },
    {
        "name": "long-form-kind",
        "inputs": {"encoding": h(bytes([0x82, 0x01, 0x81, 0x18, 0x07]))},
        "expected": {"error": "NonCanonical"},
    },
    {
        "name": "indefinite-array",
        "inputs": {"encoding": h(bytes([0x82, 0x01, 0x9f, 0x07, 0xff]))},
        "expected": {"error": "NonCanonical"},
    },
    {
        "name": "trailing-byte",
        "inputs": {"encoding": h(versioned(a(u(7))) + b"\x00")},
        "expected": {"error": "TrailingBytes"},
    },
    {
        "name": "version-two",
        "inputs": {"encoding": h(a(u(2), a(u(7))))},
        "expected": {"error": "UnsupportedVersion"},
    },
    {
        "name": "unknown-kind",
        "inputs": {"encoding": h(versioned(a(u(8))))},
        "expected": {"error": "OutOfRange"},
    },
    {
        "name": "empty-chat-body",
        "inputs": {"encoding": h(versioned(a(u(3), b(chat_id), NULL, t(""), u(0))))},
        "expected": {"error": "OutOfRange"},
    },
    {
        "name": "short-chat-id",
        "inputs": {"encoding": h(versioned(a(u(3), b(chat_id[:15]), NULL, t("hi"), u(0))))},
        "expected": {"error": "WrongLength"},
    },
]
write(
    "meetup-message-encoding",
    "Meetup frames as canonical CBOR [1, [kind, fields...]] (design B.6, B.6.1)",
    meetup,
)

# --- location plaintext ---
write(
    "location-plaintext-encoding",
    "Inner plaintext of SealedLocation as canonical CBOR (design B.6)",
    [
        {
            "name": "with-heading-no-speed",
            "inputs": {
                "latitude": 515_007_000,
                "longitude": -1_246_000,
                "accuracy": 8,
                "heading": 270,
                "speed": "",
                "taken_at": 1_790_000_000,
            },
            "expected": {
                "encoding": h(
                    versioned(
                        a(i(515_007_000), i(-1_246_000), u(8), u(270), NULL, u(1_790_000_000))
                    )
                )
            },
        },
        {
            "name": "heading-360",
            "inputs": {
                "encoding": h(versioned(a(u(0), u(0), u(8), u(360), NULL, u(0))))
            },
            "expected": {"error": "OutOfRange"},
        },
    ],
)

# --- mailbox spec ---
mbox = ramp(0x11, 16)
blob = ramp(0, 256) + ramp(0, 64)
handle = ramp(0xf0, 8)
# SealedPush: [relay, relay key id, blob, urgency, handle].
listed = a(a(u(0), u(2)), u(1), b(blob), u(1), b(handle))
custom_hash = fill(0x77, 32)
custom = a(a(u(1), t("https://push.example"), b(custom_hash)), u(7), b(blob), u(0), b(handle))
write(
    "mailbox-spec-encoding",
    "POST /v1/mailboxes body as canonical CBOR (design B.4.9, D)",
    [
        {
            "name": "listed-relay-quiet",
            "inputs": {
                "mailbox_id": h(mbox),
                "ttl_seconds": 3600,
                "relay_id": 2,
                "relay_url": "",
                "key_config_hash": "",
                "key_id": 1,
                "blob": h(blob),
                "urgency": "quiet",
                "handle": h(handle),
            },
            "expected": {"encoding": h(versioned(a(b(mbox), u(3600), listed)))},
        },
        {
            "name": "custom-relay-alert",
            "inputs": {
                "mailbox_id": h(mbox),
                "ttl_seconds": 2_592_000,
                "relay_id": "",
                "relay_url": "https://push.example",
                "key_config_hash": h(custom_hash),
                "key_id": 7,
                "blob": h(blob),
                "urgency": "alert",
                "handle": h(handle),
            },
            "expected": {"encoding": h(versioned(a(b(mbox), u(2_592_000), custom)))},
        },
        {
            "name": "no-push",
            "inputs": {
                "mailbox_id": h(mbox),
                "ttl_seconds": 60,
                "relay_id": "",
                "relay_url": "",
                "key_config_hash": "",
                "key_id": "",
                "blob": "",
                "urgency": "",
                "handle": "",
            },
            "expected": {"encoding": h(versioned(a(b(mbox), u(60), NULL)))},
        },
        {
            "name": "zero-ttl",
            "inputs": {"encoding": h(versioned(a(b(mbox), u(0), NULL)))},
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "relay-key-id-over-255",
            "inputs": {
                "encoding": h(
                    versioned(
                        a(b(mbox), u(60), a(a(u(0), u(2)), u(256), b(blob), u(1), b(handle)))
                    )
                )
            },
            "expected": {"error": "Cbor"},
        },
        {
            "name": "sealed-push-without-key-id",
            "inputs": {
                "encoding": h(versioned(a(b(mbox), u(60), a(a(u(0), u(2)), b(blob), u(1), b(handle)))))
            },
            "expected": {"error": "Cbor"},
        },
    ],
)

# --- knock ---
kp = ramp(0x33, 70)
sig = fill(0x99, 64)
reply = ramp(0x55, 16)
sub_id = ramp(0x66, 16)
subscription = a(b(sub_id), listed, u(900))
write(
    "knock-encoding",
    "Knock plaintext as canonical CBOR, before padding and sealing (design B.4.2)",
    [
        {
            "name": "with-subscription",
            "inputs": {
                "matched_randomness": h(r),
                "own_randomness": h(tag),
                "leaf_index": 5,
                "party_epoch": 12,
                "key_package": h(kp),
                "reply_mailbox": h(reply),
                "subscription_id": h(sub_id),
                "subscription_ttl_seconds": 900,
                "signature": h(sig),
            },
            "expected": {
                "encoding": h(
                    versioned(
                        a(b(r), b(tag), u(5), u(12), b(kp), b(reply), subscription, b(sig))
                    )
                )
            },
        },
        {
            "name": "first-arrival",
            "inputs": {
                "matched_randomness": h(r),
                "own_randomness": "",
                "leaf_index": 0,
                "party_epoch": 1,
                "key_package": h(kp),
                "reply_mailbox": h(reply),
                "subscription_id": "",
                "subscription_ttl_seconds": 0,
                "signature": h(sig),
            },
            "expected": {
                "encoding": h(
                    versioned(a(b(r), NULL, u(0), u(1), b(kp), b(reply), NULL, b(sig)))
                )
            },
        },
    ],
)

# --- board page ---
page_records = b"".join(ramp(k, 96) for k in range(3))
page_hash = fill(0xab, 32)
cursor = ramp(0xc0, 24)
write(
    "board-page-encoding",
    "Board page as canonical CBOR: segments, raw concatenated records, cursor (design D)",
    [
        {
            "name": "closed-and-open-segments",
            "inputs": {
                "first_start_bucket": 1_956_999,
                "first_record_count": 1,
                "first_hash": h(page_hash),
                "second_start_bucket": 1_957_000,
                "second_record_count": 2,
                "records": h(page_records),
                "next": h(cursor),
            },
            "expected": {
                "encoding": h(
                    versioned(
                        a(
                            a(
                                a(u(1_956_999), u(1), b(page_hash)),
                                a(u(1_957_000), u(2), NULL),
                            ),
                            b(page_records),
                            b(cursor),
                        )
                    )
                )
            },
        },
        {
            "name": "counts-disagree",
            "inputs": {
                "encoding": h(versioned(a(a(a(u(1), u(2), NULL)), b(page_records[:96]), NULL)))
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "partial-record",
            "inputs": {
                "encoding": h(versioned(a(a(a(u(1), u(1), NULL)), b(page_records[:95]), NULL)))
            },
            "expected": {"error": "WrongLength"},
        },
    ],
)

# --- app message ---
body = ramp(0x01, 50)
write(
    "app-message-encoding",
    "Group message envelope (POST /v1/groups/{id}/msgs) as canonical CBOR (design D)",
    [
        {
            "name": "stored-alert",
            "inputs": {"epoch": 7, "body": h(body), "delivery": "stored", "wake": "alert"},
            "expected": {"encoding": h(versioned(a(u(7), b(body), u(0), u(0))))},
        },
        {
            "name": "ephemeral-no-wake",
            "inputs": {"epoch": 70_000, "body": h(body), "delivery": "ephemeral", "wake": ""},
            "expected": {"encoding": h(versioned(a(u(70_000), b(body), u(1), NULL)))},
        },
        {
            "name": "unknown-delivery",
            "inputs": {"encoding": h(versioned(a(u(7), b(body), u(2), NULL)))},
            "expected": {"error": "OutOfRange"},
        },
    ],
)

# --- push relay request ---
relay_bind = ramp(0xb0, 32)
write(
    "push-request-encoding",
    "POST /v1/push body (push relay API) as canonical CBOR: [key id, blob, bind, urgency, handle] (design D, PushReq)",
    [
        {
            "name": "alert",
            "inputs": {
                "key_id": 1,
                "blob": h(blob),
                "bind": h(relay_bind),
                "urgency": "alert",
                "handle": h(handle),
            },
            "expected": {
                "encoding": h(versioned(a(u(1), b(blob), b(relay_bind), u(0), b(handle))))
            },
        },
        {
            "name": "quiet-key-200",
            "inputs": {
                "key_id": 200,
                "blob": h(blob),
                "bind": h(relay_bind),
                "urgency": "quiet",
                "handle": h(handle),
            },
            "expected": {
                "encoding": h(versioned(a(u(200), b(blob), b(relay_bind), u(1), b(handle))))
            },
        },
        {
            "name": "short-bind",
            "inputs": {
                "encoding": h(versioned(a(u(1), b(blob), b(relay_bind[:31]), u(0), b(handle))))
            },
            "expected": {"error": "WrongLength"},
        },
        {
            "name": "key-id-over-255",
            "inputs": {
                "encoding": h(versioned(a(u(256), b(blob), b(relay_bind), u(0), b(handle))))
            },
            "expected": {"error": "Cbor"},
        },
    ],
)

# --- push relay config ---
relay_public = ramp(0x60, 32)


def key_config(key_id, public, kem=b"\x00\x20", suites=b"\x00\x04\x00\x01\x00\x01"):
    return bytes([key_id]) + kem + public + suites


write(
    "push-relay-config-encoding",
    "GET /v1/config body (push relay API) as canonical CBOR: [key config (RFC 9458, 41 bytes), [platform...], max ttl seconds, operator] (design D, RelayConfig)",
    [
        {
            "name": "both-platforms",
            "inputs": {
                "key_id": 2,
                "public_key": h(relay_public),
                "platforms": "fcm,unifiedpush",
                "max_ttl_seconds": 2_592_000,
                "operator": "Example Relay Ltd",
            },
            "expected": {
                "key_config": h(key_config(2, relay_public)),
                "encoding": h(
                    versioned(
                        a(
                            b(key_config(2, relay_public)),
                            a(u(1), u(2)),
                            u(2_592_000),
                            t("Example Relay Ltd"),
                        )
                    )
                ),
            },
        },
        {
            "name": "unifiedpush-only",
            "inputs": {
                "key_id": 1,
                "public_key": h(relay_public),
                "platforms": "unifiedpush",
                "max_ttl_seconds": 3600,
                "operator": "Ünïcode Relay",
            },
            "expected": {
                "key_config": h(key_config(1, relay_public)),
                "encoding": h(
                    versioned(
                        a(b(key_config(1, relay_public)), a(u(2)), u(3600), t("Ünïcode Relay"))
                    )
                ),
            },
        },
        {
            "name": "chacha-suite",
            "inputs": {
                "encoding": h(
                    versioned(
                        a(
                            b(key_config(1, relay_public, suites=b"\x00\x04\x00\x01\x00\x03")),
                            a(u(2)),
                            u(3600),
                            t("x"),
                        )
                    )
                )
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "unknown-platform",
            "inputs": {
                "encoding": h(
                    versioned(a(b(key_config(1, relay_public)), a(u(3)), u(3600), t("x")))
                )
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "zero-ttl",
            "inputs": {
                "encoding": h(
                    versioned(a(b(key_config(1, relay_public)), a(u(2)), u(0), t("x")))
                )
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "short-key-config",
            "inputs": {
                "encoding": h(
                    versioned(a(b(key_config(1, relay_public)[:40]), a(u(2)), u(3600), t("x")))
                )
            },
            "expected": {"error": "WrongLength"},
        },
    ],
)

# --- Privacy Pass issuance ---
PP_TOKEN_TYPE = b"\x00\x01"


def pp_request(truncated_key_id, count):
    elements = b"".join(ramp(0x02 + k, 49) for k in range(count))
    return PP_TOKEN_TYPE + bytes([truncated_key_id]) + struct.pack(">H", len(elements)) + elements


def pp_response(count):
    elements = b"".join(ramp(0x30 + k, 49) for k in range(count))
    return struct.pack(">H", len(elements)) + elements + ramp(0x90, 96)


PP_MAX_REQUEST = 2 + 1 + 2 + 300 * 49
PP_MAX_RESPONSE = 2 + 300 * 49 + 96
write(
    "privacy-pass-issue-request-encoding",
    "POST /v1/pp/issue body as canonical CBOR: [token request], the token request being pingers-anon's RFC 9578 batched request (token_type || truncated_key_id || blinded_elements<2-byte length>), opaque here and at most 5 + 300 * 49 bytes (design B.4.8, D)",
    [
        {
            "name": "two-tokens",
            "inputs": {"token_request": h(pp_request(0x7F, 2))},
            "expected": {"encoding": h(versioned(a(b(pp_request(0x7F, 2)))))},
        },
        {
            "name": "full-batch",
            "inputs": {"token_request": h(fill(0x01, PP_MAX_REQUEST))},
            "expected": {"encoding": h(versioned(a(b(fill(0x01, PP_MAX_REQUEST)))))},
        },
        {
            "name": "over-a-batch",
            "inputs": {"encoding": h(versioned(a(b(fill(0x01, PP_MAX_REQUEST + 1)))))},
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "empty",
            "inputs": {"encoding": h(versioned(a(b(b""))))},
            "expected": {"error": "OutOfRange"},
        },
    ],
)
write(
    "privacy-pass-issue-response-encoding",
    "POST /v1/pp/issue answer as canonical CBOR: [token response], the token response being pingers-anon's RFC 9578 batched response (evaluated_elements<2-byte length> || proof(96)), opaque here and at most 2 + 300 * 49 + 96 bytes (design B.4.8, D)",
    [
        {
            "name": "two-tokens",
            "inputs": {"token_response": h(pp_response(2))},
            "expected": {"encoding": h(versioned(a(b(pp_response(2)))))},
        },
        {
            "name": "full-batch",
            "inputs": {"token_response": h(fill(0x02, PP_MAX_RESPONSE))},
            "expected": {"encoding": h(versioned(a(b(fill(0x02, PP_MAX_RESPONSE)))))},
        },
        {
            "name": "over-a-batch",
            "inputs": {"encoding": h(versioned(a(b(fill(0x02, PP_MAX_RESPONSE + 1)))))},
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "text-instead-of-bytes",
            "inputs": {"encoding": h(versioned(a(t("token"))))},
            "expected": {"error": "Cbor"},
        },
    ],
)

# --- invites ---
inv_server = "https://pingers.example"
inv_mailbox = ramp(0x21, 16)
inv_secret = ramp(0xa0, 32)
inv_fpr = ramp(0x08, 8)
write(
    "invite-payload-encoding",
    "Invite link fragment payload (https://<host>/j#v1.<base64url(this)>) as canonical CBOR: [server, invite mailbox, invite secret, inviter fingerprint, expires, uses (0 multiple, 1 single)] (design D, invite flow)",
    [
        {
            "name": "single-use",
            "inputs": {
                "server": inv_server,
                "invite_mailbox": h(inv_mailbox),
                "invite_secret": h(inv_secret),
                "inviter_fingerprint": h(inv_fpr),
                "expires": 1_800_604_800,
                "uses": "single",
            },
            "expected": {
                "encoding": h(
                    versioned(
                        a(t(inv_server), b(inv_mailbox), b(inv_secret), b(inv_fpr), u(1_800_604_800), u(1))
                    )
                )
            },
        },
        {
            "name": "multiple-use",
            "inputs": {
                "server": inv_server,
                "invite_mailbox": h(inv_mailbox),
                "invite_secret": h(inv_secret),
                "inviter_fingerprint": h(inv_fpr),
                "expires": 1_800_000_000,
                "uses": "multiple",
            },
            "expected": {
                "encoding": h(
                    versioned(
                        a(t(inv_server), b(inv_mailbox), b(inv_secret), b(inv_fpr), u(1_800_000_000), u(0))
                    )
                )
            },
        },
        {
            "name": "http-server",
            "inputs": {
                "encoding": h(
                    versioned(
                        a(t("http://pingers.example"), b(inv_mailbox), b(inv_secret), b(inv_fpr), u(1), u(0))
                    )
                )
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "short-secret",
            "inputs": {
                "encoding": h(
                    versioned(
                        a(t(inv_server), b(inv_mailbox), b(inv_secret[:31]), b(inv_fpr), u(1), u(0))
                    )
                )
            },
            "expected": {"error": "WrongLength"},
        },
        {
            "name": "unknown-uses",
            "inputs": {
                "encoding": h(
                    versioned(
                        a(t(inv_server), b(inv_mailbox), b(inv_secret), b(inv_fpr), u(1), u(2))
                    )
                )
            },
            "expected": {"error": "OutOfRange"},
        },
    ],
)

join_kp = ramp(0x40, 40)
join_reply = ramp(0x51, 16)
join_sub_id = ramp(0x61, 16)
join_subscription = a(b(join_sub_id), listed, u(2_592_000))
write(
    "join-request-encoding",
    "Join request plaintext (sealed with seal_join_request, posted to the invite mailbox) as canonical CBOR: [key package, display name, reply mailbox, party subscription] (design D, invite flow)",
    [
        {
            "name": "bob",
            "inputs": {
                "key_package": h(join_kp),
                "display_name": "Bob",
                "reply_mailbox": h(join_reply),
                "subscription_id": h(join_sub_id),
                "subscription_ttl_seconds": 2_592_000,
            },
            "expected": {
                "encoding": h(versioned(a(b(join_kp), t("Bob"), b(join_reply), join_subscription)))
            },
        },
        {
            "name": "longest-name",
            "inputs": {
                "key_package": h(join_kp),
                "display_name": "é" * 100,
                "reply_mailbox": h(join_reply),
                "subscription_id": h(join_sub_id),
                "subscription_ttl_seconds": 2_592_000,
            },
            "expected": {
                "encoding": h(
                    versioned(a(b(join_kp), t("é" * 100), b(join_reply), join_subscription))
                )
            },
        },
        {
            "name": "name-too-long",
            "inputs": {
                "encoding": h(
                    versioned(a(b(join_kp), t("a" * 201), b(join_reply), join_subscription))
                )
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "empty-name",
            "inputs": {
                "encoding": h(versioned(a(b(join_kp), t(""), b(join_reply), join_subscription)))
            },
            "expected": {"error": "OutOfRange"},
        },
        {
            "name": "no-subscription",
            "inputs": {"encoding": h(versioned(a(b(join_kp), t("Bob"), b(join_reply), NULL)))},
            "expected": {"error": "Cbor"},
        },
    ],
)
