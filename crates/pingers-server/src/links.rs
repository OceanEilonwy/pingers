// SPDX-License-Identifier: AGPL-3.0-only
//! What a browser sees of invite links (`https://<server>/j#v1.<invite>`): the landing page at
//! `GET /j` that hands the invite to the app, and `GET /.well-known/assetlinks.json`, which lets
//! Android open this server's links in the app directly (App Links).
//!
//! The invite is in the link's fragment, which browsers never send: the server only ever sees
//! `GET /j`. The page is static, the same for every invite and cacheable; its script reads the
//! fragment on the phone and forwards it to `pingers://join#<invite>`, or on Android to each
//! configured app in turn through an intent URL. No request is logged
//! (the server keeps counts only, by channel and outcome).

use axum::body::Bytes;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};

use crate::config::{AndroidPackage, AppLinks};

/// The landing page, with `{{apps}}` where [`LandingPage::render`] lists the apps.
const LANDING_HTML: &str = include_str!("../assets/landing.html");
/// Its one script: reads the fragment and points "Open in Pingers" at the app.
const LANDING_JS: &str = include_str!("../assets/landing.js");
/// Its stylesheet.
const LANDING_CSS: &str = include_str!("../assets/landing.css");

/// The landing page's path; the page loads its script and stylesheet from under it.
pub(crate) const LANDING_PATH: &str = "/j";
/// The script's path.
pub(crate) const SCRIPT_PATH: &str = "/j/landing.js";
/// The stylesheet's path.
pub(crate) const STYLE_PATH: &str = "/j/landing.css";
/// Where Android looks for the apps that may open this server's links.
pub(crate) const ASSET_LINKS_PATH: &str = "/.well-known/assetlinks.json";

/// Only this server's own script and stylesheet; nothing inline, nothing third-party, no
/// frames, forms or connections.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// A day: the page is the same for every invite.
const CACHE_CONTROL: &str = "public, max-age=86400";

/// Where the page lists the apps its script offers, in order.
const APPS_PLACEHOLDER: &str = "{{apps}}";

/// The official builds (Play, then F-Droid): what the page offers when no `[app_links]` names
/// the apps.
const OFFICIAL_PACKAGES: &str = "org.pingers.app org.pingers.app.foss";

/// `GET /j`.
pub(crate) fn landing(page: &LandingPage) -> Response {
    asset(page.0.clone(), "text/html; charset=utf-8")
}

/// The landing page, rendered once from the configuration: the same for every invite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandingPage(Bytes);

impl LandingPage {
    /// Lists the configured apps' package ids, or the official builds', for the script's
    /// intent URLs. Package ids were checked when the configuration was read (letters, digits,
    /// underscores and dots), so nothing needs escaping.
    pub(crate) fn render(links: Option<&AppLinks>) -> Self {
        let apps = links.map_or_else(
            || OFFICIAL_PACKAGES.to_owned(),
            |links| {
                links
                    .packages
                    .iter()
                    .map(AndroidPackage::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            },
        );
        Self(Bytes::from(LANDING_HTML.replace(APPS_PLACEHOLDER, &apps)))
    }
}

/// `GET /j/landing.js`.
pub(crate) async fn script() -> Response {
    asset(
        Bytes::from_static(LANDING_JS.as_bytes()),
        "text/javascript; charset=utf-8",
    )
}

/// `GET /j/landing.css`.
pub(crate) async fn style() -> Response {
    asset(
        Bytes::from_static(LANDING_CSS.as_bytes()),
        "text/css; charset=utf-8",
    )
}

fn asset(body: Bytes, content_type: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// `GET /.well-known/assetlinks.json`: the configured apps, or 404 when none are.
pub(crate) fn asset_links(links: Option<&AssetLinks>) -> Response {
    let Some(links) = links else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = links.0.clone().into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    response
}

/// The Digital Asset Links statement list, rendered once from the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssetLinks(String);

impl AssetLinks {
    /// One `handle_all_urls` statement per package, each naming every certificate. Package ids
    /// and fingerprints were checked when the configuration was read, so nothing needs
    /// escaping.
    pub(crate) fn render(links: &AppLinks) -> Self {
        let certificates = links
            .certificates
            .iter()
            .map(|certificate| format!("\"{}\"", certificate.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        let statements = links
            .packages
            .iter()
            .map(|package| {
                format!(
                    "{{\"relation\":[\"delegate_permission/common.handle_all_urls\"],\
                     \"target\":{{\"namespace\":\"android_app\",\"package_name\":\"{}\",\
                     \"sha256_cert_fingerprints\":[{certificates}]}}}}",
                    package.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        Self(format!("[{statements}]\n"))
    }
}

/// What the server answers for invite links, rendered once from the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkPages {
    /// The landing page at `/j`.
    pub(crate) landing: LandingPage,
    /// `/.well-known/assetlinks.json`, when App Links are configured.
    pub(crate) asset_links: Option<AssetLinks>,
}

impl LinkPages {
    /// The landing page naming `links`' apps (or the official builds), and their asset links.
    pub(crate) fn render(links: Option<&AppLinks>) -> Self {
        Self {
            landing: LandingPage::render(links),
            asset_links: links.map(AssetLinks::render),
        }
    }
}
