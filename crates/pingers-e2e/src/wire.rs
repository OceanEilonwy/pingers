// SPDX-License-Identifier: AGPL-3.0-only
//! The client side of the anonymous board and mailbox routes (design D), as inner requests.

use pingers_crypto::{MailboxReadKey, RevokeKey};
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header};
use pingers_proto::{
    BoardCursor, BoardPage, BoardSyncRequest, BoardSyncResponse, LeaseId, MailboxId,
    MailboxMessages, MailboxSpec, MessageAccepted, SequenceNumber, WireDecode, WireEncode,
};

use crate::E2eError;
use crate::recorder::Net;

const CBOR: &str = "application/cbor";
const READ_KEY_HEADER: &str = "pingers-read-key";

fn request(method: &str, path: String, headers: Vec<Header>, body: Vec<u8>) -> BinaryRequest {
    BinaryRequest {
        method: method.to_owned(),
        path,
        headers,
        body,
    }
}

fn content_type(value: &str) -> Header {
    Header {
        name: "content-type".to_owned(),
        value: value.as_bytes().to_vec(),
    }
}

fn read_key(key: &MailboxReadKey) -> Header {
    Header {
        name: READ_KEY_HEADER.to_owned(),
        value: hex::encode(key.expose_bytes()).into_bytes(),
    }
}

fn expect(response: BinaryResponse, what: &str, ok: u16) -> Result<Vec<u8>, E2eError> {
    let status = response.status.get();
    if status == ok {
        Ok(response.body)
    } else {
        Err(E2eError::Status {
            what: what.to_owned(),
            status,
        })
    }
}

/// `POST /v1/board/sync`.
pub(crate) async fn board_sync(
    net: &Net,
    sync: &BoardSyncRequest,
) -> Result<BoardSyncResponse, E2eError> {
    let response = net
        .send(request(
            "POST",
            "/v1/board/sync".to_owned(),
            vec![content_type(CBOR)],
            sync.encode()?,
        ))
        .await?;
    Ok(BoardSyncResponse::decode(&expect(
        response,
        "board sync",
        200,
    )?)?)
}

/// `GET /v1/board/page?c=`.
pub(crate) async fn board_page(net: &Net, cursor: &BoardCursor) -> Result<BoardPage, E2eError> {
    let path = format!("/v1/board/page?c={}", hex::encode(cursor.as_bytes()));
    let response = net
        .send(request("GET", path, Vec::new(), Vec::new()))
        .await?;
    Ok(BoardPage::decode(&expect(response, "board page", 200)?)?)
}

/// `DELETE /v1/board/lease/{id}`, revealing the revoke key.
pub(crate) async fn revoke_lease(
    net: &Net,
    lease: &LeaseId,
    revoke_key: &RevokeKey,
) -> Result<(), E2eError> {
    let path = format!("/v1/board/lease/{}", hex::encode(lease.as_bytes()));
    let response = net
        .send(request(
            "DELETE",
            path,
            Vec::new(),
            revoke_key.expose_bytes().to_vec(),
        ))
        .await?;
    expect(response, "lease revocation", 204).map(drop)
}

/// `POST /v1/mailboxes`.
pub(crate) async fn register_mailbox(net: &Net, spec: &MailboxSpec) -> Result<(), E2eError> {
    let response = net
        .send(request(
            "POST",
            "/v1/mailboxes".to_owned(),
            vec![content_type(CBOR)],
            spec.encode()?,
        ))
        .await?;
    expect(response, "mailbox registration", 201).map(drop)
}

/// `POST /v1/mailboxes/{id}/msgs` with an already padded body.
pub(crate) async fn post_to_mailbox(
    net: &Net,
    mailbox: &MailboxId,
    body: Vec<u8>,
) -> Result<SequenceNumber, E2eError> {
    let path = format!("/v1/mailboxes/{}/msgs", hex::encode(mailbox.as_bytes()));
    let response = net
        .send(request(
            "POST",
            path,
            vec![content_type("application/octet-stream")],
            body,
        ))
        .await?;
    let accepted = MessageAccepted::decode(&expect(response, "mailbox post", 202)?)?;
    Ok(accepted.sequence)
}

/// `GET /v1/mailboxes/{id}/msgs?after=`.
pub(crate) async fn read_mailbox(
    net: &Net,
    mailbox: &MailboxId,
    key: &MailboxReadKey,
    after: Option<SequenceNumber>,
) -> Result<MailboxMessages, E2eError> {
    let mut path = format!("/v1/mailboxes/{}/msgs", hex::encode(mailbox.as_bytes()));
    if let Some(after) = after {
        path = format!("{path}?after={}", after.get());
    }
    let response = net
        .send(request("GET", path, vec![read_key(key)], Vec::new()))
        .await?;
    Ok(MailboxMessages::decode(&expect(
        response,
        "mailbox read",
        200,
    )?)?)
}

/// `DELETE /v1/mailboxes/{id}`.
pub(crate) async fn delete_mailbox(
    net: &Net,
    mailbox: &MailboxId,
    key: &MailboxReadKey,
) -> Result<(), E2eError> {
    let path = format!("/v1/mailboxes/{}", hex::encode(mailbox.as_bytes()));
    let response = net
        .send(request("DELETE", path, vec![read_key(key)], Vec::new()))
        .await?;
    expect(response, "mailbox deletion", 204).map(drop)
}
