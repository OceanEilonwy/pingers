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
    assert!(script.contains("intent://join#Intent;scheme=pingers;S.invite="));
    assert!(!script.contains("fetch(") && !script.contains("XMLHttpRequest"));

    let style = send(
        &mut client,
        request(Method::GET, "/j/landing.css", None, Body::empty())?,
    )
    .await?;
    assert_eq!(style.status(), StatusCode::OK);
    assert!(header(&style, CONTENT_TYPE).starts_with("text/css"));

    // A query is the same page; the server keeps only counts by channel and outcome, never a
    // path, so nothing of a link survives the request.
    let queried = send(
        &mut client,
        request(Method::GET, "/j?from=share", None, Body::empty())?,
    )
    .await?;
    assert_eq!(queried.status(), StatusCode::OK);
    let mut operator = connect(server.address, OTHER_LOOPBACK).await?;
    let metrics = send(
        &mut operator,
        request(Method::GET, "/metrics", None, Body::empty())?,
    )
    .await?;
    let metrics = String::from_utf8(read_all(metrics).await?)?;
    assert!(!metrics.contains("/j"), "{metrics}");
    assert!(!metrics.contains("share"), "{metrics}");
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
            AndroidPackage::new("org.pingers.app".to_owned())?,
            AndroidPackage::new("org.pingers.app.foss".to_owned())?,
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
            statement("org.pingers.app"),
            statement("org.pingers.app.foss")
        )
    );
    server.stop().await
}
