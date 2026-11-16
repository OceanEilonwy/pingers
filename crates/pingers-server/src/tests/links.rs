// SPDX-License-Identifier: AGPL-3.0-only
//! What a browser gets for an invite link: the static landing page with its strict policy,
//! and App Links' `assetlinks.json`. The invite itself (the fragment) never reaches the
//! server, and nothing about the request is kept.

use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE};
use axum::http::{Method, StatusCode};

use super::{
    Echo, LOOPBACK, OTHER_LOOPBACK, Setup, TestResult, configured, connect, read_all, request, run,
    send,
};
use crate::{AndroidPackage, AppLinks, CertificateFingerprint};

const RELEASE_CERT: &str = "AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16";

fn header(
    response: &axum::http::Response<hyper::body::Incoming>,
    name: impl axum::http::header::AsHeaderName,
) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_landing_page_is_static_strict_and_names_no_invite() -> TestResult {
    let server = run(
        configured(Setup {
            metrics: vec![OTHER_LOOPBACK],
            ..Setup::default()
        })?,
        Echo,
    )
    .await?;
    let mut client = connect(server.address, LOOPBACK).await?;

    let page = send(
        &mut client,
        request(Method::GET, "/j", None, Body::empty())?,
    )
    .await?;
    assert_eq!(page.status(), StatusCode::OK);
    assert!(header(&page, CONTENT_TYPE).starts_with("text/html"));
    let policy = header(&page, CONTENT_SECURITY_POLICY);
    assert!(policy.contains("default-src 'none'"), "{policy}");
    assert!(policy.contains("script-src 'self'"), "{policy}");
    assert!(!policy.contains("unsafe"), "{policy}");
    assert!(header(&page, CACHE_CONTROL).starts_with("public"));
    assert_eq!(header(&page, "referrer-policy"), "no-referrer");
    let html = String::from_utf8(read_all(page).await?)?;
    assert!(html.contains("Open in Pingers"));
    // Without [app_links] it offers the official builds, Play first.
    assert!(html.contains("<main data-apps=\"org.pingers.app org.pingers.app.foss\">"));
    // One script, this server's own; no inline code and no third-party resources.
    assert_eq!(html.matches("<script").count(), 1);
    assert!(html.contains("<script src=\"/j/landing.js\" defer></script>"));
    assert!(!html.contains("style="));
    assert!(!html.contains("src=\"http"));
    // The one absolute link is the install page, a link to follow, not a resource.
    assert_eq!(html.matches("href=\"http").count(), 1);

    let script = send(
        &mut client,
        request(Method::GET, "/j/landing.js", None, Body::empty())?,
    )
    .await?;
    assert_eq!(script.status(), StatusCode::OK);
    assert!(header(&script, CONTENT_TYPE).starts_with("text/javascript"));
    let script = String::from_utf8(read_all(script).await?)?;
    // It reads the fragment on the phone and hands it to the app, nowhere else.
    assert!(script.contains("window.location.hash"));
    assert!(script.contains("\"pingers://join#\""));
    // Invites and the owner's setup link (#s1.) both go to the app.
    assert!(script.contains("([vs]1\\."));
    // On Android, an intent URL naming the app, falling back to the install page.
    assert!(script.contains("intent://join#Intent;scheme=pingers;package="));
    assert!(script.contains("S.browser_fallback_url="));
    // A build that is not installed falls back to this page with the next build's number in
    // the fragment (nothing fetched), the last to the install page.
    assert!(script.contains("\"#\" + (index + 2) + \".\" + invite"));
    assert!(script.contains("\"hashchange\""));
    // It makes no request of any kind: no fetch, no XHR, no beacon, no reload of this page,
    // and it uses the fragment only to hand it on.
    for request in [
        "fetch(",
        "XMLHttpRequest",
        "sendBeacon",
        "WebSocket",
        "EventSource",
        "location.href =",
        "location.assign",
        "location.replace",
        "/j?app=",
        "document.title",
        "textContent",
        "innerHTML",
    ] {
        assert!(!script.contains(request), "{request}");
    }
    assert_eq!(script.matches("window.location.hash").count(), 1);

    let style = send(
        &mut client,
        request(Method::GET, "/j/landing.css", None, Body::empty())?,
    )
    .await?;
    assert_eq!(style.status(), StatusCode::OK);
    assert!(header(&style, CONTENT_TYPE).starts_with("text/css"));

    server.stop().await
}

/// Browsers never send the fragment; a client that put the invite in the request anyway, as a
/// query or a percent-encoded `#`, gets the same page, and the invite reaches neither the
/// counts the server keeps nor its counts log, which writes exactly those counts
/// (`CountsReport`: the metrics' rendering).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invite_in_the_request_is_never_logged() -> TestResult {
    const INVITE: &str = "v1.c2VjcmV0LWludml0ZQ";
    let server = run(
        configured(Setup {
            metrics: vec![OTHER_LOOPBACK],
            ..Setup::default()
        })?,
        Echo,
    )
    .await?;
    let mut client = connect(server.address, LOOPBACK).await?;
    for path in [
        format!("/j?{INVITE}"),
        format!("/j%23{INVITE}"),
        format!("/j/?from=share#{INVITE}"),
    ] {
        let Ok(sent) = request(Method::GET, &path, None, Body::empty()) else {
            // The HTTP client itself refuses a fragment in the request: nothing is sent.
            continue;
        };
        let response = send(&mut client, sent).await?;
        assert!(
            response.status() == StatusCode::OK || response.status() == StatusCode::NOT_FOUND,
            "{path}: {}",
            response.status()
        );
        drop(read_all(response).await?);
    }
    let mut operator = connect(server.address, OTHER_LOOPBACK).await?;
    let metrics = send(
        &mut operator,
        request(Method::GET, "/metrics", None, Body::empty())?,
    )
    .await?;
    let metrics = String::from_utf8(read_all(metrics).await?)?;
    for leak in ["/j", "share", "v1.", "c2VjcmV0"] {
        assert!(!metrics.contains(leak), "{leak}: {metrics}");
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asset_links_name_the_configured_apps_or_do_not_exist() -> TestResult {
    let plain = run(configured(Setup::default())?, Echo).await?;
    let mut client = connect(plain.address, LOOPBACK).await?;
    let absent = send(
        &mut client,
        request(
            Method::GET,
            "/.well-known/assetlinks.json",
            None,
            Body::empty(),
        )?,
    )
    .await?;
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    plain.stop().await?;

    let links = AppLinks {
        packages: vec![
            AndroidPackage::new("org.pingers.app.foss".to_owned())?,
            AndroidPackage::new("org.pingers.app".to_owned())?,
        ],
        certificates: vec![CertificateFingerprint::new(RELEASE_CERT)?],
    };
    let server = run(
        crate::ServerConfig {
            app_links: Some(links),
            ..configured(Setup::default())?
        },
        Echo,
    )
    .await?;
    let mut client = connect(server.address, LOOPBACK).await?;
    // The landing page offers the configured apps, in order (here F-Droid first).
    let page = send(
        &mut client,
        request(Method::GET, "/j", None, Body::empty())?,
    )
    .await?;
    let html = String::from_utf8(read_all(page).await?)?;
    assert!(html.contains("<main data-apps=\"org.pingers.app.foss org.pingers.app\">"));
    let response = send(
        &mut client,
        request(
            Method::GET,
            "/.well-known/assetlinks.json",
            None,
            Body::empty(),
        )?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, CONTENT_TYPE), "application/json");
    let body = String::from_utf8(read_all(response).await?)?;
    let statement = |package: &str| {
        format!(
            "{{\"relation\":[\"delegate_permission/common.handle_all_urls\"],\
             \"target\":{{\"namespace\":\"android_app\",\"package_name\":\"{package}\",\
             \"sha256_cert_fingerprints\":[\"{RELEASE_CERT}\"]}}}}"
        )
    };
    assert_eq!(
        body,
        format!(
            "[{},{}]\n",
            statement("org.pingers.app.foss"),
            statement("org.pingers.app")
        )
    );
    server.stop().await
}
