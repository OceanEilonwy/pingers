// SPDX-License-Identifier: AGPL-3.0-only
//! Adding a server this device has not used: one entered by address on New party, or one an
//! invite link names.
//!
//! 1. The server's `/v1/params` are fetched direct (pingers-sync's `params_direct`): that is
//!    how the device learns its OHTTP relays. The registration that follows takes the same
//!    direct route, so the server learns nothing new from it.
//! 2. Through the advertised relay: the gateway key config, the parameters again (they must
//!    list the same relays, or the server is refused), and the chosen push relay's key config.
//! 3. A server entered by address has no pins yet: nothing is recorded until the user looks at
//!    its keys and calls `trust_server`, the one place pins are first recorded
//!    ([`ServerTrust::Confirmed`]). A server named by an invite comes with the inviter's pins,
//!    which its keys must match ([`ServerTrust::Invite`]); a mismatch is the relay key warning.
//!
//! The push relay chosen is, in order: the one whose key the invite pins, the build's own push
//! relay (the publisher's, the only one that can wake the official app through FCM) if the
//! server lists it, else the first the server lists. The build's push relay always keeps the
//! build's pins.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use pingers_ohttp::KeyPins;
use pingers_proto::{KeyConfigHash, Params, PushRelayInfo, ServerId};
use pingers_store::{ServerRecord, ServerTrust};
use pingers_sync::{SyncError, WakeBudget, params_direct};
use pingers_transport::KeyPin;

use super::network;
use super::push::same_origin;
use super::servers::Link;
use super::sync::Net;
use crate::ports::{ProbedPushRelay, ServerProbe};
use crate::snapshot::{Host, Operator};
use crate::{ClientError, ServerProblem};

/// The pins an invite link carries for its server.
#[derive(Clone, Debug)]
pub(crate) struct InvitePins {
    pub(crate) gateway: KeyPins,
    pub(crate) push_relay: Option<KeyPins>,
}

/// Servers looked at and not yet trusted, by origin. In memory only.
#[derive(Default)]
pub(crate) struct Probes(Mutex<HashMap<ServerId, (ServerRecord, Params)>>);

impl Probes {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ServerId, (ServerRecord, Params)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What looking at a server found.
struct Looked {
    record: ServerRecord,
    /// The parameters fetched through the relay, which the new link starts with.
    params: Params,
    probe: ServerProbe,
}

/// A server origin from what the user typed: a host (`pingers.example`, with an optional
/// port) or a full `https://` origin, without a path.
///
/// # Errors
///
/// [`ServerProblem::InvalidAddress`].
pub(crate) fn server_address(text: &str) -> Result<ServerId, ClientError> {
    let trimmed = text.trim().trim_end_matches('/');
    let origin = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let host = origin.strip_prefix("https://").unwrap_or_default();
    if host.is_empty()
        || host.ends_with(':')
        || host.contains(['/', '?', '#', '@'])
        || host.contains(char::is_whitespace)
    {
        return Err(ServerProblem::InvalidAddress.into());
    }
    ServerId::new(origin.to_ascii_lowercase())
        .map_err(|_not_https| ServerProblem::InvalidAddress.into())
}

fn host_of(url: &str) -> Result<Host, ClientError> {
    let rest = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    let host = rest.split('/').next().unwrap_or(rest);
    Host::new(host).map_err(|_invalid| ServerProblem::Unreachable.into())
}

/// A server's relays as a sorted set of origins, so two lists name the same relays whatever
/// their order or spelling (`/ohttp` or not).
fn relay_set(relays: &[pingers_proto::RelayUrl]) -> Vec<String> {
    let mut relays: Vec<String> = relays
        .iter()
        .map(|relay| network::relay_origin(relay.as_str()).to_owned())
        .collect();
    relays.sort();
    relays
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other failure means the server could not be reached as a Pingers server"
)]
fn problem(error: &SyncError) -> ClientError {
    match error {
        SyncError::NoRelay { .. } => ServerProblem::RelayUnreachable.into(),
        SyncError::RelayKeyMismatch | SyncError::PushRelayKeyMismatch => {
            ServerProblem::KeyMismatch.into()
        }
        SyncError::Store(_) => ClientError::Internal,
        _ => ServerProblem::Unreachable.into(),
    }
}

impl Net {
    /// The link to the server an invite payload names, recorded with the payload's pins if
    /// this device does not know it yet.
    ///
    /// # Errors
    ///
    /// As [`Net::invited_server`].
    pub(crate) async fn invite_link(
        &self,
        payload: &pingers_proto::InvitePayload,
    ) -> Result<Arc<Link>, ClientError> {
        let pins = InvitePins {
            gateway: KeyPins::new(payload.gateway_pins.iter().copied())
                .map_err(|_none| ServerProblem::KeyMismatch)?,
            push_relay: KeyPins::new(payload.push_relay_pins.iter().copied()).ok(),
        };
        self.invited_server(&payload.server, pins).await
    }
}

impl Net {
    /// Looks at the server at `address` for New party: its operator, its relays and the keys
    /// it serves, for the user to trust ([`Net::trust_server`]). Nothing is recorded. A server
    /// this device already knows is answered from what it knows, with `known` set.
    ///
    /// # Errors
    ///
    /// [`ClientError::Server`] with what went wrong; a store failure.
    pub(crate) async fn probe_server(&self, address: &str) -> Result<ServerProbe, ClientError> {
        let server = server_address(address)?;
        let looked = self.look(&server, None).await?;
        if self.servers.link(&server).await.is_ok() {
            return Ok(ServerProbe {
                known: true,
                ..looked.probe
            });
        }
        self.probes
            .lock()
            .insert(server, (looked.record, looked.params));
        Ok(looked.probe)
    }

    /// Trusts the keys of a server looked at with [`Net::probe_server`]: records its pins
    /// ([`ServerTrust::Confirmed`]) and builds its link. Its keys must still be the ones shown;
    /// a later change raises the relay key warning.
    ///
    /// # Errors
    ///
    /// [`ServerProblem::NotProbed`] if it was not looked at in this process; a store failure.
    pub(crate) async fn trust_server(&self, server: &ServerId) -> Result<(), ClientError> {
        let (record, params) = self
            .probes
            .lock()
            .remove(server)
            .ok_or(ServerProblem::NotProbed)?;
        let link = self.servers.add(record).await?;
        link.learn_params(params);
        Ok(())
    }

    /// The link to the server an invite names: the known one, or a new one recorded with the
    /// invite's pins once its keys matched them ([`ServerTrust::Invite`]).
    ///
    /// # Errors
    ///
    /// [`ServerProblem::KeyMismatch`] (and `Notice::RelayKeyMismatch`) if its keys do not match
    /// the pins; [`ClientError::Server`]; a store failure.
    pub(crate) async fn invited_server(
        &self,
        server: &ServerId,
        pins: InvitePins,
    ) -> Result<Arc<Link>, ClientError> {
        if let Ok(link) = self.servers.link(server).await {
            return Ok(link);
        }
        let looked = match self.look(server, Some(&pins)).await {
            Err(ClientError::Server(ServerProblem::KeyMismatch)) => {
                self.servers.raise_key_mismatch();
                return Err(ServerProblem::KeyMismatch.into());
            }
            other => other?,
        };
        let link = self.servers.add(looked.record).await?;
        link.learn_params(looked.params);
        Ok(link)
    }

    /// Steps 1 and 2 of the module docs.
    async fn look(
        &self,
        server: &ServerId,
        invite: Option<&InvitePins>,
    ) -> Result<Looked, ClientError> {
        let outside = self.servers.outside();
        let trust = self.servers.trust();
        let direct = network::direct(server, trust, outside)?;
        let advertised = params_direct(direct.as_ref())
            .await
            .map_err(|_unreachable| ServerProblem::Unreachable)?;
        let relays: Vec<String> = advertised
            .ohttp_relays
            .iter()
            .map(|relay| relay.as_str().to_owned())
            .collect();
        if relays.is_empty() {
            return Err(ServerProblem::NoRelay.into());
        }
        let pin = invite.map_or(KeyPin::FirstUse, |pins| {
            KeyPin::Pinned(pins.gateway.clone())
        });
        let route = network::Route {
            server,
            relays: &relays,
            pin,
        };
        let transport = network::router(route, trust, outside)?;
        let engine = network::engine(
            server,
            Arc::clone(&transport),
            self.servers.backend_database(),
            outside,
        )?;
        engine
            .check_relay(WakeBudget::UNLIMITED)
            .await
            .map_err(|error| problem(&error))?;
        let gateway_key = transport
            .gateway_key_hash()
            .ok_or(ServerProblem::RelayUnreachable)?;
        let relayed = engine
            .params(WakeBudget::UNLIMITED)
            .await
            .map_err(|error| problem(&error))?;
        if relay_set(&relayed.ohttp_relays) != relay_set(&advertised.ohttp_relays) {
            return Err(ServerProblem::RelaysDiffer.into());
        }
        let build = self.servers.build_push_relay();
        let push_relay =
            push_relay_for(&engine, &relayed.push_relays, invite, build.as_ref()).await?;
        let record = ServerRecord {
            server: server.clone(),
            ohttp_relays: relays.clone(),
            push_relay: push_relay
                .as_ref()
                .map(|chosen| chosen.info.url.as_str().to_owned()),
            gateway_pins: match invite {
                Some(pins) => pins.gateway.hashes().to_vec(),
                None => vec![gateway_key],
            },
            push_relay_pins: match (&push_relay, invite) {
                (Some(chosen), _) if chosen.is_build => Vec::new(),
                (Some(_), Some(pins)) => pins
                    .push_relay
                    .as_ref()
                    .map(|pins| pins.hashes().to_vec())
                    .unwrap_or_default(),
                (Some(chosen), None) => vec![chosen.key],
                (None, _) => Vec::new(),
            },
            trust: if invite.is_some() {
                ServerTrust::Invite
            } else {
                ServerTrust::Confirmed
            },
        };
        let probe = ServerProbe {
            server: server.clone(),
            operator: Operator::new(&relayed.server_operator).ok(),
            relay: host_of(relays.first().map_or("", String::as_str))?,
            push_relay: push_relay
                .as_ref()
                .map(|chosen| {
                    Ok::<_, ClientError>(ProbedPushRelay {
                        host: host_of(chosen.info.url.as_str())?,
                        operator: Operator::new(&chosen.info.operator).ok(),
                        key: chosen.key,
                    })
                })
                .transpose()?,
            gateway_key,
            known: false,
        };
        Ok(Looked {
            record,
            params: relayed,
            probe,
        })
    }
}

/// The push relay to use from `listed`, with its key checked against its pins: the build's for
/// the build's own relay, the invite's otherwise; while a server entered by address is only
/// being looked at, the key is read for the user to trust. `None` if no listed relay answered.
async fn push_relay_for(
    engine: &pingers_sync::SyncEngine,
    listed: &[PushRelayInfo],
    invite: Option<&InvitePins>,
    build: Option<&super::push::PushRelaySetup>,
) -> Result<Option<Chosen>, ClientError> {
    let Some(chosen) = choose_push_relay(listed, invite, build.map(|setup| setup.origin.as_str()))
    else {
        return Ok(None);
    };
    let is_build = build.is_some_and(|setup| same_origin(&setup.origin, chosen.url.as_str()));
    let pins = if is_build {
        build.and_then(|setup| setup.pins.clone())
    } else {
        invite.and_then(|pins| pins.push_relay.clone())
    };
    match engine
        .push_relay_key(chosen.relay_id, pins.as_ref(), WakeBudget::UNLIMITED)
        .await
    {
        Ok(verified) => Ok(Some(Chosen {
            info: chosen.clone(),
            key: pingers_crypto::key_config_hash(&verified.config.key_config.to_bytes()),
            is_build,
        })),
        Err(SyncError::PushRelayKeyMismatch) => Err(ServerProblem::KeyMismatch.into()),
        // The relay did not answer: the server is used without wake-up pushes until a later
        // look.
        Err(_) => Ok(None),
    }
}

/// The push relay picked, with its key's hash.
struct Chosen {
    info: PushRelayInfo,
    key: KeyConfigHash,
    is_build: bool,
}

/// The order of the module docs.
fn choose_push_relay<'listed>(
    listed: &'listed [PushRelayInfo],
    invite: Option<&InvitePins>,
    build_origin: Option<&str>,
) -> Option<&'listed PushRelayInfo> {
    let pinned = invite
        .and_then(|pins| pins.push_relay.as_ref())
        .and_then(|pins| {
            listed
                .iter()
                .find(|relay| pins.contains(&relay.key_config_hash))
        });
    pinned
        .or_else(|| {
            build_origin.and_then(|origin| {
                listed
                    .iter()
                    .find(|relay| same_origin(relay.url.as_str(), origin))
            })
        })
        .or_else(|| listed.first())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::{RelayId, RelayUrl};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn an_address_is_a_host_or_an_https_origin() -> TestResult {
        for (typed, origin) in [
            ("pingers.example", "https://pingers.example"),
            (" Pingers.Example/ ", "https://pingers.example"),
            (
                "https://pingers.example:8443",
                "https://pingers.example:8443",
            ),
        ] {
            assert_eq!(server_address(typed)?.as_str(), origin, "{typed}");
        }
        for typed in [
            "",
            "http://pingers.example",
            "pingers.example/path",
            "a b",
            "https://",
        ] {
            assert!(
                matches!(
                    server_address(typed),
                    Err(ClientError::Server(ServerProblem::InvalidAddress))
                ),
                "{typed}"
            );
        }
        Ok(())
    }

    fn relay(id: u16, url: &str, key: u8) -> Result<PushRelayInfo, Box<dyn std::error::Error>> {
        Ok(PushRelayInfo {
            relay_id: RelayId::new(id),
            url: RelayUrl::new(url.to_owned())?,
            key_config_hash: KeyConfigHash::from_bytes([key; 32]),
            operator: "op".to_owned(),
        })
    }

    #[test]
    fn each_failure_of_a_look_names_its_problem() {
        use pingers_sync::Route;
        let named = |error: SyncError| problem(&error);
        assert!(matches!(
            named(SyncError::NoRelay {
                route: Route::Params
            }),
            ClientError::Server(ServerProblem::RelayUnreachable)
        ));
        for mismatch in [SyncError::RelayKeyMismatch, SyncError::PushRelayKeyMismatch] {
            assert!(matches!(
                named(mismatch),
                ClientError::Server(ServerProblem::KeyMismatch)
            ));
        }
        assert!(matches!(
            named(SyncError::Status {
                route: Route::Params,
                status: 500
            }),
            ClientError::Server(ServerProblem::Unreachable)
        ));
        assert!(matches!(
            named(SyncError::Store(pingers_store::StoreError::UnknownServer)),
            ClientError::Internal
        ));
    }

    #[test]
    fn relay_lists_compare_by_origin_whatever_their_order() -> TestResult {
        let url = |text: &str| RelayUrl::new(text.to_owned());
        let direct = [url("https://a.example/ohttp")?, url("https://b.example")?];
        let relayed = [url("https://b.example/")?, url("https://a.example")?];
        assert_eq!(relay_set(&direct), relay_set(&relayed));
        assert_eq!(
            relay_set(&direct),
            ["https://a.example", "https://b.example"]
        );
        let other = [url("https://a.example")?, url("https://evil.example")?];
        assert_ne!(relay_set(&direct), relay_set(&other));
        Ok(())
    }

    #[test]
    fn the_push_relay_is_the_pinned_one_then_the_builds_then_the_first() -> TestResult {
        let listed = [
            relay(1, "https://first.example", 1)?,
            relay(2, "https://publisher.example", 2)?,
            relay(3, "https://pinned.example", 3)?,
        ];
        let invite = InvitePins {
            gateway: KeyPins::from(KeyConfigHash::from_bytes([9; 32])),
            push_relay: Some(KeyPins::from(KeyConfigHash::from_bytes([3; 32]))),
        };
        let id = |chosen: Option<&PushRelayInfo>| chosen.map(|relay| relay.relay_id);
        assert_eq!(
            id(choose_push_relay(
                &listed,
                Some(&invite),
                Some("https://publisher.example")
            )),
            Some(RelayId::new(3))
        );
        assert_eq!(
            id(choose_push_relay(
                &listed,
                None,
                Some("https://publisher.example/")
            )),
            Some(RelayId::new(2))
        );
        assert_eq!(
            id(choose_push_relay(&listed, None, None)),
            Some(RelayId::new(1))
        );
        assert_eq!(id(choose_push_relay(&[], None, None)), None);
        Ok(())
    }
}
