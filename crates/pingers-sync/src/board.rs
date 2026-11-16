// SPDX-License-Identifier: AGPL-3.0-only
//! The blind board's requests (design B.4.3, D) for pingers-wake, and the server's parameters.

use pingers_crypto::RevokeKey;
use pingers_proto::{
    BoardCursor, BoardPage, BoardSyncRequest, BoardSyncResponse, Lease, LeaseId, LeaseReplacement,
    Params, ReadBoard, SecretBytes,
};
use pingers_store::LastUpload;

use crate::mailbox::Delivered;
use crate::{ApiRequest, ClockStatus, Sending, SyncEngine, SyncError, WakeBudget};

/// A lease to upload, with the key that revokes it (pingers-wake's `LeaseUpload`).
pub struct NewLease {
    /// The lease.
    pub lease: Lease,
    /// The key whose hash is its revoke commitment; kept so the next upload replaces it.
    pub revoke_key: RevokeKey,
    /// Whether to read the board after the append.
    pub read: ReadBoard,
}

impl SyncEngine {
    /// Uploads a lease and, if asked, reads the first page (`Board` credential). Never
    /// queued: a lease that misses this wake looks like a dummy upload, and the next wake
    /// builds a fresh one for its own bucket.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] (422 for a lease outside the current bucket); as
    /// [`SyncEngine::send`].
    pub async fn board_sync(
        &self,
        request: &BoardSyncRequest,
        budget: WakeBudget,
    ) -> Result<Delivered<BoardSyncResponse>, SyncError> {
        let outcome = self
            .send(ApiRequest::board_sync(request)?, Sending::or_drop(budget))
            .await?;
        let delivered =
            Delivered::from_outcome(outcome, |response| response.decode::<BoardSyncResponse>())?;
        if let Delivered::Done(response) = &delivered {
            let _: ClockStatus = self.observe_server_time(response.server_time);
        }
        Ok(delivered)
    }

    /// Uploads `lease`, replacing the one this engine uploaded before (by revealing its
    /// revoke key), and keeps the new lease's id and key for the next upload. Never queued.
    ///
    /// The previous upload's id and key are kept in pingers-store's `board_upload` table, so
    /// the first upload after a restart still replaces the old lease.
    ///
    /// # Errors
    ///
    /// As [`SyncEngine::board_sync`]; [`SyncError::Store`].
    pub async fn upload_lease(
        &self,
        upload: NewLease,
        budget: WakeBudget,
    ) -> Result<Delivered<BoardSyncResponse>, SyncError> {
        let replaces = self
            .blocking(|session| Ok(session.board_upload().last()?))
            .await?
            .map(|last| LeaseReplacement {
                lease_id: last.lease,
                revoke_key: SecretBytes::new(*last.revoke_key.expose_bytes()),
            });
        let delivered = self
            .board_sync(
                &BoardSyncRequest {
                    lease: upload.lease,
                    replaces,
                    read: upload.read,
                },
                budget,
            )
            .await?;
        if let Delivered::Done(response) = &delivered {
            let last = LastUpload {
                lease: response.lease_id,
                revoke_key: upload.revoke_key,
            };
            self.blocking(move |session| Ok(session.board_upload().remember(&last)?))
                .await?;
        }
        Ok(delivered)
    }

    /// `GET /v1/params` (through the relay, no credential), recording its `server_time`
    /// against the device clock: the way to re-check the clock after
    /// [`SyncError::ClockSkew`].
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn fetch_params(&self, budget: WakeBudget) -> Result<Params, SyncError> {
        let params: Params = self
            .exchange(ApiRequest::params(), budget)
            .await?
            .success()?
            .decode()?;
        let _: ClockStatus = self.observe_server_time(params.server_time);
        self.apply_params(&params).await?;
        Ok(params)
    }

    /// The next board page after `cursor` (capability: the server-MACed cursor).
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] (410 once the cursor's ten minutes are over); as
    /// [`SyncEngine::send`].
    pub async fn board_page(
        &self,
        cursor: &BoardCursor,
        budget: WakeBudget,
    ) -> Result<BoardPage, SyncError> {
        self.exchange(ApiRequest::board_page(cursor), budget)
            .await?
            .success()?
            .decode()
    }

    /// Withdraws a lease early (exit or suppression while holding a real record). Worth
    /// queueing until the lease would have expired anyway.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn revoke_lease(
        &self,
        lease: &LeaseId,
        revoke_key: &SecretBytes<32>,
        sending: Sending,
    ) -> Result<Delivered<()>, SyncError> {
        let outcome = self
            .send(ApiRequest::revoke_lease(lease, revoke_key), sending)
            .await?;
        Delivered::from_outcome(outcome, |_| Ok(()))
    }
}
