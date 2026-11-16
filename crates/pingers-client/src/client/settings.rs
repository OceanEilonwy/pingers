// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s settings: pingers-wake reads the gate's inputs, pingers-sync owns push
//! registration.

use super::Client;
use crate::ClientError;
use crate::ports::{SettingChange, SetupCheck, SetupOutcome};
use crate::snapshot::{RelayStatus, WakePushes};
use pingers_proto::{SecretText, SetupPayload};

impl Client {
    /// Changes a device-wide privacy setting.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn change_setting(&self, change: SettingChange) -> Result<(), ClientError> {
        self.published(self.crates.wake.change_setting(change).await)
            .await
    }

    /// Turns wake-up pushes on or off ("No wake-up pushes").
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] until pingers-sync merges.
    pub async fn set_wake_pushes(&self, setting: WakePushes) -> Result<(), ClientError> {
        let tokens = self.platform.push_tokens();
        self.published(self.crates.sync.set_wake_pushes(setting, tokens).await)
            .await
    }

    /// The shell's push registration changed; re-seal it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] until pingers-sync merges.
    pub async fn push_tokens_changed(&self) -> Result<(), ClientError> {
        let tokens = self.platform.push_tokens();
        self.published(self.crates.sync.push_tokens_changed(tokens).await)
            .await
    }

    /// Checks whether this device reaches its server through the privacy relay ("What your
    /// server sees"); the snapshot's relay status then shows the answer.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] until pingers-sync merges.
    pub async fn check_reachability(&self) -> Result<RelayStatus, ClientError> {
        self.published(self.crates.sync.check_reachability().await)
            .await
    }

    /// Registers this device's account with a code the user typed ("This server needs a
    /// code"), before a new Party is created on the server.
    ///
    /// # Errors
    ///
    /// [`ClientError::RegistrationRefused`] if the server did not accept it;
    /// [`ClientError::Unavailable`] without an instance.
    pub async fn register_with_code(&self, code: SecretText) -> Result<(), ClientError> {
        self.published(self.crates.sync.register_with_code(code).await)
            .await
    }

    /// What a first-phone setup link is to this device, for its confirm screen; sends nothing.
    ///
    /// # Errors
    ///
    /// [`ClientError::Store`] or [`ClientError::Records`]; [`ClientError::Unavailable`] without
    /// an instance.
    pub async fn check_setup_link(&self, link: &SetupPayload) -> Result<SetupCheck, ClientError> {
        self.crates.sync.check_setup_link(link).await
    }

    /// Uses a first-phone setup link (its confirm screen's "Set up"): makes this device's
    /// account with its code, once its server and keys check out.
    ///
    /// # Errors
    ///
    /// A storage failure; [`ClientError::Unavailable`] without an instance. The network's
    /// answers are outcomes, not errors.
    pub async fn use_setup_link(&self, link: SetupPayload) -> Result<SetupOutcome, ClientError> {
        self.published(self.crates.sync.use_setup_link(link).await)
            .await
    }
}
