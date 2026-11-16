// SPDX-License-Identifier: AGPL-3.0-only
//! The first-phone setup link (pingers-proto's `SetupPayload`): the owner's appliance shows it
//! as a QR code, and the owner's first phone registers with its code.
//!
//! [`check`] reads only the instance and the store: the link's server must be this app's, the
//! account not made yet, and with build pins the link's served keys (its first gateway pin and
//! first push relay pin) among them. [`set_up`] checks again and, before anything goes out with
//! the code, meets the gateway key through the relay in a build without gateway pins, which must
//! be one of the link's. The link's push relay pins then become the first-use pins of a build
//! without them (unless some were learned already), so that key is checked from the start. Only
//! then is the code offered, as an invite link's is, and the account registered.
//!
//! Nothing here retries. A refused code is recorded by pingers-sync with the code and not sent
//! again (the same link answers [`SetupOutcome::Refused`] without a request); an unreachable
//! server is reported for the user to tap again.

use pingers_proto::{KeyConfigHash, ServerId, SetupPayload};
use pingers_sync::{CodeSource, CodeStanding, SyncError, WakeBudget};

use super::push::PushRelayPinning;
use super::sync::Net;
use crate::ClientError;
use crate::ports::{SetupCheck, SetupOutcome};
use crate::snapshot::Host;

/// A server's host, as screens name it: "pingers.example.org".
fn host(server: &ServerId) -> Result<Host, ClientError> {
    let origin = server.as_str();
    Host::new(origin.strip_prefix("https://").unwrap_or(origin))
        .map_err(|_never| ClientError::Internal)
}

/// Whether a build's pin set, if it has one, holds the link's served key: the first pin.
fn build_allows(build: Option<&pingers_ohttp::KeyPins>, link: &[KeyConfigHash]) -> bool {
    build.is_none_or(|pins| link.first().is_some_and(|served| pins.contains(served)))
}

/// What the link is to this device, from the instance and the store only: the server first,
/// then the account (made already, or this code refused before), then the build's pins.
///
/// # Errors
///
/// A storage failure; [`Unavailable::Sync`](crate::Unavailable::Sync) without an instance.
pub(crate) async fn check(net: &Net, link: &SetupPayload) -> Result<SetupCheck, ClientError> {
    let app = net.engine()?.server();
    if link.server != app {
        return Ok(SetupCheck::OtherServer {
            app: host(&app)?,
            link: host(&link.server)?,
        });
    }
    match net
        .engine()?
        .code_standing(link.registration_code.clone())
        .await?
    {
        CodeStanding::Registered => return Ok(SetupCheck::AlreadySetUp),
        CodeStanding::RefusedBefore => return Ok(SetupCheck::Refused),
        CodeStanding::Open => {}
    }
    let server = host(&app)?;
    let push_allowed = match net.learned.push_relay_pinning() {
        PushRelayPinning::Build(pins) => build_allows(Some(pins), &link.push_relay_pins),
        PushRelayPinning::NoPushRelay | PushRelayPinning::FirstUse => true,
    };
    if !build_allows(net.gateway_pins.as_ref(), &link.gateway_pins) || !push_allowed {
        return Ok(SetupCheck::BuildKeysDiffer { server });
    }
    Ok(SetupCheck::Ready { server })
}

/// Uses the link: checks it, meets the gateway key in a build without gateway pins, seeds the
/// push relay's first-use pins, then offers the code and registers.
///
/// # Errors
///
/// A storage failure; [`Unavailable::Sync`](crate::Unavailable::Sync) without an instance.
pub(crate) async fn set_up(net: &Net, link: SetupPayload) -> Result<SetupOutcome, ClientError> {
    let server = match check(net, &link).await? {
        SetupCheck::Ready { server } => server,
        SetupCheck::AlreadySetUp => return Ok(SetupOutcome::AlreadySetUp),
        SetupCheck::Refused => return Ok(SetupOutcome::Refused),
        SetupCheck::OtherServer { app, link } => {
            return Ok(SetupOutcome::OtherServer { app, link });
        }
        SetupCheck::BuildKeysDiffer { server } => {
            return Ok(SetupOutcome::BuildKeysDiffer { server });
        }
    };
    let engine = net.engine()?;
    if net.gateway_pins.is_none() {
        let checked = engine.check_relay(WakeBudget::UNLIMITED).await;
        net.observe(&checked);
        match checked {
            Ok(()) => {}
            // The key met first in this process differs from the one served now.
            Err(SyncError::RelayKeyMismatch) => {
                return Ok(SetupOutcome::ServerKeysDiffer { server });
            }
            Err(SyncError::Store(error)) => return Err(SyncError::Store(error).into()),
            Err(_) => return Ok(SetupOutcome::Unreachable { server }),
        }
        let met = net
            .backend
            .transport
            .as_ref()
            .and_then(|transport| transport.gateway_key_hash());
        if !met.is_some_and(|hash| link.gateway_pins.contains(&hash)) {
            return Ok(SetupOutcome::ServerKeysDiffer { server });
        }
    }
    let SetupPayload {
        registration_code,
        push_relay_pins,
        ..
    } = link;
    net.seed_first_use_pins(push_relay_pins).await?;
    engine
        .offer_registration_code(registration_code, CodeSource::Invite)
        .await?;
    let registered = engine.ensure_registered(WakeBudget::UNLIMITED).await;
    net.observe(&registered);
    match registered {
        Ok(()) => {}
        Err(SyncError::RegistrationRefused) => return Ok(SetupOutcome::Refused),
        Err(SyncError::RelayKeyMismatch) => return Ok(SetupOutcome::ServerKeysDiffer { server }),
        Err(SyncError::Store(error)) => return Err(SyncError::Store(error).into()),
        Err(_) => return Ok(SetupOutcome::Unreachable { server }),
    }
    // The tokens, the leaf and the server's parameters, as far as the network allows: a
    // failure here leaves them to the next command or wake, as everywhere.
    net.prepare_credentials().await?;
    Ok(SetupOutcome::SetUp)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_ohttp::KeyPins;
    use pingers_proto::KeyConfigHash;

    use super::build_allows;

    fn hash(byte: u8) -> KeyConfigHash {
        KeyConfigHash::from_bytes([byte; 32])
    }

    #[test]
    fn only_the_links_served_key_must_be_pinned() -> Result<(), pingers_ohttp::OhttpError> {
        let build = KeyPins::new([hash(1), hash(2)])?;
        // The served key (first) is pinned; the link's next key need not be.
        assert!(build_allows(Some(&build), &[hash(2), hash(9)]));
        // The link's next key is pinned but its served key is not.
        assert!(!build_allows(Some(&build), &[hash(9), hash(1)]));
        assert!(!build_allows(Some(&build), &[]));
        // A build without pins allows any link.
        assert!(build_allows(None, &[hash(9)]));
        Ok(())
    }
}
