// SPDX-License-Identifier: AGPL-3.0-only
//! The account and its credentials against the real server: registration (with and without
//! the operator's registration code), the bearer, the membership leaf and the token stock, over
//! the real direct transport (HTTPS to the harness server's TLS front) and the relay.

use pingers_crypto::key_config_hash;
use pingers_proto::SecretText;
use pingers_store::{AccountBearer, AccountRegistration};
use pingers_sync::{
    CodeSource, CodeStanding, CredentialChoice, Delivered, LeafStanding, Prepared, Refill,
    SyncError, WakeBudget,
};
use pingers_testkit::HarnessConfig;
use pingers_time::Clock as _;

use crate::mailbox::knock_mailbox;
use crate::support::{Device, TestError, TestResult, World};

fn code(text: &str) -> SecretText {
    SecretText::new(text.to_owned())
}

/// A bearer the server never issued, stored as valid until `expires_at`.
fn stale_bearer(device: &Device, expires_at: pingers_time::UnixSeconds) -> TestResult {
    device.session()?.account().set_bearer(&AccountBearer {
        bearer: code(&"ab".repeat(32)),
        expires_at,
    })?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bearer_is_replaced_when_it_lapses_or_the_server_refuses_it() -> TestResult {
    let world = World::start()?;
    let device = world.account_device()?;
    device
        .engine
        .ensure_registered(WakeBudget::UNLIMITED)
        .await?;
    let _ = direct(&device)?.take_seen();
    // The server answers the leaf registration 401: one fresh bearer, one more try.
    let later = world
        .clock
        .now()
        .checked_add(std::time::Duration::from_mins(30))
        .ok_or(TestError::Unexpected("time"))?;
    stale_bearer(&device, later)?;
    assert_eq!(
        device
            .engine
            .ensure_membership_leaf(WakeBudget::UNLIMITED)
            .await?,
        LeafStanding::Registered
    );
    assert_eq!(
        direct(&device)?.take_seen(),
        ["/v1/rln/leaf", "/v1/auth", "/v1/rln/leaf"]
    );
    // A bearer that lapses this very second is not presented at all.
    stale_bearer(&device, world.clock.now())?;
    assert_eq!(
        device.engine.stock_tokens(WakeBudget::UNLIMITED).await?,
        Refill::Fetched { added: 5, held: 5 }
    );
    assert_eq!(direct(&device)?.take_seen(), ["/v1/auth", "/v1/pp/issue"]);
    Ok(())
}

fn direct(device: &Device) -> Result<&crate::support::Direct, TestError> {
    device
        .direct
        .as_deref()
        .ok_or(TestError::Unexpected("not an account device"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_device_registers_stocks_tokens_and_takes_a_leaf_once() -> TestResult {
    let world = World::start()?;
    let device = world.account_device()?;
    assert!(matches!(
        device
            .engine
            .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
            .await,
        Err(SyncError::NoCredential)
    ));

    let prepared = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    assert_eq!(prepared.tokens, Refill::Fetched { added: 5, held: 5 });
    assert_eq!(prepared.leaf, LeafStanding::Registered);
    assert!(
        prepared.membership.is_some(),
        "the main app fetched the membership tree"
    );
    assert_eq!(
        direct(&device)?.take_seen(),
        ["/v1/accounts", "/v1/auth", "/v1/pp/issue", "/v1/rln/leaf"]
    );
    let identity = device
        .session()?
        .identity()
        .load()?
        .ok_or(TestError::Unexpected("no identity"))?;
    assert_eq!(identity.server, world.server_id);
    assert_eq!(
        identity.server_pin,
        key_config_hash(&world.harness.endpoints().ohttp_key_config),
        "the account records the gateway key the relay served"
    );
    assert!(device.session()?.rln_identity().load()?.is_some());

    // pingers-sync's credential choice now finds tokens, and the server takes them.
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass
    );
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));

    // Everything is in place: the next call sends nothing.
    assert_eq!(
        device
            .engine
            .prepare_credentials(WakeBudget::UNLIMITED)
            .await?,
        Prepared {
            tokens: Refill::Stocked { held: 4 },
            leaf: LeafStanding::Held,
            membership: None,
        }
    );
    assert!(direct(&device)?.take_seen().is_empty());
    assert!(device.engine.shared_registration_code().await?.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bearer_survives_a_restart_within_its_hour() -> TestResult {
    let world = World::start()?;
    let device = world.account_device()?;
    let _ = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    let device = world.restart(device)?;
    let _ = direct(&device)?.take_seen();
    device.session()?.privacy_pass_tokens().retain_keys(&[])?;
    assert_eq!(
        device.engine.stock_tokens(WakeBudget::UNLIMITED).await?,
        Refill::Fetched { added: 5, held: 5 }
    );
    assert_eq!(direct(&device)?.take_seen(), ["/v1/pp/issue"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_forgot_the_account_gets_it_again_once() -> TestResult {
    let world = World::start()?;
    let device = world.account_device()?;
    let _ = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    // The server forgets the account and its bearers (as one whose quota store is in memory
    // does when it restarts); the device still holds a bearer it believes in.
    let bearer = device
        .session()?
        .account()
        .bearer()?
        .ok_or(TestError::Unexpected("no bearer"))?;
    let deleted = world
        .account
        .send(&pingers_ohttp::BinaryRequest {
            method: "DELETE".to_owned(),
            path: "/v1/accounts/me".to_owned(),
            headers: vec![pingers_ohttp::Header {
                name: "authorization".to_owned(),
                value: format!("Bearer {}", bearer.bearer.expose()).into_bytes(),
            }],
            body: Vec::new(),
        })
        .await?;
    assert_eq!(deleted.status.get(), 204);
    device.session()?.privacy_pass_tokens().retain_keys(&[])?;
    let _ = direct(&device)?.take_seen();

    assert_eq!(
        device.engine.stock_tokens(WakeBudget::UNLIMITED).await?,
        Refill::Fetched { added: 5, held: 5 }
    );
    // 401 to the stale bearer, 403 to the challenge of an unknown account, one registration,
    // then a fresh bearer and the refill.
    assert_eq!(
        direct(&device)?.take_seen(),
        [
            "/v1/pp/issue",
            "/v1/auth",
            "/v1/accounts",
            "/v1/auth",
            "/v1/pp/issue"
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_code_is_not_tried_again_until_another_is_offered() -> TestResult {
    let world = World::start_with(&HarnessConfig {
        registration_codes: vec!["friends".to_owned()],
        ..HarnessConfig::default()
    })?;
    let device = world.account_device()?;
    assert_eq!(
        device.engine.code_standing(code("strangers")).await?,
        CodeStanding::Open
    );
    // A later invite's code replaces an earlier one that waits.
    device
        .engine
        .offer_registration_code(code("early"), CodeSource::Invite)
        .await?;
    device
        .engine
        .offer_registration_code(code("strangers"), CodeSource::Invite)
        .await?;
    // A build's code does not replace one the user brought in.
    device
        .engine
        .offer_registration_code(code("friends"), CodeSource::Build)
        .await?;
    for _ in 0..2 {
        assert!(matches!(
            device
                .engine
                .prepare_credentials(WakeBudget::UNLIMITED)
                .await,
            Err(SyncError::RegistrationRefused)
        ));
    }
    assert_eq!(direct(&device)?.take_seen(), ["/v1/accounts"]);
    assert!(matches!(
        device.session()?.account().registration()?,
        AccountRegistration::Refused { code: Some(refused) } if refused.expose() == "strangers"
    ));
    // A screen can tell the refused code from another, without a request.
    assert_eq!(
        device.engine.code_standing(code("strangers")).await?,
        CodeStanding::RefusedBefore
    );
    assert_eq!(
        device.engine.code_standing(code("friends")).await?,
        CodeStanding::Open
    );
    assert!(direct(&device)?.take_seen().is_empty());

    // The same code again changes nothing; an empty one offers nothing.
    device
        .engine
        .offer_registration_code(code("strangers"), CodeSource::Invite)
        .await?;
    device
        .engine
        .offer_registration_code(code(""), CodeSource::Invite)
        .await?;
    assert!(matches!(
        device.engine.ensure_registered(WakeBudget::UNLIMITED).await,
        Err(SyncError::RegistrationRefused)
    ));
    assert!(direct(&device)?.take_seen().is_empty());

    device
        .engine
        .offer_registration_code(code("friends"), CodeSource::Build)
        .await?;
    let _ = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    assert_eq!(
        direct(&device)?.take_seen().first().map(String::as_str),
        Some("/v1/accounts")
    );
    assert_eq!(
        device
            .engine
            .shared_registration_code()
            .await?
            .as_ref()
            .map(SecretText::expose),
        Some("friends")
    );
    assert_eq!(
        device.engine.code_standing(code("strangers")).await?,
        CodeStanding::Registered
    );
    // A registered account keeps its code whatever is offered later.
    device
        .engine
        .offer_registration_code(code("other"), CodeSource::Invite)
        .await?;
    assert_eq!(
        device
            .engine
            .shared_registration_code()
            .await?
            .as_ref()
            .map(SecretText::expose),
        Some("friends")
    );
    Ok(())
}
