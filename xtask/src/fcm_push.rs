// SPDX-License-Identifier: AGPL-3.0-only
//! The FCM step of `cargo xtask e2e-emulators --flavour gms`, against a local stack started
//! with `--fcm-service-account`: one push sent the way the Pingers server sends one, to the
//! local push relay, which opens it and sends it through FCM to phone A.
//!
//! The device's FCM registration token is sealed here to the relay's published key (`GET
//! /v1/config`) exactly as the core seals it, then posted to `/v1/push` with the server's
//! credential from the stack's push relay configuration. Nothing here touches the service
//! account: the push relay reads that file itself. The token and credential are never printed.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use getrandom::SysRng;
use pingers_crypto::{PushRelayPublicKey, PushTokenToSeal, seal_push_token};
use pingers_proto::{
    PushBind, PushClass, PushHandle, PushPlatform, PushRelayConfig, PushRequest, PushToken,
    PushTokenPlaintext, Urgency, WireDecode as _, WireEncode as _,
};
use pingers_time::{SystemClock, UnixSeconds};
use rand_core::{Rng as _, UnwrapErr};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use toml::{Table, Value};
use zeroize::Zeroizing;

/// The push relay on loopback, as the stack runs it.
const PUSH_RELAY: &str = "http://127.0.0.1:9472";
/// How long the sealed token stays good; the push is sent at once.
const LIFETIME: Duration = Duration::from_secs(3600);
/// How long either request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Whether the push relay configuration at `config` has an `[fcm]` table.
///
/// # Errors
///
/// When the file cannot be read or parsed.
pub(crate) fn configured(config: &Path) -> Result<bool> {
    Ok(read(config)?.contains_key("fcm"))
}

/// Sends `handle` to the FCM `token` through the push relay whose configuration is `config`,
/// as an alert. Returns the relay's HTTP status line.
///
/// # Errors
///
/// When the relay cannot be reached, its config does not decode, or it refuses the push.
pub(crate) fn send(config: &Path, token: &str, handle: [u8; 8]) -> Result<String> {
    let credential = credential(&read(config)?)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(post(&credential, token, handle))
}

async fn post(credential: &str, token: &str, handle: [u8; 8]) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .context("building the HTTP client")?;
    let published = client
        .get(format!("{PUSH_RELAY}/v1/config"))
        .send()
        .await
        .context("GET /v1/config from the push relay")?;
    if !published.status().is_success() {
        bail!(
            "the push relay answered {} for /v1/config",
            published.status()
        );
    }
    let config = PushRelayConfig::decode(&published.bytes().await?)
        .map_err(|_error| anyhow!("the push relay's /v1/config does not decode"))?;
    if !config.platforms.contains(&PushPlatform::Fcm) {
        bail!("the push relay does not offer FCM (no [fcm] in its configuration)");
    }
    let body = request(&config, token, handle)?;
    let answer = client
        .post(format!("{PUSH_RELAY}/v1/push"))
        .header(AUTHORIZATION, format!("Bearer {credential}"))
        .header(CONTENT_TYPE, "application/cbor")
        .body(body)
        .send()
        .await
        .context("POST /v1/push to the push relay")?;
    let status = answer.status();
    if !status.is_success() {
        bail!("the push relay refused the push: {status}");
    }
    Ok(status.to_string())
}

/// The `/v1/push` body: `token` sealed to the relay's published key under a fresh bind.
fn request(config: &PushRelayConfig, token: &str, handle: [u8; 8]) -> Result<Vec<u8>> {
    let mut rng = UnwrapErr(SysRng);
    let mut bind = [0; 32];
    rng.fill_bytes(&mut bind);
    let bind = PushBind::from_bytes(bind);
    let now = SystemClock
        .try_now()
        .map_err(|_error| anyhow!("the system clock is before 1970"))?;
    let plaintext = PushTokenPlaintext {
        platform: PushPlatform::Fcm,
        class: PushClass::Standard,
        token: PushToken::from_bytes(token.as_bytes().to_vec()),
        not_after: UnixSeconds::new(now.get().saturating_add(LIFETIME.as_secs())),
    };
    let key = &config.key_config;
    let public_key = PushRelayPublicKey::new(key.key_id.get(), key.public_key);
    let blob = seal_push_token(
        &public_key,
        &PushTokenToSeal {
            bind,
            plaintext: &plaintext,
        },
        &mut rng,
    )
    .map_err(|_error| anyhow!("sealing the token to the relay's key failed"))?;
    PushRequest {
        key_id: key.key_id,
        blob,
        bind,
        urgency: Urgency::Alert,
        handle: PushHandle::from_bytes(handle),
    }
    .encode()
    .map_err(|_error| anyhow!("encoding the push request failed"))
}

fn read(config: &Path) -> Result<Table> {
    let text = Zeroizing::new(
        std::fs::read_to_string(config).with_context(|| config.display().to_string())?,
    );
    text.parse::<Table>()
        .map_err(|_error| anyhow!("{} does not parse", config.display()))
}

/// The server credential in the push relay configuration.
fn credential(table: &Table) -> Result<Zeroizing<String>> {
    table
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(|server| server.get("credential"))
        .and_then(Value::as_str)
        .map(|credential| Zeroizing::new(credential.to_owned()))
        .context("the push relay configuration has no server credential")
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::{PushRelayPrivateKey, PushTokenToOpen, open_push_token};
    use pingers_proto::{PushRelayKeyConfig, PushRelayKeyId, TimeToLive};

    use super::*;

    #[test]
    fn the_request_opens_on_the_relay_as_an_fcm_alert_for_the_token() -> Result<()> {
        let private_key = PushRelayPrivateKey::generate(&mut UnwrapErr(SysRng));
        let public_key = private_key.public_key(3)?;
        let config = PushRelayConfig {
            key_config: PushRelayKeyConfig {
                key_id: PushRelayKeyId::new(3),
                public_key: *public_key.public_key(),
            },
            platforms: vec![PushPlatform::Fcm],
            max_time_to_live: TimeToLive::new(LIFETIME)?,
            operator: "test".to_owned(),
        };
        let body = request(&config, "fcm-token", [7; 8])?;
        let sent = PushRequest::decode(&body)?;
        assert_eq!(sent.key_id, PushRelayKeyId::new(3));
        assert_eq!(sent.urgency, Urgency::Alert);
        assert_eq!(sent.handle, PushHandle::from_bytes([7; 8]));
        let opened = open_push_token(
            &private_key,
            &PushTokenToOpen {
                key_id: 3,
                bind: sent.bind,
                blob: &sent.blob,
            },
        )?;
        assert_eq!(opened.platform, PushPlatform::Fcm);
        assert_eq!(opened.class, PushClass::Standard);
        assert_eq!(opened.token.as_bytes(), b"fcm-token");
        let now = SystemClock.try_now()?.get();
        assert!(opened.not_after.get() > now);
        assert!(opened.not_after.get() <= now.saturating_add(LIFETIME.as_secs()));
        Ok(())
    }

    #[test]
    fn fcm_and_the_credential_are_read_from_the_relay_configuration() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("xtask-fcm-push-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let config = dir.join("push-relay.toml");
        std::fs::write(&config, "[[servers]]\ncredential = \"abc\"\n")?;
        assert!(!configured(&config)?);
        assert_eq!(credential(&read(&config)?)?.as_str(), "abc");
        std::fs::write(&config, "[fcm]\nproject_id = \"p\"\n")?;
        assert!(configured(&config)?);
        assert!(credential(&read(&config)?).is_err());
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
