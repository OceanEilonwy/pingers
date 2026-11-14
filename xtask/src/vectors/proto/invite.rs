// SPDX-License-Identifier: AGPL-3.0-only
//! The invite flow: the link fragment's payload and the join request.

use anyhow::Result;

use super::super::cbor::{array, array_of, bytes, optional, text, uint, versioned};
use super::super::json::{Json, object};
use super::super::{VectorFile, case, error, hex, prefix, ramp};
use super::{listed_sealed_push, proto_file};

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![invite_payload()?, join_request()?])
}

/// One invite payload's fields, before encoding.
#[derive(Clone, Copy)]
struct Invite<'fields> {
    server: &'fields str,
    secret: &'fields [u8],
    expires: u32,
    /// 0 for multiple use, 1 for single use, anything else refused.
    uses: u64,
    registration_code: Option<&'fields str>,
}

impl Invite<'_> {
    fn mailbox() -> Vec<u8> {
        ramp(0x21, 16)
    }

    fn fingerprint() -> Vec<u8> {
        ramp(0x08, 8)
    }

    /// `[server, mailbox, secret, fingerprint, expires, uses]`, then the registration code
    /// only when there is one.
    fn fields(&self) -> Vec<Vec<u8>> {
        let mut fields = vec![
            text(self.server),
            bytes(&Self::mailbox()),
            bytes(self.secret),
            bytes(&Self::fingerprint()),
            uint(u64::from(self.expires)),
            uint(self.uses),
        ];
        fields.extend(self.registration_code.map(text));
        fields
    }

    fn encode(&self) -> Vec<u8> {
        versioned(array_of(self.fields()))
    }

    /// The inputs of a case that encodes this invite; `uses` names `self.uses`.
    fn inputs(&self, uses: &str) -> Json {
        let mut pairs = vec![
            ("server".to_owned(), Json::from(self.server)),
            ("invite_mailbox".to_owned(), hex(&Self::mailbox())),
            ("invite_secret".to_owned(), hex(self.secret)),
            ("inviter_fingerprint".to_owned(), hex(&Self::fingerprint())),
            ("expires".to_owned(), Json::from(i64::from(self.expires))),
            ("uses".to_owned(), Json::from(uses)),
        ];
        pairs.extend(
            self.registration_code
                .map(|code| ("registration_code".to_owned(), Json::from(code))),
        );
        Json::Object(pairs)
    }
}

fn invite_payload() -> Result<VectorFile> {
    let secret = ramp(0xa0, 32);
    let short_secret = prefix(&secret, 31)?;
    let invite = |expires: u32, uses: u64| Invite {
        server: "https://pingers.example",
        secret: &secret,
        expires,
        uses,
        registration_code: None,
    };
    let single = invite(1_800_604_800, 1);
    let multiple = invite(1_800_000_000, 0);
    let with_code = Invite {
        registration_code: Some("friends-2026"),
        ..single
    };
    let empty_code = Invite {
        registration_code: Some(""),
        ..single
    };
    let eight_elements = versioned(array_of([with_code.fields(), vec![uint(0)]].concat()));
    let http_server = Invite {
        server: "http://pingers.example",
        ..invite(1, 0)
    };
    let short = Invite {
        secret: &short_secret,
        ..invite(1, 0)
    };
    proto_file(
        "invite-payload-encoding",
        "Invite link fragment payload (https://<host>/j#v1.<base64url(this)>) as canonical CBOR: [server, invite mailbox, invite secret, inviter fingerprint, expires, uses (0 multiple, 1 single), registration code (text; present only when the inviter passes one on, so the array has 6 elements without it and 7 with it)] (design D, invite flow; design B.1 registration codes). An empty registration code is refused.",
        vec![
            case(
                "single-use",
                single.inputs("single"),
                object! { "encoding": hex(&single.encode()) },
            ),
            case(
                "multiple-use",
                multiple.inputs("multiple"),
                object! { "encoding": hex(&multiple.encode()) },
            ),
            case(
                "single-use-with-registration-code",
                with_code.inputs("single"),
                object! { "encoding": hex(&with_code.encode()) },
            ),
            case(
                "empty-registration-code",
                object! { "encoding": hex(&empty_code.encode()) },
                error("OutOfRange"),
            ),
            case(
                "eight-elements",
                object! { "encoding": hex(&eight_elements) },
                error("Cbor"),
            ),
            case(
                "http-server",
                object! { "encoding": hex(&http_server.encode()) },
                error("OutOfRange"),
            ),
            case(
                "short-secret",
                object! { "encoding": hex(&short.encode()) },
                error("WrongLength"),
            ),
            case(
                "unknown-uses",
                object! { "encoding": hex(&invite(1, 2).encode()) },
                error("OutOfRange"),
            ),
        ],
    )
}

fn join_request() -> Result<VectorFile> {
    let key_package = ramp(0x40, 40);
    let reply = ramp(0x51, 16);
    let subscription_id = ramp(0x61, 16);
    let subscription = array([
        bytes(&subscription_id),
        listed_sealed_push(),
        uint(2_592_000),
    ]);
    let request = |name: &str, subscription: Option<&[u8]>| {
        versioned(array([
            bytes(&key_package),
            text(name),
            bytes(&reply),
            optional(subscription, <[u8]>::to_vec),
        ]))
    };
    let inputs = |name: &str| {
        object! {
            "key_package": hex(&key_package),
            "display_name": name,
            "reply_mailbox": hex(&reply),
            "subscription_id": hex(&subscription_id),
            "subscription_ttl_seconds": 2_592_000,
        }
    };
    let longest = "é".repeat(100);
    proto_file(
        "join-request-encoding",
        "Join request plaintext (sealed with seal_join_request, posted to the invite mailbox) as canonical CBOR: [key package, display name, reply mailbox, party subscription] (design D, invite flow)",
        vec![
            case(
                "bob",
                inputs("Bob"),
                object! { "encoding": hex(&request("Bob", Some(&subscription))) },
            ),
            case(
                "longest-name",
                inputs(&longest),
                object! { "encoding": hex(&request(&longest, Some(&subscription))) },
            ),
            case(
                "name-too-long",
                object! { "encoding": hex(&request(&"a".repeat(201), Some(&subscription))) },
                error("OutOfRange"),
            ),
            case(
                "empty-name",
                object! { "encoding": hex(&request("", Some(&subscription))) },
                error("OutOfRange"),
            ),
            case(
                "no-subscription",
                object! { "encoding": hex(&request("Bob", None)) },
                error("Cbor"),
            ),
        ],
    )
}
