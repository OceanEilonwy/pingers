// SPDX-License-Identifier: AGPL-3.0-only
//! The invite flow (join flow A): the link fragment's payload, the join request, and the invite
//! post that carries a join request or its withdrawal.

use anyhow::Result;

use super::super::cbor::{array, array_of, bytes, optional, text, uint, versioned};
use super::super::json::{Json, object};
use super::super::{VectorFile, case, error, hex, prefix, ramp};
use super::{listed_sealed_push, proto_file};

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![invite_payload()?, join_request()?, invite_post()?])
}

/// One invite payload's fields, before encoding.
#[derive(Clone, Copy)]
struct Invite<'fields> {
    server: &'fields str,
    secret: &'fields [u8],
    expires: u32,
    /// The server's gateway key config hashes: 1 to 4.
    gateway_pins: &'fields [Vec<u8>],
    /// Its push relay's key config hashes: 0 to 4.
    push_relay_pins: &'fields [Vec<u8>],
    registration_code: Option<&'fields str>,
}

impl Invite<'_> {
    fn mailbox() -> Vec<u8> {
        ramp(0x21, 16)
    }

    fn fingerprint() -> Vec<u8> {
        ramp(0x08, 8)
    }

    /// `[server, mailbox, secret, fingerprint, expires, gateway pins, push relay pins]`, then the
    /// registration code only when there is one.
    fn fields(&self) -> Vec<Vec<u8>> {
        let mut fields = vec![
            text(self.server),
            bytes(&Self::mailbox()),
            bytes(self.secret),
            bytes(&Self::fingerprint()),
            uint(u64::from(self.expires)),
            array_of(self.gateway_pins.iter().map(|pin| bytes(pin)).collect()),
            array_of(self.push_relay_pins.iter().map(|pin| bytes(pin)).collect()),
        ];
        fields.extend(self.registration_code.map(text));
        fields
    }

    fn encode(&self) -> Vec<u8> {
        versioned(array_of(self.fields()))
    }

    /// The inputs of a case that encodes this invite.
    fn inputs(&self) -> Json {
        let mut pairs = vec![
            ("server".to_owned(), Json::from(self.server)),
            ("invite_mailbox".to_owned(), hex(&Self::mailbox())),
            ("invite_secret".to_owned(), hex(self.secret)),
            ("inviter_fingerprint".to_owned(), hex(&Self::fingerprint())),
            ("expires".to_owned(), Json::from(i64::from(self.expires))),
            ("gateway_pins".to_owned(), pins(self.gateway_pins)),
            ("push_relay_pins".to_owned(), pins(self.push_relay_pins)),
        ];
        pairs.extend(
            self.registration_code
                .map(|code| ("registration_code".to_owned(), Json::from(code))),
        );
        Json::Object(pairs)
    }
}

/// Pins as a JSON array of hex strings.
fn pins(pins: &[Vec<u8>]) -> Json {
    Json::Array(pins.iter().map(|pin| hex(pin)).collect())
}

#[expect(
    clippy::too_many_lines,
    reason = "every case of the vector file, side by side"
)]
fn invite_payload() -> Result<VectorFile> {
    let secret = ramp(0xa0, 32);
    let gateway = [ramp(0xc0, 32), ramp(0xe0, 32)];
    let push_relay = [ramp(0x70, 32)];
    let five: Vec<Vec<u8>> = (0..5).map(|index| ramp(index, 32)).collect();
    let short_secret = prefix(&secret, 31)?;
    let invite = |expires: u32| Invite {
        server: "https://pingers.example",
        secret: &secret,
        expires,
        gateway_pins: &gateway,
        push_relay_pins: &push_relay,
        registration_code: None,
    };
    let link = invite(1_800_604_800);
    let earlier = invite(1_800_000_000);
    let with_code = Invite {
        registration_code: Some("friends-2026"),
        ..link
    };
    let empty_code = Invite {
        registration_code: Some(""),
        ..link
    };
    let without_push_pins = Invite {
        push_relay_pins: &[],
        ..earlier
    };
    let no_gateway_pins = Invite {
        gateway_pins: &[],
        ..link
    };
    let too_many_pins = Invite {
        gateway_pins: &five,
        ..link
    };
    let nine_elements = versioned(array_of([with_code.fields(), vec![uint(0)]].concat()));
    // The layout before join flow A: a use count (1, single use) after the expiry.
    let old_layout = versioned(array_of([link.fields(), vec![uint(1)]].concat()));
    let http_server = Invite {
        server: "http://pingers.example",
        ..invite(1)
    };
    let short = Invite {
        secret: &short_secret,
        ..invite(1)
    };
    proto_file(
        "invite-payload-encoding",
        "Invite link fragment payload (https://<host>/j#v1.<base64url(this)>) as canonical CBOR: [server, invite mailbox, invite secret, inviter fingerprint, expires, gateway pins (array of 1 to 4 SHA-256 key config hashes of the server's OHTTP gateway), push relay pins (array of 0 to 4 hashes of its push relay's key config), registration code (text; present only when the inviter passes one on, so the array has 7 elements without it and 8 with it)] (design D, invite flow; join flow A: nothing about the Party, and no use count, since every invite waits for the inviter and stops after the first joiner is let in; design B.1 registration codes; B.12 key pinning: a joiner new to the server pins the inviter's hashes, never the first key it sees). An empty registration code is refused.",
        vec![
            case(
                "link",
                link.inputs(),
                object! { "encoding": hex(&link.encode()) },
            ),
            case(
                "earlier-expiry",
                earlier.inputs(),
                object! { "encoding": hex(&earlier.encode()) },
            ),
            case(
                "with-registration-code",
                with_code.inputs(),
                object! { "encoding": hex(&with_code.encode()) },
            ),
            case(
                "empty-registration-code",
                object! { "encoding": hex(&empty_code.encode()) },
                error("OutOfRange"),
            ),
            case(
                "without-push-relay-pins",
                without_push_pins.inputs(),
                object! { "encoding": hex(&without_push_pins.encode()) },
            ),
            case(
                "no-gateway-pins",
                object! { "encoding": hex(&no_gateway_pins.encode()) },
                error("OutOfRange"),
            ),
            case(
                "five-gateway-pins",
                object! { "encoding": hex(&too_many_pins.encode()) },
                error("OutOfRange"),
            ),
            case(
                "nine-elements",
                object! { "encoding": hex(&nine_elements) },
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
                "old-layout-with-use-count",
                object! { "encoding": hex(&old_layout) },
                error("Cbor"),
            ),
        ],
    )
}

/// The join request every case starts from: Bob's key package, reply mailbox and, when given,
/// his Party subscription.
struct Join {
    key_package: Vec<u8>,
    x_wing_key_package: Vec<u8>,
    reply: Vec<u8>,
    subscription_id: Vec<u8>,
}

impl Join {
    fn new() -> Self {
        Self {
            key_package: ramp(0x40, 40),
            x_wing_key_package: ramp(0x90, 24),
            reply: ramp(0x51, 16),
            subscription_id: ramp(0x61, 16),
        }
    }

    fn subscription(&self) -> Vec<u8> {
        array([
            bytes(&self.subscription_id),
            listed_sealed_push(),
            uint(2_592_000),
        ])
    }

    /// The unversioned body `[key package, X-Wing key package or null, name, reply mailbox,
    /// last]`, `last` given encoded (a subscription, `null`, or something malformed). Bob
    /// sends an X-Wing key package only when he has a subscription, so both shapes of each
    /// optional field appear.
    fn body_with_last(&self, name: &str, last: Vec<u8>, x_wing: bool) -> Vec<u8> {
        array([
            bytes(&self.key_package),
            optional(x_wing.then_some(self.x_wing_key_package.as_slice()), bytes),
            text(name),
            bytes(&self.reply),
            last,
        ])
    }

    /// The unversioned body with a subscription, or `null` without one.
    fn body(&self, name: &str, subscription: Option<&[u8]>) -> Vec<u8> {
        self.body_with_last(
            name,
            optional(subscription, <[u8]>::to_vec),
            subscription.is_some(),
        )
    }

    /// The inputs of a case, led by `extra`; `""` means no X-Wing key package or no subscription
    /// (Bob sends both or neither).
    fn inputs(&self, extra: Vec<(String, Json)>, name: &str, with_subscription: bool) -> Json {
        let mut pairs = extra;
        pairs.extend([
            ("key_package".to_owned(), hex(&self.key_package)),
            (
                "x_wing_key_package".to_owned(),
                if with_subscription {
                    hex(&self.x_wing_key_package)
                } else {
                    Json::from("")
                },
            ),
            ("display_name".to_owned(), Json::from(name)),
            ("reply_mailbox".to_owned(), hex(&self.reply)),
        ]);
        if with_subscription {
            pairs.extend([
                ("subscription_id".to_owned(), hex(&self.subscription_id)),
                (
                    "subscription_ttl_seconds".to_owned(),
                    Json::from(2_592_000_i64),
                ),
            ]);
        } else {
            pairs.push(("subscription_id".to_owned(), Json::from("")));
        }
        Json::Object(pairs)
    }
}

fn join_request() -> Result<VectorFile> {
    let join = Join::new();
    let subscription = join.subscription();
    let request =
        |name: &str, subscription: Option<&[u8]>| versioned(join.body(name, subscription));
    let longest = "é".repeat(100);
    proto_file(
        "join-request-encoding",
        "Join request plaintext (sealed with seal_join_request, posted to the invite mailbox) as canonical CBOR: [key package, X-Wing key package or null when the joiner cannot make one, display name, reply mailbox, party subscription or null when the joiner has wake-up pushes off] (design D, invite flow). Inside an invite post it is nested as [0, this body] (invite-post-encoding)",
        vec![
            case(
                "bob",
                join.inputs(Vec::new(), "Bob", true),
                object! { "encoding": hex(&request("Bob", Some(&subscription))) },
            ),
            case(
                "longest-name",
                join.inputs(Vec::new(), &longest, true),
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
                join.inputs(Vec::new(), "Bob", false),
                object! { "encoding": hex(&request("Bob", None)) },
            ),
            case(
                "subscription-not-an-array",
                object! { "encoding": hex(&versioned(join.body_with_last("Bob", uint(0), false))) },
                error("Cbor"),
            ),
        ],
    )
}

fn invite_post() -> Result<VectorFile> {
    let join = Join::new();
    let post = |kind: u64, body: Vec<u8>| versioned(array([uint(kind), body]));
    let short_mailbox = prefix(&join.reply, 15)?;
    proto_file(
        "invite-post-encoding",
        "What a joiner posts to an invite mailbox (sealed with seal_join_request) as canonical CBOR: [0, join request body] to ask to join, or [1, reply mailbox] to withdraw the request whose Welcome would go to that mailbox (design D, invite flow; join flow A). The join request body is as in join-request-encoding.",
        vec![
            case(
                "join-without-subscription",
                join.inputs(vec![("kind".to_owned(), Json::from("join"))], "Bob", false),
                object! { "encoding": hex(&post(0, join.body("Bob", None))) },
            ),
            case(
                "withdraw",
                object! {
                    "kind": "withdraw",
                    "reply_mailbox": hex(&join.reply),
                },
                object! { "encoding": hex(&post(1, bytes(&join.reply))) },
            ),
            case(
                "unknown-kind",
                object! { "encoding": hex(&post(2, bytes(&join.reply))) },
                error("OutOfRange"),
            ),
            case(
                "withdraw-short-mailbox",
                object! { "encoding": hex(&post(1, bytes(&short_mailbox))) },
                error("WrongLength"),
            ),
            case(
                "bare-join-request-is-not-a-post",
                object! { "encoding": hex(&versioned(join.body("Bob", None))) },
                error("Cbor"),
            ),
        ],
    )
}
