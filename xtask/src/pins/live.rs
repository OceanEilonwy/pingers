// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask pins --relay <ohttp relay> --push <push relay>`: the same two variable lines,
//! from the running services instead of their configuration files, so they can be checked
//! from outside without the private keys.
//!
//! - The gateway's key config is fetched as the app fetches it: `GET <relay>/v1/ohttp/config`
//!   through the OHTTP relay (the gateway itself takes requests only from relays). Only the
//!   served key is visible this way; the next slot comes from `pingers-server pins`.
//! - The push relay's comes from `GET <push>/v1/config` (the published key), followed by
//!   `GET <push>/v1/config/next` when a next key is scheduled (`404` when none is), in that
//!   order.
//!
//! Each body is decoded with the same parsers the app uses (pingers-ohttp's `KeyConfig`,
//! pingers-proto's `PushRelayConfig`) and hashed with pingers-crypto's `key_config_hash`, so a
//! malformed answer fails here rather than in a build. Compare the output with the file mode
//! (or the binaries' `pins` subcommands) before storing the CI variables.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use pingers_ohttp::KeyConfig;
use pingers_proto::{KeyConfigHash, PushRelayConfig, WireDecode as _};
use reqwest::{Client, StatusCode, Url};

use super::variables;

/// Per request, connection included.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Larger than any key config or push relay config (41 bytes of key, a few platforms and an
/// operator name), as the server's own limit for relay configs.
const MAX_BODY_BYTES: usize = 4096;

/// The two services to ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveOptions {
    /// The OHTTP relay: its origin, or its `/ohttp` URL as servers advertise it.
    pub(crate) relay: Url,
    /// The push relay's origin.
    pub(crate) push: Url,
}

impl LiveOptions {
    /// Parses `--relay <url> --push <url>`, in either order; `None` for anything else.
    pub(crate) fn parse(options: &[String]) -> Option<Self> {
        let mut relay = None;
        let mut push = None;
        let mut options = options.iter();
        while let Some(option) = options.next() {
            let slot = match option.as_str() {
                "--relay" => &mut relay,
                "--push" => &mut push,
                _ => return None,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(web_url(options.next()?)?);
        }
        Some(Self {
            relay: relay?,
            push: push?,
        })
    }
}

/// An `http` or `https` URL with a host.
fn web_url(text: &str) -> Option<Url> {
    Url::parse(text)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

/// `<base>/<path>`, with the base's trailing slashes and, if `strip_ohttp`, its final `/ohttp`
/// removed first, so the relay may be named either way and nothing is doubled.
fn endpoint(base: &Url, path: &str, strip_ohttp: StripOhttp) -> String {
    let text = base.as_str().trim_end_matches('/');
    let origin = match strip_ohttp {
        StripOhttp::Yes => text.strip_suffix("/ohttp").unwrap_or(text),
        StripOhttp::No => text,
    };
    format!("{origin}{path}")
}

/// Whether a base URL may end in the relay's `/ohttp` path.
#[derive(Clone, Copy)]
enum StripOhttp {
    Yes,
    No,
}

/// Fetches both and prints the two variable lines.
pub(crate) fn run(options: &LiveOptions) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(fetch(options))
}

async fn fetch(options: &LiveOptions) -> Result<String> {
    let client = Client::builder()
        .timeout(TIMEOUT)
        .build()
        .context("building the HTTP client")?;
    let gateway = gateway_pin(&client, &options.relay).await?;
    let push_relay = push_relay_pins(&client, &options.push).await?;
    Ok(variables(&[gateway], &push_relay))
}

/// The served gateway key's pin, through the relay.
async fn gateway_pin(client: &Client, relay: &Url) -> Result<KeyConfigHash> {
    let url = endpoint(relay, "/v1/ohttp/config", StripOhttp::Yes);
    let body = get(client, &url)
        .await?
        .with_context(|| format!("{url}: no key config (404)"))?;
    let config = KeyConfig::decode(&body)
        .map_err(|_error| anyhow::anyhow!("{url}: not a valid OHTTP key config"))?;
    Ok(config.hash())
}

/// The push relay's published key and, when one is scheduled, its next key.
async fn push_relay_pins(client: &Client, push: &Url) -> Result<Vec<KeyConfigHash>> {
    let published = endpoint(push, "/v1/config", StripOhttp::No);
    let mut pins = vec![push_relay_pin(
        &published,
        &get(client, &published)
            .await?
            .with_context(|| format!("{published}: no published key (404)"))?,
    )?];
    let next = endpoint(push, "/v1/config/next", StripOhttp::No);
    if let Some(body) = get(client, &next).await? {
        pins.push(push_relay_pin(&next, &body)?);
    }
    Ok(pins)
}

/// The pin of a `GET /v1/config` body: the hash of its 41-byte key config.
fn push_relay_pin(url: &str, body: &[u8]) -> Result<KeyConfigHash> {
    let config = PushRelayConfig::decode(body)
        .map_err(|_error| anyhow::anyhow!("{url}: not a valid push relay config"))?;
    Ok(pingers_crypto::key_config_hash(
        &config.key_config.to_bytes(),
    ))
}

/// The body of a `200`, `None` for a `404`, an error for anything else or a body over
/// [`MAX_BODY_BYTES`].
async fn get(client: &Client, url: &str) -> Result<Option<Vec<u8>>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("{url}: request failed"))?;
    match response.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return Ok(None),
        status => bail!("{url}: answered {status}"),
    }
    let body = response
        .bytes()
        .await
        .with_context(|| format!("{url}: reading the body failed"))?;
    if body.len() > MAX_BODY_BYTES {
        bail!("{url}: body over {MAX_BODY_BYTES} bytes");
    }
    Ok(Some(body.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(options: &[&str]) -> Option<LiveOptions> {
        LiveOptions::parse(
            &options
                .iter()
                .map(|option| (*option).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn both_urls_are_needed_once_each_in_any_order() {
        let both = parse(&[
            "--push",
            "https://push.test",
            "--relay",
            "http://relay.test/ohttp",
        ]);
        assert_eq!(
            both.map(|options| (options.relay.to_string(), options.push.to_string())),
            Some((
                "http://relay.test/ohttp".to_owned(),
                "https://push.test/".to_owned()
            ))
        );
        assert_eq!(parse(&["--relay", "https://relay.test"]), None);
        assert_eq!(parse(&["--push", "https://push.test"]), None);
        assert_eq!(
            parse(&[
                "--relay",
                "https://a.test",
                "--relay",
                "https://b.test",
                "--push",
                "https://push.test"
            ]),
            None
        );
        assert_eq!(parse(&["--relay", "https://relay.test", "--push"]), None);
        assert_eq!(
            parse(&["--relay", "https://relay.test", "--push", "x", "--y"]),
            None
        );
    }

    #[test]
    fn only_web_urls_with_a_host_are_taken() {
        assert!(web_url("https://push.test").is_some());
        assert!(web_url("http://127.0.0.1:8080").is_some());
        assert!(web_url("ftp://push.test").is_none());
        assert!(web_url("push.test").is_none());
        assert!(web_url("file:///etc/passwd").is_none());
    }

    #[test]
    fn the_relay_may_be_named_by_origin_or_by_its_ohttp_url() {
        for relay in [
            "https://relay.test",
            "https://relay.test/",
            "https://relay.test/ohttp",
            "https://relay.test/ohttp/",
        ] {
            let url = web_url(relay).map(|url| endpoint(&url, "/v1/ohttp/config", StripOhttp::Yes));
            assert_eq!(
                url.as_deref(),
                Some("https://relay.test/v1/ohttp/config"),
                "{relay}"
            );
        }
        let push = web_url("https://push.test/ohttp/")
            .map(|url| endpoint(&url, "/v1/config", StripOhttp::No));
        assert_eq!(push.as_deref(), Some("https://push.test/ohttp/v1/config"));
    }
}
