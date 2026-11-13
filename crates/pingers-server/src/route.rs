// SPDX-License-Identifier: AGPL-3.0-only
//! Which API route a request names (design D), and which channel may carry it (design B.12).
//!
//! Routing only: identifiers in the path are left for the owning crate to parse.

/// Every route of design D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Route {
    /// `GET /v1/params`.
    Params,
    /// `POST /v1/accounts`.
    CreateAccount,
    /// `POST /v1/auth`.
    Auth,
    /// `DELETE /v1/accounts/me`.
    DeleteAccount,
    /// `GET /v1/ohttp/config`.
    OhttpConfig,
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
}

/// Who a route identifies, which decides how it may arrive (design B.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// No credential and nothing about the caller: either way.
    Public,
    /// Names the account anyway: direct.
    Identified,
    /// Must not reveal the caller's address: one-shot OHTTP only.
    Anonymous,
    /// The live channel: chunked OHTTP only.
    Stream,
}

/// How a request reached the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    /// A plain HTTP request.
    Direct,
    /// A one-shot OHTTP request from an allowed relay.
    Oblivious,
    /// A chunked OHTTP exchange from an allowed relay.
    ObliviousStream,
}

impl Class {
    /// Whether a route of this class may arrive on `channel`.
    pub(crate) const fn allowed_on(self, channel: Channel) -> bool {
        matches!(
            (self, channel),
            (Self::Public, Channel::Direct | Channel::Oblivious)
                | (Self::Identified, Channel::Direct)
                | (Self::Anonymous, Channel::Oblivious)
                | (Self::Stream, Channel::ObliviousStream)
        )
    }
}

impl Route {
    pub(crate) const fn class(self) -> Class {
        match self {
            // The parameters and key configs are needed before a client can do anything else.
            Self::Params | Self::OhttpConfig | Self::PrivacyPassKeys => Class::Public,
            Self::CreateAccount
            | Self::Auth
            | Self::DeleteAccount
            | Self::RlnLeaf
            | Self::PrivacyPassIssue => Class::Identified,
            Self::Live => Class::Stream,
            // B.12 lists the membership-tree leaf list as anonymous: who fetches it when says
            // who is about to prove.
            Self::RlnLeaves
            | Self::BoardSync
            | Self::BoardPage
            | Self::BoardRevokeLease
            | Self::CreateMailbox
            | Self::PostToMailbox
            | Self::ReadMailbox
            | Self::DeleteMailbox
            | Self::CreateGroup
            | Self::Commit
            | Self::PutSubscription
            | Self::DeleteSubscription
            | Self::PostGroupMessage
            | Self::ReadGroupMessages
            | Self::GroupState
            | Self::DeleteGroup => Class::Anonymous,
        }
    }
}

/// What a method and path name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A route.
    Found(Route),
    /// The path exists, with other methods.
    MethodNotAllowed,
    /// No such path.
    NotFound,
}

/// The routes at one path, by method.
fn by_method(method: &str, routes: &[(&str, Route)]) -> Resolved {
    routes
        .iter()
        .find(|(name, _)| *name == method)
        .map_or(Resolved::MethodNotAllowed, |(_, route)| {
            Resolved::Found(*route)
        })
}

/// Resolves `method` and `path` (which may carry a query) to a route.
pub(crate) fn resolve(method: &str, path: &str) -> Resolved {
    let path = path.split_once('?').map_or(path, |(path, _query)| path);
    let Some(rest) = path.strip_prefix("/v1/") else {
        return Resolved::NotFound;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    // Identifiers are opaque here, but never empty: `//` and trailing slashes name nothing.
    if segments.iter().any(|segment| segment.is_empty()) {
        return Resolved::NotFound;
    }
    match segments.as_slice() {
        ["params"] => by_method(method, &[("GET", Route::Params)]),
        ["accounts"] => by_method(method, &[("POST", Route::CreateAccount)]),
        ["accounts", "me"] => by_method(method, &[("DELETE", Route::DeleteAccount)]),
        ["auth"] => by_method(method, &[("POST", Route::Auth)]),
        ["ohttp", "config"] => by_method(method, &[("GET", Route::OhttpConfig)]),
        ["rln", "leaf"] => by_method(method, &[("POST", Route::RlnLeaf)]),
        ["rln", "leaves"] => by_method(method, &[("GET", Route::RlnLeaves)]),
        ["pp", "issue"] => by_method(method, &[("POST", Route::PrivacyPassIssue)]),
        ["pp", "keys"] => by_method(method, &[("GET", Route::PrivacyPassKeys)]),
        ["board", "sync"] => by_method(method, &[("POST", Route::BoardSync)]),
        ["board", "page"] => by_method(method, &[("GET", Route::BoardPage)]),
        ["board", "lease", _] => by_method(method, &[("DELETE", Route::BoardRevokeLease)]),
        ["mailboxes"] => by_method(method, &[("POST", Route::CreateMailbox)]),
        ["mailboxes", _] => by_method(method, &[("DELETE", Route::DeleteMailbox)]),
        ["mailboxes", _, "msgs"] => by_method(
            method,
            &[("POST", Route::PostToMailbox), ("GET", Route::ReadMailbox)],
        ),
        ["groups"] => by_method(method, &[("POST", Route::CreateGroup)]),
        ["groups", _] => by_method(method, &[("DELETE", Route::DeleteGroup)]),
        ["groups", _, "commit"] => by_method(method, &[("POST", Route::Commit)]),
        ["groups", _, "subs", _] => by_method(
            method,
            &[
                ("PUT", Route::PutSubscription),
                ("DELETE", Route::DeleteSubscription),
            ],
        ),
        ["groups", _, "msgs"] => by_method(
            method,
            &[
                ("POST", Route::PostGroupMessage),
                ("GET", Route::ReadGroupMessages),
            ],
        ),
        ["groups", _, "state"] => by_method(method, &[("GET", Route::GroupState)]),
        ["groups", _, "live"] => by_method(method, &[("POST", Route::Live)]),
        _ => Resolved::NotFound,
    }
}

#[cfg(test)]
mod tests;
