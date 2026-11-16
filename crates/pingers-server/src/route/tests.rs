// SPDX-License-Identifier: AGPL-3.0-only
//! Every design D route resolves, and each class arrives only on its channels.

use super::{Channel, Class, Resolved, Route, resolve};

/// Design D, route by route: method, an example path, the route, its class.
const TABLE: [(&str, &str, Route, Class); 30] = [
    (
        "POST",
        "/v1/maps/ohttp",
        Route::MapRequest,
        Class::Anonymous,
    ),
    (
        "GET",
        "/v1/maps/ohttp-keys",
        Route::MapKeys,
        Class::Anonymous,
    ),
    ("GET", "/v1/params", Route::Params, Class::Public),
    (
        "POST",
        "/v1/accounts",
        Route::CreateAccount,
        Class::Identified,
    ),
    ("POST", "/v1/auth", Route::Auth, Class::Identified),
    (
        "DELETE",
        "/v1/accounts/me",
        Route::DeleteAccount,
        Class::Identified,
    ),
    ("GET", "/v1/ohttp/config", Route::OhttpConfig, Class::Public),
    ("POST", "/v1/rln/leaf", Route::RlnLeaf, Class::Identified),
    (
        "GET",
        "/v1/rln/leaves?after=7",
        Route::RlnLeaves,
        Class::Anonymous,
    ),
    (
        "POST",
        "/v1/pp/issue",
        Route::PrivacyPassIssue,
        Class::Identified,
    ),
    ("GET", "/v1/pp/keys", Route::PrivacyPassKeys, Class::Public),
    (
        "GET",
        "/v1/push-relays/1/config",
        Route::PushRelayConfig,
        Class::Public,
    ),
    (
        "GET",
        "/v1/push-relays/1/config/next",
        Route::PushRelayNextConfig,
        Class::Public,
    ),
    ("POST", "/v1/board/sync", Route::BoardSync, Class::Anonymous),
    (
        "GET",
        "/v1/board/page?c=abc",
        Route::BoardPage,
        Class::Anonymous,
    ),
    (
        "DELETE",
        "/v1/board/lease/l1",
        Route::BoardRevokeLease,
        Class::Anonymous,
    ),
    (
        "POST",
        "/v1/mailboxes",
        Route::CreateMailbox,
        Class::Anonymous,
    ),
    (
        "POST",
        "/v1/mailboxes/m1/msgs",
        Route::PostToMailbox,
        Class::Anonymous,
    ),
    (
        "GET",
        "/v1/mailboxes/m1/msgs?after=3",
        Route::ReadMailbox,
        Class::Anonymous,
    ),
    (
        "DELETE",
        "/v1/mailboxes/m1",
        Route::DeleteMailbox,
        Class::Anonymous,
    ),
    (
        "PUT",
        "/v1/mailboxes/m1/push",
        Route::MailboxPush,
        Class::Anonymous,
    ),
    ("POST", "/v1/groups", Route::CreateGroup, Class::Anonymous),
    (
        "POST",
        "/v1/groups/g1/commit",
        Route::Commit,
        Class::Anonymous,
    ),
    (
        "PUT",
        "/v1/groups/g1/subs/s1",
        Route::PutSubscription,
        Class::Anonymous,
    ),
    (
        "DELETE",
        "/v1/groups/g1/subs/s1",
        Route::DeleteSubscription,
        Class::Anonymous,
    ),
    (
        "POST",
        "/v1/groups/g1/msgs",
        Route::PostGroupMessage,
        Class::Anonymous,
    ),
    (
        "GET",
        "/v1/groups/g1/msgs?after=0",
        Route::ReadGroupMessages,
        Class::Anonymous,
    ),
    (
        "GET",
        "/v1/groups/g1/state",
        Route::GroupState,
        Class::Anonymous,
    ),
    (
        "DELETE",
        "/v1/groups/g1",
        Route::DeleteGroup,
        Class::Anonymous,
    ),
    (
        "POST",
        "/v1/groups/g1/live?sub=s1",
        Route::Live,
        Class::Stream,
    ),
];

#[test]
fn every_design_route_resolves_to_its_route_and_class() {
    for (method, path, route, class) in TABLE {
        assert_eq!(
            resolve(method, path),
            Resolved::Found(route),
            "{method} {path}"
        );
        assert_eq!(route.class(), class, "{route:?}");
    }
}

#[test]
fn another_method_on_a_known_path_is_not_allowed() {
    for (_, path, _, _) in TABLE {
        assert_eq!(resolve("PATCH", path), Resolved::MethodNotAllowed, "{path}");
    }
}

#[test]
fn unknown_paths_and_empty_segments_name_nothing() {
    for path in [
        "/",
        "/ohttp",
        "/v2/params",
        "/v1/",
        "/v1/params/",
        "/v1/nope",
        "/v1/groups//state",
        "/v1/mailboxes/m1/msgs/extra",
        "/v1/board/lease",
        "/v1/groups/g1/subs",
        "v1/params",
    ] {
        assert_eq!(resolve("GET", path), Resolved::NotFound, "{path}");
    }
}

#[test]
fn each_class_arrives_only_on_its_channels() {
    let channels = [
        Channel::Direct,
        Channel::Oblivious,
        Channel::ObliviousStream,
    ];
    let expected = [
        (Class::Public, [true, true, false]),
        (Class::Identified, [true, false, false]),
        (Class::Anonymous, [false, true, false]),
        (Class::Stream, [false, true, true]),
    ];
    for (class, allowed) in expected {
        for (channel, allowed) in channels.into_iter().zip(allowed) {
            assert_eq!(
                class.allowed_on(channel),
                allowed,
                "{class:?} on {channel:?}"
            );
        }
    }
}
