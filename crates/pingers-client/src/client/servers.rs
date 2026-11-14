// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s servers: looking at a server that is not built in, trusting its keys, and
//! giving a server the registration code it asks for (pingers-sync, one engine per server).

use pingers_proto::{SecretText, ServerId};

use super::Client;
use crate::ClientError;
use crate::ports::ServerProbe;

impl Client {
    /// Looks at the server at `address` (a host such as `pingers.example`, or an `https://`
    /// origin) for New party: who runs it, its privacy relay and push relay, and the keys it
    /// serves. Its parameters are fetched direct (the route its registration takes anyway),
    /// everything else through its own relay. Nothing is recorded until
    /// [`Client::trust_server`].
    ///
    /// # Errors
    ///
    /// [`ClientError::Server`] with the [`ServerProblem`](crate::ServerProblem): not an
    /// address, not reachable, no relay advertised, relay not reachable, relays that differ.
    pub async fn probe_server(&self, address: String) -> Result<ServerProbe, ClientError> {
        self.crates.sync.probe_server(address).await
    }

    /// Trusts the keys of a server looked at with [`Client::probe_server`]: the one place a
    /// server's pins are first recorded. A Party can then be created on it.
    ///
    /// # Errors
    ///
    /// [`ServerProblem::NotProbed`](crate::ServerProblem::NotProbed) if it was not looked at.
    pub async fn trust_server(&self, server: ServerId) -> Result<(), ClientError> {
        self.published(self.crates.sync.trust_server(server).await)
            .await
    }

    /// Gives `server` (`None`: the built-in one) the registration code the user typed ("This
    /// server needs a code") and registers with it.
    ///
    /// # Errors
    ///
    /// [`ClientError::RegistrationCodeRefused`] if the server refused it (nothing was set up;
    /// another code can be tried); [`ClientError::Unavailable`] for an unknown server.
    pub async fn submit_registration_code(
        &self,
        server: Option<ServerId>,
        code: SecretText,
    ) -> Result<(), ClientError> {
        self.published(
            self.crates
                .sync
                .submit_registration_code(server, code)
                .await,
        )
        .await
    }
}
