// SPDX-License-Identifier: AGPL-3.0-only
//! Servers that are not built in: looking at one by address for New party, and trusting its
//! keys. The registration code goes with `create_party`. The device-wide settings are
//! unaffected; each Party's server shows in its settings (`party_views.rs`).

use pingers_client::{ServerProbe, ServerProblem};
use pingers_proto::ServerId;
use pingers_words::Language;
use pingers_words::errors::{ServerRefusal, server_refusal};

use crate::CoreError;
use crate::core::Core;

/// Why a server could not be used, for the app to pick its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ServerIssue {
    /// What was typed is not a server address.
    InvalidAddress,
    /// The server did not answer.
    Unreachable,
    /// The server advertises no privacy relay; the app never uses such a server.
    NoRelay,
    /// None of its privacy relays answered.
    RelayUnreachable,
    /// It named different relays directly and through its relay.
    RelaysDiffer,
    /// Its keys are not the ones being trusted (the relay key warning).
    KeyMismatch,
    /// `trust_server` without `probe_server` first.
    NotLookedAt,
}

impl ServerIssue {
    const fn refusal(self) -> ServerRefusal {
        match self {
            Self::InvalidAddress => ServerRefusal::InvalidAddress,
            Self::Unreachable => ServerRefusal::Unreachable,
            Self::NoRelay => ServerRefusal::NoRelay,
            Self::RelayUnreachable => ServerRefusal::RelayUnreachable,
            Self::RelaysDiffer => ServerRefusal::RelaysDiffer,
            Self::KeyMismatch => ServerRefusal::KeyMismatch,
            Self::NotLookedAt => ServerRefusal::NotLookedAt,
        }
    }

    /// The issue a client error stands for, if it is one of these.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "ServerProblem is non-exhaustive; a new problem reads as unreachable"
    )]
    pub(crate) fn of(error: &pingers_client::ClientError) -> Option<Self> {
        use pingers_client::ClientError;
        Some(match error {
            // A Party on a server whose keys this device has not trusted: look it up first.
            ClientError::Unavailable(pingers_client::Unavailable::UnknownServer) => {
                Self::NotLookedAt
            }
            ClientError::Server(problem) => match problem {
                ServerProblem::InvalidAddress => Self::InvalidAddress,
                ServerProblem::NoRelay => Self::NoRelay,
                ServerProblem::RelayUnreachable => Self::RelayUnreachable,
                ServerProblem::RelaysDiffer => Self::RelaysDiffer,
                ServerProblem::KeyMismatch => Self::KeyMismatch,
                ServerProblem::NotProbed => Self::NotLookedAt,
                _ => Self::Unreachable,
            },
            _ => return None,
        })
    }

    /// The error the app gets: the issue and what to tell the user.
    pub(crate) fn error(self) -> CoreError {
        CoreError::Server {
            issue: self,
            reason: server_refusal(Language::English, self.refusal()),
        }
    }
}

/// A key's hash as the app shows it: lower-case hex in groups of four, for comparing by eye.
fn fingerprint(hash: &pingers_proto::KeyConfigHash) -> String {
    let hex = hex::encode(hash.as_bytes());
    hex.as_bytes()
        .chunks(4)
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What looking at a server found, for "Trust this server's keys".
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ServerLook {
    /// The server's origin, to pass back to `trust_server` and to `NewPartyDraft::server`.
    pub server: String,
    /// Who it declares runs it, if anyone.
    pub operator: Option<String>,
    /// Its privacy relay's host.
    pub relay: String,
    /// Its push relay's host, if one answered.
    pub push_relay: Option<String>,
    /// Who declares to run the push relay.
    pub push_relay_operator: Option<String>,
    /// The gateway key fingerprint that trusting pins.
    pub gateway_key: String,
    /// The push relay key fingerprint that trusting pins.
    pub push_relay_key: Option<String>,
    /// This device already trusts the server: nothing to confirm.
    pub known: bool,
}

impl From<ServerProbe> for ServerLook {
    fn from(probe: ServerProbe) -> Self {
        Self {
            server: probe.server.as_str().to_owned(),
            operator: probe.operator.map(|operator| operator.as_str().to_owned()),
            relay: probe.relay.as_str().to_owned(),
            push_relay: probe
                .push_relay
                .as_ref()
                .map(|relay| relay.host.as_str().to_owned()),
            push_relay_operator: probe
                .push_relay
                .as_ref()
                .and_then(|relay| relay.operator.as_ref())
                .map(|operator| operator.as_str().to_owned()),
            gateway_key: fingerprint(&probe.gateway_key),
            push_relay_key: probe
                .push_relay
                .as_ref()
                .map(|relay| fingerprint(&relay.key)),
            known: probe.known,
        }
    }
}

pub(crate) fn server_id(origin: String) -> Result<ServerId, CoreError> {
    ServerId::new(origin).map_err(|_invalid| ServerIssue::InvalidAddress.error())
}

#[uniffi::export(async_runtime = "tokio")]
impl Core {
    /// Looks at the server at `address` (`pingers.example` or `https://pingers.example`) for
    /// New party. Nothing is recorded.
    ///
    /// # Errors
    ///
    /// [`CoreError::Server`] with the [`ServerIssue`].
    pub async fn probe_server(&self, address: String) -> Result<ServerLook, CoreError> {
        Ok(self.client().probe_server(address).await?.into())
    }

    /// Trusts the keys of a server looked at with `probe_server` ("Trust this server's keys"):
    /// the one place they are first recorded.
    ///
    /// # Errors
    ///
    /// [`CoreError::Server`] (`NotLookedAt`).
    pub async fn trust_server(&self, server: String) -> Result<(), CoreError> {
        Ok(self.client().trust_server(server_id(server)?).await?)
    }
}

/// How this device came to trust a Party's server keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum KeysTrusted {
    /// The app's built-in server: pinned in the app.
    BuiltIn,
    /// From the invite link, as the inviter trusted them.
    FromInvite,
    /// The user trusted them when creating the Party.
    Confirmed,
}

impl From<pingers_screens::snapshot::KeyTrust> for KeysTrusted {
    fn from(trust: pingers_screens::snapshot::KeyTrust) -> Self {
        use pingers_screens::snapshot::KeyTrust;
        match trust {
            KeyTrust::BuiltIn => Self::BuiltIn,
            KeyTrust::FromInvite => Self::FromInvite,
            KeyTrust::Confirmed => Self::Confirmed,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn a_fingerprint_is_hex_in_groups_of_four() {
        let shown = fingerprint(&pingers_proto::KeyConfigHash::from_bytes([0xab; 32]));
        assert!(shown.starts_with("abab abab "));
        assert_eq!(shown.split(' ').count(), 16);
    }

    #[test]
    fn every_server_problem_has_its_own_issue_and_words() {
        use pingers_client::ClientError;
        let cases = [
            (ServerProblem::InvalidAddress, ServerIssue::InvalidAddress),
            (ServerProblem::Unreachable, ServerIssue::Unreachable),
            (ServerProblem::NoRelay, ServerIssue::NoRelay),
            (
                ServerProblem::RelayUnreachable,
                ServerIssue::RelayUnreachable,
            ),
            (ServerProblem::RelaysDiffer, ServerIssue::RelaysDiffer),
            (ServerProblem::KeyMismatch, ServerIssue::KeyMismatch),
            (ServerProblem::NotProbed, ServerIssue::NotLookedAt),
        ];
        let mut words = Vec::new();
        for (problem, issue) in cases {
            assert_eq!(ServerIssue::of(&ClientError::Server(problem)), Some(issue));
            let error = issue.error();
            assert!(matches!(&error, CoreError::Server { issue: shown, .. } if *shown == issue));
            words.push(error.to_string());
        }
        words.sort();
        words.dedup();
        assert_eq!(words.len(), 7, "each issue is told in its own words");
    }
}
