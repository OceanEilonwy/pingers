// SPDX-License-Identifier: AGPL-3.0-only
//! Typed requests for every server route the client uses (design D), with the encodings of
//! pingers-server's `wire.rs`: identifiers in paths as lowercase hex, `?after=<decimal>`,
//! `?c=<hex>` for board cursors, the mailbox read key in `pingers-read-key` as hex, and the
//! lease revoke key as the raw body of its `DELETE`.
//!
//! A builder fixes everything about a request except its anonymous credential, which the
//! engine attaches when it is actually sent (so a queued request gets a credential for the
//! bucket it is sent in, and a request that is never sent spends none).

use pingers_crypto::{GroupRequest, LogAuthKeyPair, MailboxReadKey, sign_group_request};
use pingers_ohttp::{BinaryRequest, Header};
use pingers_proto::{
    AppMessage, AuthRequest, BoardCursor, BoardSyncRequest, CommitRequest, CreateGroup, GroupId,
    LeaseId, MailboxId, MailboxPush, MailboxSpec, MembershipIndex, MembershipLeafRequest, Padded,
    PrivacyPassIssueRequest, RegisterRequest, RelayId, RlnScope, SecretBytes, SecretText,
    SequenceNumber, Subscription, SubscriptionId, WireEncode,
};
use pingers_time::UnixSeconds;
use pingers_transport::PrivacyClass;
use zeroize::Zeroizing;

use crate::SyncError;

/// The media type of every CBOR body.
const CBOR: &str = "application/cbor";
/// The header carrying a mailbox read key (auth mode Cap).
pub(crate) const READ_KEY_HEADER: &str = "pingers-read-key";
/// The header carrying a group request's signing time (auth mode Grp).
pub(crate) const GROUP_TIME_HEADER: &str = "pingers-group-time";
/// The header carrying a group request's Ed25519 signature, as hex (auth mode Grp).
pub(crate) const GROUP_SIGNATURE_HEADER: &str = "pingers-group-signature";
/// The header carrying a bearer token or an anonymous credential.
pub(crate) const AUTHORIZATION: &str = "authorization";

/// Every server route the client uses (design D).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Route {
    /// `GET /v1/params`.
    Params,
    /// `POST /v1/accounts`.
    CreateAccount,
    /// `POST /v1/auth`.
    Auth,
    /// `DELETE /v1/accounts/me`.
    DeleteAccount,
    /// `POST /v1/rln/leaf`.
    RlnLeaf,
    /// `GET /v1/rln/leaves?after=n`.
    RlnLeaves,
    /// `POST /v1/pp/issue`.
    PrivacyPassIssue,
    /// `GET /v1/pp/keys`.
    PrivacyPassKeys,
    /// `POST /v1/board/sync`.
    BoardSync,
    /// `GET /v1/board/page?c=`.
    BoardPage,
    /// `DELETE /v1/board/lease/{id}`.
    BoardRevokeLease,
    /// `POST /v1/mailboxes`.
    CreateMailbox,
    /// `POST /v1/mailboxes/{id}/msgs`.
    PostToMailbox,
    /// `GET /v1/mailboxes/{id}/msgs?after=`.
    ReadMailbox,
    /// `DELETE /v1/mailboxes/{id}`.
    DeleteMailbox,
    /// `POST /v1/groups`.
    CreateGroup,
    /// `POST /v1/groups/{id}/commit`.
    Commit,
    /// `PUT /v1/groups/{id}/subs/{sub_id}`.
    PutSubscription,
    /// `DELETE /v1/groups/{id}/subs/{sub_id}`.
    DeleteSubscription,
    /// `POST /v1/groups/{id}/msgs`.
    PostGroupMessage,
    /// `GET /v1/groups/{id}/msgs?after=`.
    ReadGroupMessages,
    /// `GET /v1/groups/{id}/state`.
    GroupState,
    /// `DELETE /v1/groups/{id}`.
    DeleteGroup,
    /// `POST /v1/groups/{id}/live?sub=`: the chunked live channel.
    Live,
    /// `GET /v1/ohttp/config`, asked of the OHTTP relay (which forwards it to the gateway) by
    /// the transport itself and never built as an [`ApiRequest`]: named for the reachability
    /// check's errors.
    OhttpConfig,
    /// `GET /v1/push-relays/{relay id}/config`: a listed push relay's key config, which the
    /// server fetches from the relay so that devices ask for it through the OHTTP relay too
    /// (design B.12: the push relay key config is anonymous traffic).
    PushRelayConfig,
    /// `GET /v1/push-relays/{relay id}/config/next`: the key that relay publishes next and
    /// when, fetched the same way, so devices pre-seal for a rotation (design B.4.9).
    PushRelayNextConfig,
    /// `PUT /v1/mailboxes/{id}/push`: replaces a mailbox's sealed push, authorised by the read
    /// key (design B.4.9).
    ReplaceMailboxPush,
}

impl Route {
    /// The privacy class the request travels in (design B.12). The public routes (parameters
    /// and key lists) go through the relay too: they need no credential, and the server then
    /// does not learn the address of whoever is about to use it.
    #[must_use]
    pub const fn class(self) -> PrivacyClass {
        match self {
            Self::CreateAccount
            | Self::Auth
            | Self::DeleteAccount
            | Self::RlnLeaf
            | Self::PrivacyPassIssue => PrivacyClass::Identified,
            Self::Live => PrivacyClass::Stream,
            Self::Params
            | Self::OhttpConfig
            | Self::PushRelayConfig
            | Self::PushRelayNextConfig
            | Self::PrivacyPassKeys
            | Self::RlnLeaves
            | Self::BoardSync
            | Self::BoardPage
            | Self::BoardRevokeLease
            | Self::CreateMailbox
            | Self::PostToMailbox
            | Self::ReadMailbox
            | Self::DeleteMailbox
            | Self::ReplaceMailboxPush
            | Self::CreateGroup
            | Self::Commit
            | Self::PutSubscription
            | Self::DeleteSubscription
            | Self::PostGroupMessage
            | Self::ReadGroupMessages
            | Self::GroupState
            | Self::DeleteGroup => PrivacyClass::Anonymous,
        }
    }

    /// The RLN scope of the anonymous credential the route needs, if any (design B.4.8 and D).
    /// Reads, deletes and board pages need none: a capability in the request (read key,
    /// revoke key, server-MACed cursor) or the group signature authorises them.
    #[must_use]
    pub const fn credential(self) -> Option<RlnScope> {
        match self {
            Self::BoardSync => Some(RlnScope::Board),
            Self::CreateMailbox | Self::PostToMailbox => Some(RlnScope::Mailbox),
            Self::CreateGroup | Self::Commit | Self::PutSubscription | Self::PostGroupMessage => {
                Some(RlnScope::Group)
            }
            Self::Params
            | Self::OhttpConfig
            | Self::PushRelayConfig
            | Self::PushRelayNextConfig
            | Self::CreateAccount
            | Self::Auth
            | Self::DeleteAccount
            | Self::RlnLeaf
            | Self::RlnLeaves
            | Self::PrivacyPassIssue
            | Self::PrivacyPassKeys
            | Self::BoardPage
            | Self::BoardRevokeLease
            | Self::ReadMailbox
            | Self::DeleteMailbox
            | Self::ReplaceMailboxPush
            | Self::DeleteSubscription
            | Self::ReadGroupMessages
            | Self::GroupState
            | Self::DeleteGroup
            | Self::Live => None,
        }
    }
}

/// How long the server keeps a group (design A.2): a Party for months, a Meetup until it has
/// been idle for two hours. The server takes a group created without it for a Meetup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GroupLifetime {
    /// A Meetup group.
    Meetup,
    /// A Party group.
    Party,
}

/// What a new group is and the MLS epoch its members are in at upload (a Party at 0, a
/// Meetup at 1, after the commit its Welcome carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupStart {
    /// Its lifetime class.
    pub lifetime: GroupLifetime,
    /// Its epoch at upload.
    pub epoch: pingers_proto::Epoch,
}

/// What a group request is signed with (auth mode Grp): the epoch's log-auth key and the
/// signing time.
pub struct GroupSigner<'key> {
    /// The current epoch's log-auth key pair.
    pub key: &'key LogAuthKeyPair,
    /// When the request is made, by the (skew-corrected) wall clock.
    pub now: UnixSeconds,
}

/// One request to the server, complete except for its anonymous credential.
///
/// `Debug` prints only the route: paths carry identifiers and headers carry secrets.
#[derive(Clone)]
pub struct ApiRequest {
    route: Route,
    group: Option<GroupId>,
    method: &'static str,
    path: String,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl core::fmt::Debug for ApiRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ApiRequest")
            .field("route", &self.route)
            .finish_non_exhaustive()
    }
}

impl Drop for ApiRequest {
    fn drop(&mut self) {
        // Bearer tokens and read keys travel in headers.
        for header in &mut self.headers {
            zeroize::Zeroize::zeroize(&mut header.value);
        }
    }
}

fn id_hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn encode(value: &impl WireEncode) -> Result<Vec<u8>, SyncError> {
    value.encode().map_err(SyncError::Encode)
}

fn after_query(after: Option<SequenceNumber>) -> String {
    after.map_or_else(String::new, |after| format!("?after={}", after.get()))
}

impl ApiRequest {
    fn new(route: Route, method: &'static str, path: String) -> Self {
        Self {
            route,
            group: None,
            method,
            path,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn with_cbor(mut self, value: &impl WireEncode) -> Result<Self, SyncError> {
        self.body = encode(value)?;
        self.headers.push(Header {
            name: "content-type".to_owned(),
            value: CBOR.as_bytes().to_vec(),
        });
        Ok(self)
    }

    fn with_header(mut self, name: &str, value: Vec<u8>) -> Self {
        self.headers.push(Header {
            name: name.to_owned(),
            value,
        });
        self
    }

    fn with_bearer(self, bearer: &SecretText) -> Self {
        let value = Zeroizing::new(format!("Bearer {}", bearer.expose()));
        self.with_header(AUTHORIZATION, value.as_bytes().to_vec())
    }

    fn with_read_key(self, read_key: &MailboxReadKey) -> Self {
        let value = Zeroizing::new(hex::encode(read_key.expose_bytes()));
        self.with_header(READ_KEY_HEADER, value.as_bytes().to_vec())
    }

    /// Signs the request as it now stands (method, path with query, body).
    fn signed(self, signer: &GroupSigner<'_>) -> Result<Self, SyncError> {
        let signature = sign_group_request(
            signer.key,
            &GroupRequest {
                method: self.method,
                path: &self.path,
                body: &self.body,
                timestamp: signer.now,
            },
        )?;
        Ok(self
            .with_header(GROUP_TIME_HEADER, signer.now.get().to_string().into_bytes())
            .with_header(
                GROUP_SIGNATURE_HEADER,
                hex::encode(signature.as_bytes()).into_bytes(),
            ))
    }

    fn in_group(mut self, group: &GroupId) -> Self {
        self.group = Some(*group);
        self
    }

    /// The group a group-log request addresses.
    #[must_use]
    pub const fn group(&self) -> Option<GroupId> {
        self.group
    }

    /// The route.
    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }

    /// The HTTP method.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        self.method
    }

    /// The path with its query (a wire string: it carries identifiers).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The body (a wire boundary).
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The headers fixed by the builder (a wire boundary).
    #[must_use]
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    /// The request as the transport carries it, with `extra` headers (the credential) added.
    pub(crate) fn to_binary(&self, extra: Option<Header>) -> BinaryRequest {
        let mut headers = self.headers.clone();
        headers.extend(extra);
        BinaryRequest {
            method: self.method.to_owned(),
            path: self.path.clone(),
            headers,
            body: self.body.clone(),
        }
    }

    /// `GET /v1/params`.
    #[must_use]
    pub fn params() -> Self {
        Self::new(Route::Params, "GET", "/v1/params".to_owned())
    }

    /// `POST /v1/accounts`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn create_account(request: &RegisterRequest) -> Result<Self, SyncError> {
        Self::new(Route::CreateAccount, "POST", "/v1/accounts".to_owned()).with_cbor(request)
    }

    /// `POST /v1/auth`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn auth(request: &AuthRequest) -> Result<Self, SyncError> {
        Self::new(Route::Auth, "POST", "/v1/auth".to_owned()).with_cbor(request)
    }

    /// `DELETE /v1/accounts/me`.
    #[must_use]
    pub fn delete_account(bearer: &SecretText) -> Self {
        Self::new(Route::DeleteAccount, "DELETE", "/v1/accounts/me".to_owned()).with_bearer(bearer)
    }

    /// `POST /v1/rln/leaf`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn rln_leaf(
        request: &MembershipLeafRequest,
        bearer: &SecretText,
    ) -> Result<Self, SyncError> {
        Ok(Self::new(Route::RlnLeaf, "POST", "/v1/rln/leaf".to_owned())
            .with_cbor(request)?
            .with_bearer(bearer))
    }

    /// `GET /v1/rln/leaves?after=n`.
    #[must_use]
    pub fn rln_leaves(after: MembershipIndex) -> Self {
        Self::new(
            Route::RlnLeaves,
            "GET",
            format!("/v1/rln/leaves?after={}", after.get()),
        )
    }

    /// `POST /v1/pp/issue`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn privacy_pass_issue(
        request: &PrivacyPassIssueRequest,
        bearer: &SecretText,
    ) -> Result<Self, SyncError> {
        Ok(
            Self::new(Route::PrivacyPassIssue, "POST", "/v1/pp/issue".to_owned())
                .with_cbor(request)?
                .with_bearer(bearer),
        )
    }

    /// `GET /v1/pp/keys`.
    #[must_use]
    pub fn privacy_pass_keys() -> Self {
        Self::new(Route::PrivacyPassKeys, "GET", "/v1/pp/keys".to_owned())
    }

    /// `GET /v1/push-relays/{relay id}/config`.
    #[must_use]
    pub fn push_relay_config(relay: RelayId) -> Self {
        Self::new(
            Route::PushRelayConfig,
            "GET",
            format!("/v1/push-relays/{}/config", relay.get()),
        )
    }

    /// `GET /v1/push-relays/{relay id}/config/next`.
    #[must_use]
    pub fn push_relay_next_config(relay: RelayId) -> Self {
        Self::new(
            Route::PushRelayNextConfig,
            "GET",
            format!("/v1/push-relays/{}/config/next", relay.get()),
        )
    }

    /// `POST /v1/board/sync`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn board_sync(request: &BoardSyncRequest) -> Result<Self, SyncError> {
        Self::new(Route::BoardSync, "POST", "/v1/board/sync".to_owned()).with_cbor(request)
    }

    /// `GET /v1/board/page?c=<cursor>`.
    #[must_use]
    pub fn board_page(cursor: &BoardCursor) -> Self {
        Self::new(
            Route::BoardPage,
            "GET",
            format!("/v1/board/page?c={}", id_hex(cursor.as_bytes())),
        )
    }

    /// `DELETE /v1/board/lease/{id}` with the revoke key as the body.
    #[must_use]
    pub fn revoke_lease(lease: &LeaseId, revoke_key: &SecretBytes<32>) -> Self {
        let mut request = Self::new(
            Route::BoardRevokeLease,
            "DELETE",
            format!("/v1/board/lease/{}", id_hex(lease.as_bytes())),
        );
        request.body = revoke_key.expose().to_vec();
        request
    }

    /// `POST /v1/mailboxes`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn create_mailbox(spec: &MailboxSpec) -> Result<Self, SyncError> {
        Self::new(Route::CreateMailbox, "POST", "/v1/mailboxes".to_owned()).with_cbor(spec)
    }

    /// `POST /v1/mailboxes/{id}/msgs` with an already padded body.
    #[must_use]
    pub fn post_to_mailbox(mailbox: &MailboxId, body: &Padded) -> Self {
        let mut request = Self::new(
            Route::PostToMailbox,
            "POST",
            format!("/v1/mailboxes/{}/msgs", id_hex(mailbox.as_bytes())),
        );
        request.body = body.as_bytes().to_vec();
        request
    }

    /// `GET /v1/mailboxes/{id}/msgs?after=seq`, authorised by the read key.
    #[must_use]
    pub fn read_mailbox(
        mailbox: &MailboxId,
        read_key: &MailboxReadKey,
        after: Option<SequenceNumber>,
    ) -> Self {
        Self::new(
            Route::ReadMailbox,
            "GET",
            format!(
                "/v1/mailboxes/{}/msgs{}",
                id_hex(mailbox.as_bytes()),
                after_query(after)
            ),
        )
        .with_read_key(read_key)
    }

    /// `DELETE /v1/mailboxes/{id}`, authorised by the read key.
    #[must_use]
    pub fn delete_mailbox(mailbox: &MailboxId, read_key: &MailboxReadKey) -> Self {
        Self::new(
            Route::DeleteMailbox,
            "DELETE",
            format!("/v1/mailboxes/{}", id_hex(mailbox.as_bytes())),
        )
        .with_read_key(read_key)
    }

    /// `PUT /v1/mailboxes/{id}/push`, authorised by the read key.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn replace_mailbox_push(
        mailbox: &MailboxId,
        read_key: &MailboxReadKey,
        update: &MailboxPush,
    ) -> Result<Self, SyncError> {
        Self::new(
            Route::ReplaceMailboxPush,
            "PUT",
            format!("/v1/mailboxes/{}/push", id_hex(mailbox.as_bytes())),
        )
        .with_cbor(update)
        .map(|request| request.with_read_key(read_key))
    }

    /// `POST /v1/groups?lifetime=meetup|party&epoch=n`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode.
    pub fn create_group(request: &CreateGroup, start: GroupStart) -> Result<Self, SyncError> {
        let lifetime = match start.lifetime {
            GroupLifetime::Meetup => "meetup",
            GroupLifetime::Party => "party",
        };
        let path = format!("/v1/groups?lifetime={lifetime}&epoch={}", start.epoch.get());
        Self::new(Route::CreateGroup, "POST", path).with_cbor(request)
    }

    /// `POST /v1/groups/{id}/commit`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode; [`SyncError::Crypto`] if signing
    /// fails.
    pub fn commit(
        group: &GroupId,
        request: &CommitRequest,
        signer: &GroupSigner<'_>,
    ) -> Result<Self, SyncError> {
        Self::new(
            Route::Commit,
            "POST",
            format!("/v1/groups/{}/commit", id_hex(group.as_bytes())),
        )
        .in_group(group)
        .with_cbor(request)?
        .signed(signer)
    }

    /// `PUT /v1/groups/{id}/subs/{sub_id}`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode; [`SyncError::Crypto`] if signing
    /// fails.
    pub fn put_subscription(
        group: &GroupId,
        subscription: &Subscription,
        signer: &GroupSigner<'_>,
    ) -> Result<Self, SyncError> {
        Self::new(
            Route::PutSubscription,
            "PUT",
            format!(
                "/v1/groups/{}/subs/{}",
                id_hex(group.as_bytes()),
                id_hex(subscription.id.as_bytes())
            ),
        )
        .with_cbor(subscription)?
        .signed(signer)
    }

    /// `DELETE /v1/groups/{id}/subs/{sub_id}`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if signing fails.
    pub fn delete_subscription(
        group: &GroupId,
        subscription: &SubscriptionId,
        signer: &GroupSigner<'_>,
    ) -> Result<Self, SyncError> {
        Self::new(
            Route::DeleteSubscription,
            "DELETE",
            format!(
                "/v1/groups/{}/subs/{}",
                id_hex(group.as_bytes()),
                id_hex(subscription.as_bytes())
            ),
        )
        .signed(signer)
    }

    /// `POST /v1/groups/{id}/msgs`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Encode`] if the body does not encode; [`SyncError::Crypto`] if signing
    /// fails.
    pub fn post_group_message(
        group: &GroupId,
        message: &AppMessage,
        signer: &GroupSigner<'_>,
    ) -> Result<Self, SyncError> {
        Self::new(
            Route::PostGroupMessage,
            "POST",
            format!("/v1/groups/{}/msgs", id_hex(group.as_bytes())),
        )
        .in_group(group)
        .with_cbor(message)?
        .signed(signer)
    }

    /// `GET /v1/groups/{id}/msgs?after=seq`; `None` reads from the start (`after=0`).
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if signing fails.
    pub fn read_group_messages(
        group: &GroupId,
        after: Option<SequenceNumber>,
        signer: &GroupSigner<'_>,
    ) -> Result<Self, SyncError> {
        let after = after.map_or(0, SequenceNumber::get);
        Self::new(
            Route::ReadGroupMessages,
            "GET",
            format!("/v1/groups/{}/msgs?after={after}", id_hex(group.as_bytes())),
        )
        .signed(signer)
    }

    /// `GET /v1/groups/{id}/state`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if signing fails.
    pub fn group_state(group: &GroupId, signer: &GroupSigner<'_>) -> Result<Self, SyncError> {
        Self::new(
            Route::GroupState,
            "GET",
            format!("/v1/groups/{}/state", id_hex(group.as_bytes())),
        )
        .signed(signer)
    }

    /// `DELETE /v1/groups/{id}`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if signing fails.
    pub fn delete_group(group: &GroupId, signer: &GroupSigner<'_>) -> Result<Self, SyncError> {
        Self::new(
            Route::DeleteGroup,
            "DELETE",
            format!("/v1/groups/{}", id_hex(group.as_bytes())),
        )
        .signed(signer)
    }

    /// `POST /v1/groups/{id}/live?sub=<sub_id>`: the head of the chunked live channel.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if signing fails.
    pub fn live(
        group: &GroupId,
        subscription: &SubscriptionId,
        signer: &GroupSigner<'_>,
    ) -> Result<Self, SyncError> {
        Self::new(
            Route::Live,
            "POST",
            format!(
                "/v1/groups/{}/live?sub={}",
                id_hex(group.as_bytes()),
                id_hex(subscription.as_bytes())
            ),
        )
        .signed(signer)
    }
}

mod queued;

#[cfg(test)]
mod tests;
