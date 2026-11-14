// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s settings: pingers-wake reads the gate's inputs, pingers-sync owns push
//! registration.

use super::Client;
use crate::ClientError;
use crate::ports::SettingChange;
use crate::snapshot::{RelayStatus, WakePushes};
use pingers_proto::SecretText;

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
}
