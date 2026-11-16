// SPDX-License-Identifier: AGPL-3.0-only
//! The first-phone setup link's payload (`https://<server>/j#s1.<base64url(this)>`), which the
//! owner's appliance shows as a QR code.

use anyhow::Result;

use super::super::cbor::{array, array_of, bytes, text, versioned};
use super::super::json::{Json, object};
use super::super::{VectorFile, case, error, hex, ramp};
use super::proto_file;

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![setup_payload()?])
}

/// One payload's fields, before encoding. Pins are given as their 32-byte hashes.
struct Setup<'fields> {
    server: &'fields str,
    code: &'fields str,
    gateway: Vec<Vec<u8>>,
    push_relay: Vec<Vec<u8>>,
    map_host: Vec<Vec<u8>>,
}

impl Setup<'_> {
    fn encode(&self) -> Vec<u8> {
        let pins = |pins: &[Vec<u8>]| array_of(pins.iter().map(|pin| bytes(pin)).collect());
        versioned(array([
            text(self.server),
            text(self.code),
            pins(&self.gateway),
            pins(&self.push_relay),
            pins(&self.map_host),
        ]))
    }

    /// The inputs: each pin list as the hex of its hashes, concatenated.
    fn inputs(&self) -> Json {
        object! {
            "server": self.server,
            "registration_code": self.code,
            "gateway_pins": hex(&self.gateway.concat()),
            "push_relay_pins": hex(&self.push_relay.concat()),
            "map_host_pins": hex(&self.map_host.concat()),
        }
    }
}

fn setup_payload() -> Result<VectorFile> {
    let two = |start: usize| vec![ramp(start, 32), ramp(start.saturating_add(0x20), 32)];
    let link = Setup {
        server: "https://pingers.example",
        code: "0123456789abcdef0123456789abcdef",
        gateway: two(0x10),
        push_relay: two(0x80),
        map_host: vec![ramp(0xa0, 32)],
    };
    let one_each = Setup {
        gateway: vec![ramp(0x10, 32)],
        push_relay: vec![ramp(0x80, 32)],
        map_host: Vec::new(),
        ..two_pins(&link)
    };
    let empty_code = Setup {
        code: "",
        ..two_pins(&link)
    };
    let no_gateway_pins = Setup {
        gateway: Vec::new(),
        ..two_pins(&link)
    };
    let five_pins = Setup {
        push_relay: [0x00, 0x20, 0x40, 0x60, 0x80]
            .map(|start| ramp(start, 32))
            .to_vec(),
        ..two_pins(&link)
    };
    let short_pin = Setup {
        gateway: vec![ramp(0x10, 31)],
        ..two_pins(&link)
    };
    let http_server = Setup {
        server: "http://pingers.example",
        ..two_pins(&link)
    };
    proto_file(
        "setup-payload-encoding",
        "First-phone setup link payload (https://<server>/j#s1.<base64url(this)>, shown by the owner's appliance as a QR code) as canonical CBOR: [server, registration code (text, 1 to 128 bytes), gateway key pins ([32-byte SHA-256 of the key config], served key first, 1 to 4), push relay key pins (the same, published key first, 1 to 4), map host key pins (the same, 0 to 4)]. Inputs give each pin list as the hex of its hashes, concatenated.",
        vec![
            case(
                "link",
                link.inputs(),
                object! { "encoding": hex(&link.encode()) },
            ),
            case(
                "one-pin-each",
                one_each.inputs(),
                object! { "encoding": hex(&one_each.encode()) },
            ),
            case(
                "empty-registration-code",
                object! { "encoding": hex(&empty_code.encode()) },
                error("OutOfRange"),
            ),
            case(
                "no-gateway-pins",
                object! { "encoding": hex(&no_gateway_pins.encode()) },
                error("OutOfRange"),
            ),
            case(
                "five-push-relay-pins",
                object! { "encoding": hex(&five_pins.encode()) },
                error("OutOfRange"),
            ),
            case(
                "short-pin",
                object! { "encoding": hex(&short_pin.encode()) },
                error("WrongLength"),
            ),
            case(
                "http-server",
                object! { "encoding": hex(&http_server.encode()) },
                error("OutOfRange"),
            ),
        ],
    )
}

/// A copy of `setup` to change one field of.
fn two_pins<'fields>(setup: &Setup<'fields>) -> Setup<'fields> {
    Setup {
        server: setup.server,
        code: setup.code,
        gateway: setup.gateway.clone(),
        push_relay: setup.push_relay.clone(),
        map_host: setup.map_host.clone(),
    }
}
