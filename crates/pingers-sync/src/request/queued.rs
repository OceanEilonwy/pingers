// SPDX-License-Identifier: AGPL-3.0-only
//! A request as pingers-store's queue keeps it across a restart: its route's code, then its
//! Binary HTTP encoding (pingers-ohttp) without a credential, which is attached when it is
//! finally sent. The group, which only commits and group messages carry, is kept beside it
//! in the store, so ending that Meetup deletes the request.

use pingers_ohttp::BinaryRequest;
use pingers_proto::GroupId;
use pingers_store::RequestBytes;
use zeroize::{Zeroize as _, Zeroizing};

use super::{ApiRequest, Route};
use crate::SyncError;

/// Every route, at the index that is its code. Append only: queued requests outlive an app
/// update, so a code once used never changes meaning.
const ROUTE_CODES: [Route; 26] = [
    Route::Params,
    Route::CreateAccount,
    Route::Auth,
    Route::DeleteAccount,
    Route::RlnLeaf,
    Route::RlnLeaves,
    Route::PrivacyPassIssue,
    Route::PrivacyPassKeys,
    Route::BoardSync,
    Route::BoardPage,
    Route::BoardRevokeLease,
    Route::CreateMailbox,
    Route::PostToMailbox,
    Route::ReadMailbox,
    Route::DeleteMailbox,
    Route::CreateGroup,
    Route::Commit,
    Route::PutSubscription,
    Route::DeleteSubscription,
    Route::PostGroupMessage,
    Route::ReadGroupMessages,
    Route::GroupState,
    Route::DeleteGroup,
    Route::Live,
    Route::OhttpConfig,
    Route::PushRelayConfig,
];

fn route_code(route: Route) -> Result<u8, SyncError> {
    ROUTE_CODES
        .iter()
        .position(|known| *known == route)
        .and_then(|index| u8::try_from(index).ok())
        .ok_or(SyncError::QueuedRequest)
}

/// The builders' methods, so a decoded request keeps a `&'static str` like a built one.
fn known_method(method: &str) -> Result<&'static str, SyncError> {
    match method {
        "GET" => Ok("GET"),
        "POST" => Ok("POST"),
        "PUT" => Ok("PUT"),
        "DELETE" => Ok("DELETE"),
        _ => Err(SyncError::QueuedRequest),
    }
}

impl ApiRequest {
    /// The request encoded for the store's queue.
    ///
    /// # Errors
    ///
    /// [`SyncError::QueuedRequest`] if it does not encode.
    pub(crate) fn to_queued(&self) -> Result<RequestBytes, SyncError> {
        let code = route_code(self.route)?;
        let mut binary = self.to_binary(None);
        let encoded = binary.encode().map(Zeroizing::new);
        // The copy holds the same capabilities (signatures, revoke keys) as the request.
        for header in &mut binary.headers {
            header.value.zeroize();
        }
        binary.body.zeroize();
        let encoded = encoded.map_err(|_| SyncError::QueuedRequest)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(encoded.len().saturating_add(1)));
        bytes.push(code);
        bytes.extend_from_slice(&encoded);
        Ok(RequestBytes::from_bytes(bytes))
    }

    /// A request read back from the store's queue.
    ///
    /// # Errors
    ///
    /// [`SyncError::QueuedRequest`] if the bytes are not a queued request.
    pub(crate) fn from_queued(
        group: Option<GroupId>,
        bytes: &RequestBytes,
    ) -> Result<Self, SyncError> {
        let (code, encoded) = bytes
            .expose_bytes()
            .split_first()
            .ok_or(SyncError::QueuedRequest)?;
        let route = ROUTE_CODES
            .get(usize::from(*code))
            .copied()
            .ok_or(SyncError::QueuedRequest)?;
        let mut binary = BinaryRequest::decode(encoded).map_err(|_| SyncError::QueuedRequest)?;
        Ok(Self {
            route,
            group,
            method: known_method(&binary.method)?,
            path: core::mem::take(&mut binary.path),
            headers: core::mem::take(&mut binary.headers),
            body: core::mem::take(&mut binary.body),
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::LogAuthKeyPair;
    use pingers_ohttp::Header;
    use pingers_proto::{AppMessage, Delivery, Epoch, LeaseId, MlsMessageBytes, SecretBytes};
    use pingers_time::UnixSeconds;

    use super::*;
    use crate::GroupSigner;

    /// Encodes and decodes `request`, checking that nothing about it changed.
    fn round_trip(request: &ApiRequest) -> Result<ApiRequest, SyncError> {
        let back = ApiRequest::from_queued(request.group(), &request.to_queued()?)?;
        assert_eq!(back.route(), request.route());
        assert_eq!(back.method(), request.method());
        assert_eq!(back.to_binary(None), request.to_binary(None));
        Ok(back)
    }

    fn key() -> LogAuthKeyPair {
        LogAuthKeyPair::from_bytes(Zeroizing::new([7; 32]))
    }

    #[test]
    fn every_route_has_its_own_code() -> Result<(), SyncError> {
        for (index, route) in ROUTE_CODES.iter().enumerate() {
            assert_eq!(usize::from(route_code(*route)?), index, "{route:?}");
        }
        Ok(())
    }

    #[test]
    fn requests_of_every_method_come_back_unchanged() -> Result<(), SyncError> {
        let group = GroupId::from_bytes([4; 32]);
        let key = key();
        let signer = GroupSigner {
            key: &key,
            now: UnixSeconds::new(1_000),
        };
        let message = AppMessage {
            epoch: Epoch::new(3),
            body: MlsMessageBytes::from_bytes(vec![5; 64]),
            delivery: Delivery::Stored,
            wake: None,
        };
        let posted = round_trip(&ApiRequest::post_group_message(&group, &message, &signer)?)?;
        assert_eq!(posted.group(), Some(group));
        round_trip(&ApiRequest::params())?;
        round_trip(&ApiRequest::revoke_lease(
            &LeaseId::from_bytes([1; 16]),
            &SecretBytes::new([2; 32]),
        ))?;
        // Built by hand: a real subscription needs a sealed push, which adds nothing here.
        round_trip(
            &ApiRequest::new(
                Route::PutSubscription,
                "PUT",
                "/v1/groups/04/subs/06".to_owned(),
            )
            .with_header("content-type", b"application/cbor".to_vec()),
        )?;
        Ok(())
    }

    #[test]
    fn bytes_that_are_not_a_queued_request_are_refused() -> Result<(), SyncError> {
        let empty = RequestBytes::from_bytes(Zeroizing::new(Vec::new()));
        assert!(matches!(
            ApiRequest::from_queued(None, &empty),
            Err(SyncError::QueuedRequest)
        ));
        let params = ApiRequest::params().to_queued()?;
        let mut unknown_route = params.expose_bytes().to_vec();
        if let Some(code) = unknown_route.first_mut() {
            *code = 26;
        }
        assert!(matches!(
            ApiRequest::from_queued(
                None,
                &RequestBytes::from_bytes(Zeroizing::new(unknown_route))
            ),
            Err(SyncError::QueuedRequest)
        ));
        let patch = BinaryRequest {
            method: "PATCH".to_owned(),
            path: "/v1/params".to_owned(),
            headers: vec![Header {
                name: "x".to_owned(),
                value: Vec::new(),
            }],
            body: Vec::new(),
        };
        let mut odd_method = vec![0];
        odd_method.extend(patch.encode().map_err(|_| SyncError::QueuedRequest)?);
        assert!(matches!(
            ApiRequest::from_queued(None, &RequestBytes::from_bytes(Zeroizing::new(odd_method))),
            Err(SyncError::QueuedRequest)
        ));
        Ok(())
    }
}
