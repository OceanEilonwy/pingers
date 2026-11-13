// SPDX-License-Identifier: AGPL-3.0-only
//! The blind board's requests (design B.4.3, D) for pingers-wake, and the membership tree's
//! leaf list (design B.4.8), which the main app keeps whole so it never asks for its own path.

use pingers_anon::LeafValue;
use pingers_proto::{
    BoardCursor, BoardPage, BoardSyncRequest, BoardSyncResponse, LeaseId, MembershipIndex,
    MembershipLeaves, SecretBytes,
};
use pingers_store::{TreeLeaf, TreePosition};

use crate::engine::blocking;
use crate::mailbox::Delivered;
use crate::{ApiRequest, Sending, SyncEngine, SyncError, WakeBudget};

/// What a membership refresh changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct MembershipRefresh {
    /// Leaves appended to the local tree.
    pub added: usize,
    /// Leaves the server lists as zeroed (banned or deleted devices), all set to zero.
    pub zeroed: usize,
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
        Delivered::from_outcome(outcome, |response| response.decode())
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

    /// Brings the local membership tree up to the server's (`GET /v1/rln/leaves`,
    /// anonymous and without a credential): appends new leaves page by page and zeroes the
    /// listed positions. Main app only in practice, since only it proves.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; [`SyncError::Store`]; as [`SyncEngine::send`].
    pub async fn refresh_membership(
        &self,
        budget: WakeBudget,
    ) -> Result<MembershipRefresh, SyncError> {
        let mut refresh = MembershipRefresh {
            added: 0,
            zeroed: 0,
        };
        loop {
            let held = blocking(&self.session, |session| {
                Ok(session.rln_tree().leaves()?.len())
            })
            .await?;
            let after = MembershipIndex::new(
                u32::try_from(held).map_err(|_| SyncError::OutOfRange("tree"))?,
            );
            let page: MembershipLeaves = self
                .exchange(ApiRequest::rln_leaves(after), budget)
                .await?
                .success()?
                .decode()?;
            let added = page.leaves.len();
            refresh.zeroed = page.zeroed.len();
            blocking(&self.session, move |session| {
                session.transaction(|session| {
                    let tree = session.rln_tree();
                    for (value, position) in page.leaves.iter().zip(after.get()..) {
                        tree.set_leaf(&TreeLeaf {
                            position: TreePosition::new(position),
                            value: LeafValue::from_bytes(*value),
                        })?;
                    }
                    for position in &page.zeroed {
                        tree.set_leaf(&TreeLeaf {
                            position: TreePosition::new(position.get()),
                            value: LeafValue::from_bytes([0; 32]),
                        })?;
                    }
                    // Cached interior nodes are stale once leaves change.
                    tree.clear_nodes()?;
                    Ok::<_, SyncError>(())
                })
            })
            .await?;
            refresh.added = refresh.added.saturating_add(added);
            if added == 0 {
                return Ok(refresh);
            }
        }
    }
}
