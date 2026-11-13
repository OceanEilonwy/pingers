// SPDX-License-Identifier: AGPL-3.0-only
//! The board routes, each one call into pingers-board-store.

use std::sync::{Arc, PoisonError};

use axum::http::StatusCode;
use pingers_board_store::{Board, BoardError};
use pingers_ohttp::BinaryRequest;
use pingers_proto::{BoardCursor, BoardSyncRequest, LeaseId, Redacted};
use pingers_time::Clock;

use crate::Shared;
use crate::dispatch::Reply;
use crate::wire::{decode, either, encoded, hex_array, query, segment};

/// The board as the server holds it.
pub(crate) type BoardService = Board<Arc<dyn Clock>>;

fn status_of(error: &BoardError) -> StatusCode {
    match error {
        BoardError::LeaseNotCurrent(_) => StatusCode::UNPROCESSABLE_ENTITY,
        BoardError::RevokeKeyMismatch => StatusCode::FORBIDDEN,
        BoardError::UnknownLease => StatusCode::NOT_FOUND,
        BoardError::BadCursor => StatusCode::BAD_REQUEST,
        BoardError::CursorExpired => StatusCode::GONE,
        BoardError::PageSize | BoardError::Crypto(_) | BoardError::Internal | _ => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn with_board<R>(shared: &Shared, use_board: impl FnOnce(&mut BoardService) -> R) -> R {
    // A panic inside the board leaves at worst a stale segment, which expiry removes.
    let mut board = shared.board.lock().unwrap_or_else(PoisonError::into_inner);
    use_board(&mut board)
}

/// `POST /v1/board/sync`.
pub(crate) fn sync(shared: &Shared, request: &BinaryRequest) -> Reply {
    either(decode::<BoardSyncRequest>(&request.body).map(|sync| {
        match with_board(shared, |board| {
            shared.with_rng(|rng| board.sync(&sync, rng))
        }) {
            Ok(response) => encoded(StatusCode::OK, &response),
            Err(error) => Reply::status(status_of(&error)),
        }
    }))
}

/// `GET /v1/board/page?c=<cursor>`.
pub(crate) fn page(shared: &Shared, request: &BinaryRequest) -> Reply {
    let Some(cursor) = query(&request.path, "c").and_then(|text| hex::decode(text).ok()) else {
        return Reply::status(StatusCode::BAD_REQUEST);
    };
    let cursor = BoardCursor::from_bytes(cursor);
    match with_board(shared, |board| {
        shared.with_rng(|rng| board.page(&cursor, rng))
    }) {
        Ok(page) => encoded(StatusCode::OK, &page),
        Err(error) => Reply::status(status_of(&error)),
    }
}

/// `DELETE /v1/board/lease/{id}`, body: the revoke key.
pub(crate) fn revoke(shared: &Shared, request: &BinaryRequest) -> Reply {
    let lease = segment(&request.path, 2).and_then(hex_array::<{ LeaseId::LENGTH }>);
    let revoke_key = <[u8; 32]>::try_from(request.body.as_slice()).ok();
    let (Some(lease), Some(revoke_key)) = (lease, revoke_key) else {
        return Reply::status(StatusCode::BAD_REQUEST);
    };
    let revoke_key = Redacted::new(revoke_key);
    match with_board(shared, |board| {
        board.revoke(&LeaseId::from_bytes(lease), &revoke_key)
    }) {
        Ok(()) => Reply::status(StatusCode::NO_CONTENT),
        Err(error) => Reply::status(status_of(&error)),
    }
}

/// Drops expired segments (the sweep timer).
pub(crate) fn expire(shared: &Shared) {
    with_board(shared, BoardService::expire);
}
