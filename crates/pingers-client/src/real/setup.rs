// SPDX-License-Identifier: AGPL-3.0-only
//! The first-phone setup link (pingers-proto's `SetupPayload`): the owner's appliance shows it
//! as a QR code, and the owner's first phone makes its account on that appliance's server with
//! the link's code.
//!
//! The account is made on the server the link names, with that server's own engine (`Link`,
//! one per server). A link carries the server's pins as an invite does:
//!
//! - A server this device knows already (the built-in one, with the build's pins, or one
//!   recorded earlier) keeps its pins: the link's served keys (its first gateway pin and, when
//!   both have one, its first push relay pin) must be among them, or the link is
//!   [`SetupCheck::BuildKeysDiffer`].
//! - A server this device does not know is looked at through its relay as an invite's server
//!   is (`probe.rs`) and recorded with the link's pins once the keys it serves match them; a
//!   mismatch is [`SetupOutcome::ServerKeysDiffer`]. That includes the server of a build
//!   without gateway pins, which then keeps the link's pins (`servers.rs`).
//!
//! [`check`] reads only the store: nothing is sent. [`set_up`] checks again; only once every
//! check passed is the code offered (as an invite link's is) and the account registered.
//!
//! Nothing here retries. A refused code is recorded by pingers-sync with the code and not sent
//! again (the same link answers [`SetupOutcome::Refused`] without a request); an unreachable
//! server is reported for the user to tap again.

use std::sync::Arc;

use pingers_ohttp::KeyPins;
use pingers_proto::{KeyConfigHash, ServerId, SetupPayload};
use pingers_sync::{CodeSource, CodeStanding, SyncError, WakeBudget};

use super::probe::InvitePins;
use super::servers::Link;
use super::sync::Net;
use crate::ports::{SetupCheck, SetupOutcome};
use crate::snapshot::Host;
use crate::{ClientError, ServerProblem};

/// A server's host, as screens name it: "pingers.example.org".
fn host(server: &ServerId) -> Result<Host, ClientError> {
    let origin = server.as_str();
    Host::new(origin.strip_prefix("https://").unwrap_or(origin))
        .map_err(|_never| ClientError::Internal)
}

/// Whether a pin set, if there is one, holds the link's served key: its first pin.
fn pins_allow(pins: Option<&KeyPins>, link: &[KeyConfigHash]) -> bool {
    pins.is_none_or(|pins| link.first().is_some_and(|served| pins.contains(served)))
}

/// Whether the pins this device keeps for the link's server allow the link's served keys.
fn known_pins_allow(known: &Link, link: &SetupPayload) -> bool {
    let gateway = KeyPins::new(known.record.gateway_pins.iter().copied()).ok();
    let push_relay = known
        .learned
        .push_relay_pins()
        .filter(|_pinned| !link.push_relay_pins.is_empty());
    pins_allow(gateway.as_ref(), &link.gateway_pins)
        && pins_allow(push_relay, &link.push_relay_pins)
}

/// What the link is to this device, from the store only: the account on the link's server
/// (made already, or this code refused before), then the pins this device keeps for it.
///
/// # Errors
///
/// A storage failure.
pub(crate) async fn check(net: &Net, link: &SetupPayload) -> Result<SetupCheck, ClientError> {
    let server = host(&link.server)?;
    // A server with no link yet has no account on this device either.
    let Ok(known) = net.servers.link(&link.server).await else {
        return Ok(SetupCheck::Ready { server });
    };
    match known
        .engine
        .code_standing(link.registration_code.clone())
        .await?
    {
        CodeStanding::Registered => return Ok(SetupCheck::AlreadySetUp),
        CodeStanding::RefusedBefore => return Ok(SetupCheck::Refused),
        CodeStanding::Open => {}
    }
    if !known_pins_allow(&known, link) {
        return Ok(SetupCheck::BuildKeysDiffer { server });
    }
    Ok(SetupCheck::Ready { server })
}

/// The link's server: the one this device knows, or one recorded now with the link's pins
/// once the keys it serves match them.
async fn link_server(
    net: &Net,
    link: &SetupPayload,
    server: &Host,
) -> Result<Result<Arc<Link>, SetupOutcome>, ClientError> {
    if let Ok(known) = net.servers.link(&link.server).await {
        return Ok(Ok(known));
    }
    let Ok(gateway) = KeyPins::new(link.gateway_pins.iter().copied()) else {
        return Ok(Err(SetupOutcome::ServerKeysDiffer {
            server: server.clone(),
        }));
    };
    let pins = InvitePins {
        gateway,
        push_relay: KeyPins::new(link.push_relay_pins.iter().copied()).ok(),
    };
    match net.invited_server(&link.server, pins).await {
        Ok(recorded) => Ok(Ok(recorded)),
        Err(ClientError::Server(ServerProblem::KeyMismatch)) => {
            Ok(Err(SetupOutcome::ServerKeysDiffer {
                server: server.clone(),
            }))
        }
        Err(ClientError::Server(_)) => Ok(Err(SetupOutcome::Unreachable {
            server: server.clone(),
        })),
        Err(error) => Err(error),
    }
}

/// Uses the link: checks it, records the link's server with its pins if this device does not
/// know it, then offers the code and registers there.
///
/// # Errors
///
/// A storage failure.
pub(crate) async fn set_up(net: &Net, link: SetupPayload) -> Result<SetupOutcome, ClientError> {
    let server = match check(net, &link).await? {
        SetupCheck::Ready { server } => server,
        SetupCheck::AlreadySetUp => return Ok(SetupOutcome::AlreadySetUp),
        SetupCheck::Refused => return Ok(SetupOutcome::Refused),
        SetupCheck::BuildKeysDiffer { server } => {
            return Ok(SetupOutcome::BuildKeysDiffer { server });
        }
    };
    let target = match link_server(net, &link, &server).await? {
        Ok(target) => target,
        Err(outcome) => return Ok(outcome),
    };
    target
        .engine
        .offer_registration_code(link.registration_code, CodeSource::Invite)
        .await?;
    let registered = target.engine.ensure_registered(WakeBudget::UNLIMITED).await;
    target.observe(&registered);
    match registered {
        Ok(()) => {}
        Err(SyncError::RegistrationCodeRefused | SyncError::RegistrationCodeRequired) => {
            return Ok(SetupOutcome::Refused);
        }
        Err(SyncError::RelayKeyMismatch) => return Ok(SetupOutcome::ServerKeysDiffer { server }),
        Err(SyncError::Store(error)) => return Err(SyncError::Store(error).into()),
        Err(_) => return Ok(SetupOutcome::Unreachable { server }),
    }
    // The tokens, the leaf and the server's parameters, as far as the network allows: a
    // failure here leaves them to the next command or wake, as everywhere.
    target.prepare_credentials().await?;
    Ok(SetupOutcome::SetUp)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_ohttp::KeyPins;
    use pingers_proto::KeyConfigHash;

    use super::pins_allow;

    fn hash(byte: u8) -> KeyConfigHash {
        KeyConfigHash::from_bytes([byte; 32])
    }

    #[test]
    fn only_the_links_served_key_must_be_pinned() -> Result<(), pingers_ohttp::OhttpError> {
        let kept = KeyPins::new([hash(1), hash(2)])?;
        // The served key (first) is pinned; the link's next key need not be.
        assert!(pins_allow(Some(&kept), &[hash(2), hash(9)]));
        // The link's next key is pinned but its served key is not.
        assert!(!pins_allow(Some(&kept), &[hash(9), hash(1)]));
        assert!(!pins_allow(Some(&kept), &[]));
        // Without pins kept there is nothing to differ from.
        assert!(pins_allow(None, &[hash(9)]));
        Ok(())
    }
}
