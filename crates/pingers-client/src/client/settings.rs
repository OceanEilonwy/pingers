// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s settings: pingers-wake reads the gate's inputs, pingers-sync owns push
//! registration.

use super::Client;
use crate::ClientError;
use crate::ports::SettingChange;
use crate::snapshot::{RelayStatus, WakePushes};

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
    /// A store failure; turning them on: [`ClientError::Unavailable`] with no push token or
    /// without any server, and the first server's error when no server could be sealed for.
    pub async fn set_wake_pushes(&self, setting: WakePushes) -> Result<(), ClientError> {
        let tokens = self.platform.push_tokens();
        self.published(self.crates.sync.set_wake_pushes(setting, tokens).await)
            .await
    }

    /// The shell's push registration changed; re-seal it.
    ///
    /// # Errors
    ///
    /// A store failure; [`ClientError::Unavailable`] without any server; the first server's
    /// error when no server could be sealed for.
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
    /// A store failure; a server's error other than an unreachable relay or a key mismatch
    /// (those are answers, per server, not errors).
    pub async fn check_reachability(&self) -> Result<RelayStatus, ClientError> {
        self.published(self.crates.sync.check_reachability().await)
            .await
    }
}
