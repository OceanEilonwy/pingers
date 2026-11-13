// SPDX-License-Identifier: AGPL-3.0-only
//! The board requests pingers-wake makes, against the real board.

use pingers_crypto::{RevokeKey, revoke_commitment};
use pingers_proto::{BoardCursor, BoardSyncRequest, Lease, ReadBoard, Record, SecretBytes};
use pingers_sync::{Delivered, Route, Sending, SyncError, WakeBudget};
use pingers_time::Clock as _;
use zeroize::Zeroizing;

use crate::support::{TestError, TestResult, World};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lease_is_uploaded_read_back_and_revoked() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let tokens = device.engine.token_count().await?;
    let budget = WakeBudget::UNLIMITED;
    let revoke_key = RevokeKey::from_bytes(Zeroizing::new([7; 32]));
    let records = [1_u8, 2, 3, 4].map(|fill| Record::from_bytes(&[fill; Record::SIZE]));
    let lease = Lease {
        start: world.clock.now().bucket(),
        records,
        revoke_commitment: revoke_commitment(&revoke_key),
    };
    let synced = device
        .engine
        .board_sync(
            &BoardSyncRequest {
                lease,
                replaces: None,
                read: ReadBoard::Read,
            },
            budget,
        )
        .await?;
    let Delivered::Done(response) = synced else {
        return Err(TestError::Unexpected("board sync was not sent"));
    };
    let page = response.page.ok_or(TestError::Unexpected("no page read"))?;
    assert_eq!(page.records, records.to_vec(), "our own records come back");
    assert_eq!(
        device.engine.token_count().await?,
        tokens.saturating_sub(1),
        "a board sync spends a credential"
    );

    let key = SecretBytes::new([7; 32]);
    let revoked = device
        .engine
        .revoke_lease(&response.lease_id, &key, Sending::or_drop(budget))
        .await?;
    assert_eq!(revoked, Delivered::Done(()));
    let again = device
        .engine
        .revoke_lease(&response.lease_id, &key, Sending::or_drop(budget))
        .await;
    assert!(matches!(
        again,
        Err(SyncError::Status {
            route: Route::BoardRevokeLease,
            status: 404
        })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forged_cursor_is_refused() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let page = device
        .engine
        .board_page(
            &BoardCursor::from_bytes(vec![1, 2, 3]),
            WakeBudget::UNLIMITED,
        )
        .await;
    assert!(matches!(
        page,
        Err(SyncError::Status {
            route: Route::BoardPage,
            status: 400
        })
    ));
    Ok(())
}
