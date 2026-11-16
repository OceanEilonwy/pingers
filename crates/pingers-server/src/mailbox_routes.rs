// SPDX-License-Identifier: AGPL-3.0-only
//! The mailbox routes, each one call into pingers-mailbox. SQLite runs on the blocking pool; a
//! post's wake becomes a push job owned by the server.

use std::sync::{Arc, Mutex, PoisonError};

use axum::http::StatusCode;
use pingers_crypto::MailboxReadKey;
use pingers_mailbox::{ListedRelays, MailboxError, Mailboxes, PostsPerBucket, SqliteStorage};
use pingers_ohttp::BinaryRequest;
use pingers_proto::{
    MailboxId, MailboxPush, MailboxSpec, MessageAccepted, Padded, RlnScope, SequenceNumber,
};
use pingers_time::Clock;
use zeroize::Zeroizing;

use crate::Shared;
use crate::credential::admit;
use crate::dispatch::Reply;
use crate::wire::{READ_KEY_HEADER, decode, encoded, header_hex, hex_array, query, segment};

/// The mailboxes as the server holds them.
pub(crate) type MailboxService =
    Mailboxes<SqliteStorage, Arc<dyn Clock>, ListedRelays, PostsPerBucket>;

fn status_of(error: &MailboxError) -> StatusCode {
    match error {
        MailboxError::LifetimeTooLong(_)
        | MailboxError::UnknownRelay
        | MailboxError::NotAMailboxClass(_) => StatusCode::UNPROCESSABLE_ENTITY,
        MailboxError::AlreadyExists | MailboxError::Full => StatusCode::CONFLICT,
        MailboxError::NotFound => StatusCode::NOT_FOUND,
        MailboxError::WrongReadKey => StatusCode::FORBIDDEN,
        MailboxError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        MailboxError::Storage(_) | MailboxError::Internal | _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Runs `use_mailboxes` on the blocking pool, since storage may be an SQLite file.
async fn blocking<R: Send + 'static>(
    mailboxes: &Arc<Mutex<MailboxService>>,
    use_mailboxes: impl FnOnce(&mut MailboxService) -> R + Send + 'static,
) -> Result<R, Reply> {
    let mailboxes = Arc::clone(mailboxes);
    tokio::task::spawn_blocking(move || {
        use_mailboxes(&mut mailboxes.lock().unwrap_or_else(PoisonError::into_inner))
    })
    .await
    .map_err(|_| Reply::status(StatusCode::INTERNAL_SERVER_ERROR))
}

fn mailbox_id(request: &BinaryRequest) -> Result<MailboxId, Reply> {
    segment(&request.path, 1)
        .and_then(hex_array::<{ MailboxId::LENGTH }>)
        .map(MailboxId::from_bytes)
        .ok_or(Reply::status(StatusCode::BAD_REQUEST))
}

fn read_key(request: &BinaryRequest) -> Result<MailboxReadKey, Reply> {
    header_hex::<{ MailboxReadKey::LENGTH }>(request, READ_KEY_HEADER)
        .map(|bytes| MailboxReadKey::from_bytes(Zeroizing::new(bytes)))
        .ok_or(Reply::status(StatusCode::FORBIDDEN))
}

fn answer<T>(result: Result<T, MailboxError>, ok: impl FnOnce(T) -> Reply) -> Reply {
    match result {
        Ok(value) => ok(value),
        Err(error) => Reply::status(status_of(&error)),
    }
}

/// Checks the Anon credential (scope Mailbox) on the blocking pool.
async fn admitted(shared: &Arc<Shared>, request: &BinaryRequest) -> Result<(), Reply> {
    let shared = Arc::clone(shared);
    let request = request.clone();
    tokio::task::spawn_blocking(move || admit(&shared, &request, RlnScope::Mailbox).map(drop))
        .await
        .map_err(|_| Reply::status(StatusCode::INTERNAL_SERVER_ERROR))?
}

/// `POST /v1/mailboxes`: Anon, scope Mailbox.
pub(crate) async fn register(
    shared: &Arc<Shared>,
    request: &BinaryRequest,
) -> Result<Reply, Reply> {
    admitted(shared, request).await?;
    let spec = decode::<MailboxSpec>(&request.body)?;
    let registered = blocking(&shared.mailboxes, move |mailboxes| {
        mailboxes.register(&spec)
    })
    .await?;
    Ok(answer(registered, |()| Reply::status(StatusCode::CREATED)))
}

/// `POST /v1/mailboxes/{id}/msgs`: Anon, scope Mailbox. Stores the post and hands its wake
/// to the push jobs.
pub(crate) async fn post(shared: &Arc<Shared>, request: &BinaryRequest) -> Result<Reply, Reply> {
    admitted(shared, request).await?;
    let id = mailbox_id(request)?;
    let body = Padded::from_wire(request.body.clone())
        .map_err(|_| Reply::status(StatusCode::UNPROCESSABLE_ENTITY))?;
    // The post draws its push delay on the blocking pool, from a generator seeded here.
    let mut rng = shared.forked_rng();
    let posted = blocking(&shared.mailboxes, move |mailboxes| {
        mailboxes.post(&id, &body, &mut rng)
    })
    .await?;
    Ok(answer(posted, |posted| {
        if let Some(job) = posted.wake {
            shared.tasks.forward(job);
        }
        encoded(
            StatusCode::ACCEPTED,
            &MessageAccepted {
                sequence: posted.sequence,
            },
        )
    }))
}

/// `GET /v1/mailboxes/{id}/msgs?after=seq`.
pub(crate) async fn read(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let id = mailbox_id(request)?;
    let key = read_key(request)?;
    let after = match query(&request.path, "after") {
        None => None,
        Some(text) => Some(SequenceNumber::new(
            text.parse()
                .map_err(|_| Reply::status(StatusCode::BAD_REQUEST))?,
        )),
    };
    let messages = blocking(&shared.mailboxes, move |mailboxes| {
        mailboxes.read(&id, &key, after)
    })
    .await?;
    Ok(answer(messages, |messages| {
        encoded(StatusCode::OK, &messages)
    }))
}

/// `DELETE /v1/mailboxes/{id}`.
pub(crate) async fn delete(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let id = mailbox_id(request)?;
    let key = read_key(request)?;
    let deleted = blocking(&shared.mailboxes, move |mailboxes| {
        mailboxes.delete(&id, &key)
    })
    .await?;
    Ok(answer(deleted, |()| Reply::status(StatusCode::NO_CONTENT)))
}

/// `PUT /v1/mailboxes/{id}/push`: Cap (the read key). Replaces the sealed push.
pub(crate) async fn replace_push(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let id = mailbox_id(request)?;
    let key = read_key(request)?;
    let update = decode::<MailboxPush>(&request.body)?;
    let replaced = blocking(&shared.mailboxes, move |mailboxes| {
        mailboxes.replace_push(&id, &key, &update)
    })
    .await?;
    Ok(answer(replaced, |()| Reply::status(StatusCode::NO_CONTENT)))
}

/// Deletes expired mailboxes (the sweep timer). A storage failure is retried next sweep.
pub(crate) async fn expire(mailboxes: &Arc<Mutex<MailboxService>>) {
    let _ = blocking(mailboxes, MailboxService::expire).await;
}
