// SPDX-License-Identifier: AGPL-3.0-only
//! Servers that are not built in: looking at one by address, trusting its keys, and giving a
//! server the registration code it asks for. The device-wide settings are unaffected; each
//! Party's server shows in its settings (`PartySettingsView`).

use pingers_client::{ServerProbe, ServerProblem};
use pingers_proto::{SecretText, ServerId};
use pingers_words::Language;
use pingers_words::errors::{ServerRefusal, server_refusal};

use crate::core::Core;
use crate::{CoreError, Snapshot};

/// Why a server could not be used, for the app to pick its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ServerIssue {
    /// The server needs a registration code: show "This server needs a code".
    CodeRequired,
    /// The server refused the code: show "Registration code refused" (try again as often as
    /// you like).
    CodeRefused,
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
            Self::CodeRequired => ServerRefusal::CodeRequired,
            Self::CodeRefused => ServerRefusal::CodeRefused,
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
            ClientError::RegistrationCodeRequired => Self::CodeRequired,
            ClientError::RegistrationCodeRefused => Self::CodeRefused,
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

    /// Gives `server` the registration code the user typed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Server`] with `CodeRefused` if the server refused it.
    pub async fn submit_registration_code(
        &self,
        server: String,
        code: String,
    ) -> Result<(), CoreError> {
        let code = code.trim().to_owned();
        if code.is_empty() {
            return Err(ServerIssue::CodeRefused.error());
        }
        Ok(self
            .client()
            .submit_registration_code(server_id(server)?, SecretText::new(code))
            .await?)
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

/// One service on a Party's settings page.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ServiceRowView {
    /// Where it is.
    pub host: String,
    /// Who declares to run it.
    pub operator: String,
}

/// A Party's settings page: its own server, relays and operators. Device-wide settings are
/// elsewhere.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PartySettingsView {
    /// The server's origin.
    pub origin: String,
    /// The server.
    pub server: ServiceRowView,
    /// The privacy relay.
    pub relay: ServiceRowView,
    /// Whether the server answered through it lately, and that in words.
    pub relay_check: crate::views::RelayCheck,
    /// The push relay: `None` host for the official one.
    pub push_relay_host: Option<String>,
    /// Who declares to run the push relay.
    pub push_relay_operator: String,
    /// "No wake-up pushes" is on.
    pub wake_pushes_off: bool,
    /// How the server's keys were trusted.
    pub keys: KeysTrusted,
    /// The quantum-resistant suite.
    pub post_quantum: bool,
    /// The server and push relay declare the same operator.
    pub same_operator: bool,
}

impl From<pingers_screens::party_settings::PartySettingsView> for PartySettingsView {
    fn from(view: pingers_screens::party_settings::PartySettingsView) -> Self {
        use pingers_screens::snapshot::{KeyTrust, PostQuantum, PushRelayKind, WakePushes};
        Self {
            origin: view.origin.as_str().to_owned(),
            server: ServiceRowView {
                host: view.server.host.as_str().to_owned(),
                operator: view.server.operator.as_str().to_owned(),
            },
            relay: ServiceRowView {
                host: view.relay.host.as_str().to_owned(),
                operator: view.relay.operator.as_str().to_owned(),
            },
            relay_check: view.relay_status.into(),
            push_relay_host: match view.push_relay.kind {
                PushRelayKind::Official => None,
                PushRelayKind::Custom(host) => Some(host.as_str().to_owned()),
            },
            push_relay_operator: view.push_relay.operator.as_str().to_owned(),
            wake_pushes_off: view.push_relay.wake_pushes == WakePushes::NoWakePushes,
            keys: match view.keys {
                KeyTrust::BuiltIn => KeysTrusted::BuiltIn,
                KeyTrust::FromInvite => KeysTrusted::FromInvite,
                KeyTrust::Confirmed => KeysTrusted::Confirmed,
            },
            post_quantum: view.post_quantum == PostQuantum::On,
            same_operator: view.same_operator,
        }
    }
}

#[uniffi::export]
impl Snapshot {
    /// A Party's settings page.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn party_settings(&self, id: &crate::PartyId) -> Result<PartySettingsView, CoreError> {
        Ok(
            pingers_screens::party_settings::party_settings(
                self.client_snapshot(),
                id.try_into()?,
            )?
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn a_fingerprint_is_hex_in_groups_of_four() {
        let shown = fingerprint(&pingers_proto::KeyConfigHash::from_bytes([0xab; 32]));
        assert!(shown.starts_with("abab abab "));
        assert_eq!(shown.split(' ').count(), 16);
    }

    #[test]
    fn a_party_s_settings_carry_its_own_server() -> Result<(), CoreError> {
        let mut snapshot = pingers_screens::fixtures::snapshot()
            .map_err(|_fixture| CoreError::invalid("fixture"))?;
        let id = snapshot
            .parties
            .first()
            .map(|party| party.id)
            .ok_or_else(|| CoreError::invalid("fixture"))?;
        if let Some(party) = snapshot.parties.first_mut() {
            party.config.keys = pingers_screens::snapshot::KeyTrust::FromInvite;
            party.config.push_relay.kind = pingers_screens::snapshot::PushRelayKind::Custom(
                pingers_screens::common::Host::new("push.other.example")?,
            );
        }
        let snapshot = Snapshot::new(std::sync::Arc::new(snapshot));
        let view = snapshot.party_settings(&crate::PartyId::from(id))?;
        assert_eq!(view.origin, "https://pingers.flat7.org");
        assert_eq!(view.server.host, "pingers.flat7.org");
        assert_eq!(view.server.operator, "Flat 7");
        assert_eq!(view.relay.host, "relay.quietpath.net");
        assert_eq!(view.keys, KeysTrusted::FromInvite);
        assert_eq!(view.push_relay_host.as_deref(), Some("push.other.example"));
        assert!(!view.post_quantum);
        Ok(())
    }

    #[test]
    fn a_missing_and_a_refused_code_are_different_issues() {
        use pingers_client::ClientError;
        assert_eq!(
            ServerIssue::of(&ClientError::RegistrationCodeRequired),
            Some(ServerIssue::CodeRequired)
        );
        assert_eq!(
            ServerIssue::of(&ClientError::RegistrationCodeRefused),
            Some(ServerIssue::CodeRefused)
        );
        assert_eq!(ServerIssue::of(&ClientError::Internal), None);
    }
}
