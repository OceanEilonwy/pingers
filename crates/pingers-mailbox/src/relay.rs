// SPDX-License-Identifier: AGPL-3.0-only
//! The outbound call to a push relay: `POST /v1/push` (design D, push relay API).

use core::future::Future;
use std::collections::HashMap;

use pingers_crypto::PushBind;
use pingers_proto::{PushHandle, RelayId, RelayRef, RelayUrl, SealedPushBlob, Urgency};

/// The path of the relay's push endpoint, relative to its URL.
pub const PUSH_PATH: &str = "/v1/push";

/// The credential the relay operator issued to this server, sent as
/// `Authorization: Bearer <credential>`. `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayCredential(String);

impl RelayCredential {
    /// Wraps the credential from the server's configuration.
    #[must_use]
    pub const fn new(credential: String) -> Self {
        Self(credential)
    }

    /// The `Authorization` header value.
    #[must_use]
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl core::fmt::Debug for RelayCredential {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("RelayCredential(redacted)")
    }
}

/// Which of the relay's HPKE keys a blob is sealed to (the relay's `key_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RelayKeyId(u8);

impl RelayKeyId {
    /// Wraps the key id from the relay's published config.
    #[must_use]
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// The wire value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Everything needed to call one relay (design C.1 `push_relay`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayTarget {
    /// The relay's base URL.
    pub url: RelayUrl,
    /// This server's credential at the relay.
    pub credential: RelayCredential,
    /// The relay's current key id, which devices seal to.
    pub key_id: RelayKeyId,
}

/// Resolves the relay a sealed push names to a target this server can call.
pub trait RelayDirectory {
    /// The target for `relay`, or `None` if this server does not forward to it.
    fn resolve(&self, relay: &RelayRef) -> Option<RelayTarget>;
}

/// The relays in the server's configuration (`Params.push_relays`). Custom relays resolve to
/// nothing: the server holds no credential for them.
#[derive(Clone, Debug, Default)]
pub struct ListedRelays {
    relays: HashMap<RelayId, RelayTarget>,
}

impl ListedRelays {
    /// The configured relays by id.
    #[must_use]
    pub fn new(relays: impl IntoIterator<Item = (RelayId, RelayTarget)>) -> Self {
        Self {
            relays: relays.into_iter().collect(),
        }
    }
}

impl RelayDirectory for ListedRelays {
    fn resolve(&self, relay: &RelayRef) -> Option<RelayTarget> {
        match relay {
            RelayRef::Listed(id) => self.relays.get(id).cloned(),
            RelayRef::Custom { .. } => None,
        }
    }
}

/// The body of `POST /v1/push`: the blob as registered, never opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    /// Which relay key the blob is sealed to.
    pub key_id: RelayKeyId,
    /// The sealed token.
    pub blob: SealedPushBlob,
    /// `SHA-256("pingers/v1/push-bind" || mailbox_id)`: the relay never sees the id.
    pub bind: PushBind,
    /// Priority for class Standard.
    pub urgency: Urgency,
    /// Echoed in the push so the device knows which mailbox to read.
    pub handle: PushHandle,
}

impl PushRequest {
    /// The JSON body the relay expects:
    ///
    /// ```json
    /// {"key_id":1,"blob":"<640 hex>","bind":"<64 hex>","urgency":"alert","handle":"<16 hex>"}
    /// ```
    ///
    /// `urgency` is `"alert"` or `"quiet"`.
    #[must_use]
    pub fn to_json(&self) -> String {
        let urgency = match self.urgency {
            Urgency::Alert => "alert",
            Urgency::Quiet => "quiet",
        };
        format!(
            r#"{{"key_id":{},"blob":"{}","bind":"{}","urgency":"{}","handle":"{}"}}"#,
            self.key_id.get(),
            hex::encode(self.blob.as_bytes()),
            hex::encode(self.bind.as_bytes()),
            urgency,
            hex::encode(self.handle.as_bytes()),
        )
    }
}

/// What the relay answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelayAnswer {
    /// 202: the push was sent.
    Accepted,
    /// 410: the token was rejected by the push service or has expired; delete the blob.
    Gone,
    /// 422: the blob does not open under this bind.
    Rejected,
    /// 429: a rate limit at the relay.
    RateLimited,
    /// Any other status.
    Failed,
}

impl RelayAnswer {
    /// Maps an HTTP status from the relay.
    #[must_use]
    pub const fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Accepted,
            410 => Self::Gone,
            422 => Self::Rejected,
            429 => Self::RateLimited,
            _ => Self::Failed,
        }
    }
}

/// The relay could not be reached, or did not answer in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("push relay unreachable")]
pub struct RelayUnreachable;

/// Sends one push request to a relay. pingers-server implements it over HTTPS
/// (`POST {url}{PUSH_PATH}`, `Authorization` from [`RelayCredential::authorization`],
/// `Content-Type: application/json`, body [`PushRequest::to_json`], with a timeout from its
/// `Timer`); tests use a fake.
pub trait PushRelay {
    /// Sends `request` to `target`.
    ///
    /// # Errors
    ///
    /// [`RelayUnreachable`] if no answer arrived.
    fn push(
        &self,
        target: &RelayTarget,
        request: &PushRequest,
    ) -> impl Future<Output = Result<RelayAnswer, RelayUnreachable>> + Send;
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::KeyConfigHash;

    use super::*;

    fn target() -> Result<RelayTarget, pingers_proto::ProtoError> {
        Ok(RelayTarget {
            url: RelayUrl::new(String::from("https://push.example"))?,
            credential: RelayCredential::new(String::from("s3cret")),
            key_id: RelayKeyId::new(1),
        })
    }

    #[test]
    fn json_body_matches_the_relay_contract() -> Result<(), serde_json::Error> {
        let request = PushRequest {
            key_id: RelayKeyId::new(3),
            blob: SealedPushBlob::from_bytes([0xab; 320]),
            bind: PushBind::from_bytes([0x01; 32]),
            urgency: Urgency::Quiet,
            handle: PushHandle::from_bytes([0xfe; 8]),
        };
        let json: serde_json::Value = serde_json::from_str(&request.to_json())?;
        let expected = serde_json::json!({
            "key_id": 3,
            "blob": "ab".repeat(320),
            "bind": "01".repeat(32),
            "urgency": "quiet",
            "handle": "fe".repeat(8),
        });
        assert_eq!(json, expected);
        let alert = PushRequest {
            urgency: Urgency::Alert,
            ..request
        };
        let json: serde_json::Value = serde_json::from_str(&alert.to_json())?;
        assert_eq!(json.get("urgency"), Some(&serde_json::json!("alert")));
        Ok(())
    }

    #[test]
    fn credential_is_sent_as_a_bearer_token_and_never_printed() {
        let credential = RelayCredential::new(String::from("s3cret"));
        assert_eq!(credential.authorization(), "Bearer s3cret");
        assert_eq!(format!("{credential:?}"), "RelayCredential(redacted)");
    }

    #[test]
    fn statuses_map_to_answers() {
        assert_eq!(RelayAnswer::from_status(200), RelayAnswer::Accepted);
        assert_eq!(RelayAnswer::from_status(202), RelayAnswer::Accepted);
        assert_eq!(RelayAnswer::from_status(299), RelayAnswer::Accepted);
        assert_eq!(RelayAnswer::from_status(300), RelayAnswer::Failed);
        assert_eq!(RelayAnswer::from_status(199), RelayAnswer::Failed);
        assert_eq!(RelayAnswer::from_status(410), RelayAnswer::Gone);
        assert_eq!(RelayAnswer::from_status(422), RelayAnswer::Rejected);
        assert_eq!(RelayAnswer::from_status(429), RelayAnswer::RateLimited);
        assert_eq!(RelayAnswer::from_status(500), RelayAnswer::Failed);
    }

    #[test]
    fn only_listed_relays_resolve() -> Result<(), pingers_proto::ProtoError> {
        let relays = ListedRelays::new([(RelayId::new(1), target()?)]);
        assert_eq!(
            relays.resolve(&RelayRef::Listed(RelayId::new(1))),
            Some(target()?)
        );
        assert_eq!(relays.resolve(&RelayRef::Listed(RelayId::new(2))), None);
        let custom = RelayRef::Custom {
            url: RelayUrl::new(String::from("https://push.example"))?,
            key_config_hash: KeyConfigHash::from_bytes([0; 32]),
        };
        assert_eq!(relays.resolve(&custom), None);
        Ok(())
    }
}
