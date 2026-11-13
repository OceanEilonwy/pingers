// SPDX-License-Identifier: AGPL-3.0-only
//! The outbound call to a push relay: `POST /v1/push` (design D, push relay API).

use core::future::Future;
use std::collections::HashMap;

use pingers_proto::{PushRequest, RelayId, RelayRef, RelayUrl};

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

/// Everything needed to call one relay (design C.1 `push_relay`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayTarget {
    /// The relay's base URL.
    pub url: RelayUrl,
    /// This server's credential at the relay.
    pub credential: RelayCredential,
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

/// The `Content-Type` of a push request: the body is
/// [`pingers_proto::PushRequest`] in CBOR (`WireEncode::encode`), as the relay expects.
pub const PUSH_CONTENT_TYPE: &str = "application/cbor";

/// What the relay answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelayAnswer {
    /// 202: the push was sent.
    Accepted,
    /// 410: the token was rejected by the push service, has expired, outlives the relay's
    /// `max_ttl`, or its key is retired; delete the blob.
    Gone,
    /// 422: the blob does not open under this bind and key id.
    Rejected,
    /// 429: a rate limit at the relay.
    RateLimited,
    /// Any other status (400, 401, 500, 501 platform not served, 502, 504): keep the blob.
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
/// `Content-Type:` [`PUSH_CONTENT_TYPE`], body `request.encode()` (CBOR), with a timeout from
/// its `Timer`); tests use a fake.
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
        })
    }

    #[test]
    fn the_body_is_the_relay_s_cbor_push_request() -> Result<(), pingers_proto::ProtoError> {
        use pingers_proto::{
            PushBind, PushHandle, PushRelayKeyId, SealedPushBlob, Urgency, WireDecode as _,
            WireEncode as _,
        };
        let request = PushRequest {
            key_id: PushRelayKeyId::new(3),
            blob: SealedPushBlob::from_bytes([0xab; 320]),
            bind: PushBind::from_bytes([0x01; 32]),
            urgency: Urgency::Quiet,
            handle: PushHandle::from_bytes([0xfe; 8]),
        };
        // What the relay decodes is exactly what was sent.
        assert_eq!(PushRequest::decode(&request.encode()?)?, request);
        assert_eq!(PUSH_CONTENT_TYPE, "application/cbor");
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
