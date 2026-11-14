// SPDX-License-Identifier: AGPL-3.0-only
//! Service-account parsing, the signed assertion, and token caching, against a throwaway RSA
//! key made here and a mock token endpoint. No real key is ever read.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::sync::LazyLock;

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use axum::http::StatusCode as MockStatus;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pingers_testkit::ManualClock;

use super::*;
use crate::mock_http::MockHttp;

type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) const CLIENT_EMAIL: &str = "relay@pingers-test.iam.gserviceaccount.com";
const START: u64 = 1_800_000_000;
pub(crate) const TOKEN_ANSWER: &str =
    r#"{"access_token":"ya29.minted","expires_in":3599,"token_type":"Bearer"}"#;

/// One throwaway 2048-bit key for the whole test run: generating RSA keys is slow.
static TEST_KEY: LazyLock<Option<rcgen::KeyPair>> =
    LazyLock::new(|| rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).ok());

pub(crate) fn test_key() -> Result<&'static rcgen::KeyPair, &'static str> {
    TEST_KEY.as_ref().ok_or("cannot generate the test RSA key")
}

/// A service-account file as the Firebase console writes it, with the test key.
pub(crate) fn account_json(token_uri: &str) -> Result<String, &'static str> {
    Ok(serde_json::json!({
        "type": "service_account",
        "project_id": "pingers-test",
        "private_key_id": "test-key-id",
        "private_key": test_key()?.serialize_pem(),
        "client_email": CLIENT_EMAIL,
        "client_id": "1",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": token_uri,
    })
    .to_string())
}

pub(crate) fn test_account(token_uri: &str) -> Result<ServiceAccount, Box<dyn std::error::Error>> {
    Ok(ServiceAccount::from_json(&account_json(token_uri)?)?)
}

fn clock() -> ManualClock {
    ManualClock::new(UnixSeconds::new(START))
}

fn auth(token_uri: &str, clock: &ManualClock) -> Result<FcmAuth, Box<dyn std::error::Error>> {
    Ok(FcmAuth::new(
        test_account(token_uri)?,
        Arc::new(clock.clone()),
    ))
}

fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn decode_json(part: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part)?)?)
}

/// Splits a JWT and checks its signature with the test public key.
fn verified_parts(
    jwt: &str,
) -> Result<(serde_json::Value, serde_json::Value), Box<dyn std::error::Error>> {
    let (signing_input, signature) = jwt.rsplit_once('.').ok_or("no signature")?;
    let (header, claims) = signing_input.split_once('.').ok_or("no claims")?;
    let public_key =
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, test_key()?.public_key_raw());
    public_key
        .verify(
            signing_input.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature)?,
        )
        .map_err(|_| "signature does not verify")?;
    Ok((decode_json(header)?, decode_json(claims)?))
}

#[test]
fn the_assertion_is_rs256_with_the_documented_claims_and_verifies() -> TestResult {
    let account = test_account("https://oauth2.googleapis.com/token")?;
    let jwt = account.assertion(UnixSeconds::new(START))?;
    let (header, claims) = verified_parts(&jwt)?;
    assert_eq!(
        header,
        serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": "test-key-id"})
    );
    assert_eq!(
        claims,
        serde_json::json!({
            "iss": CLIENT_EMAIL,
            "scope": "https://www.googleapis.com/auth/firebase.messaging",
            "aud": "https://oauth2.googleapis.com/token",
            "iat": START,
            "exp": START + 3600,
        })
    );
    Ok(())
}

#[test]
fn the_audience_is_the_token_uri_exactly_as_written() -> TestResult {
    // Url normalises this to "https://oauth2.googleapis.com/token"; aud must not change.
    let account = test_account("https://OAUTH2.googleapis.com:443/token")?;
    let (_, claims) = verified_parts(&account.assertion(UnixSeconds::new(START))?)?;
    assert_eq!(
        claims.get("aud"),
        Some(&serde_json::json!(
            "https://OAUTH2.googleapis.com:443/token"
        ))
    );
    Ok(())
}

#[test]
fn without_a_private_key_id_the_header_has_no_kid() -> TestResult {
    for key_id in [serde_json::Value::Null, serde_json::json!("")] {
        let mut file: serde_json::Value =
            serde_json::from_str(&account_json("https://oauth2.googleapis.com/token")?)?;
        let object = file.as_object_mut().ok_or("not an object")?;
        if key_id.is_null() {
            object.remove("private_key_id");
        } else {
            object.insert("private_key_id".to_owned(), key_id);
        }
        let account = ServiceAccount::from_json(&file.to_string())?;
        let (header, _) = verified_parts(&account.assertion(UnixSeconds::new(START))?)?;
        assert_eq!(header, serde_json::json!({"alg": "RS256", "typ": "JWT"}));
    }
    Ok(())
}

fn with_field(field: &str, value: serde_json::Value) -> Result<String, Box<dyn std::error::Error>> {
    let mut file: serde_json::Value =
        serde_json::from_str(&account_json("https://oauth2.googleapis.com/token")?)?;
    file.as_object_mut()
        .ok_or("not an object")?
        .insert(field.to_owned(), value);
    Ok(file.to_string())
}

fn error_of(text: &str) -> Option<ServiceAccountError> {
    ServiceAccount::from_json(text).err()
}

#[test]
fn malformed_files_are_refused_by_field() -> TestResult {
    assert_eq!(
        error_of("{\n\"type\": "),
        Some(ServiceAccountError::Syntax { line: 2 })
    );
    assert!(matches!(
        error_of(r#"{"type":"service_account"}"#),
        Some(ServiceAccountError::Syntax { .. })
    ));
    assert!(matches!(
        error_of(&with_field("client_email", serde_json::json!(7))?),
        Some(ServiceAccountError::Syntax { .. })
    ));
    assert_eq!(
        error_of(&with_field("type", serde_json::json!("authorized_user"))?),
        Some(ServiceAccountError::NotServiceAccount)
    );
    assert_eq!(
        error_of(&with_field("client_email", serde_json::json!(""))?),
        Some(ServiceAccountError::NoClientEmail)
    );
    Ok(())
}

#[test]
fn the_type_field_may_be_absent() -> TestResult {
    let mut file: serde_json::Value =
        serde_json::from_str(&account_json("https://oauth2.googleapis.com/token")?)?;
    file.as_object_mut().ok_or("not an object")?.remove("type");
    assert!(ServiceAccount::from_json(&file.to_string()).is_ok());
    Ok(())
}

#[test]
fn the_token_uri_must_be_https_or_loopback_http() -> TestResult {
    for good in [
        "https://oauth2.googleapis.com/token",
        "http://127.0.0.1:8080/token",
        "http://[::1]:8080/token",
    ] {
        assert_eq!(error_of(&account_json(good)?), None, "{good}");
    }
    for bad in [
        "http://oauth2.googleapis.com/token",
        "http://localhost/token",
        "http://10.0.0.1/token",
        "ftp://oauth2.googleapis.com/token",
        "not a url",
    ] {
        assert_eq!(
            error_of(&account_json(bad)?),
            Some(ServiceAccountError::BadTokenUri),
            "{bad}"
        );
    }
    Ok(())
}

#[test]
fn a_private_key_pingers_crypto_refuses_is_a_bad_private_key() -> TestResult {
    // Which keys are refused is pingers-crypto's to test (rs256); this is the mapping.
    let ec_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?.serialize_pem();
    assert_eq!(
        error_of(&with_field("private_key", serde_json::json!(ec_key))?),
        Some(ServiceAccountError::BadPrivateKey)
    );
    Ok(())
}

/// Pieces of the test key that must never show up in output.
fn key_fragments() -> Result<Vec<String>, &'static str> {
    let pem = test_key()?.serialize_pem();
    let mut fragments = vec!["PRIVATE KEY".to_owned(), "test-key-id".to_owned()];
    fragments.extend(
        pem.lines()
            .filter(|line| !line.starts_with("-----"))
            .map(|line| line.chars().take(16).collect::<String>()),
    );
    Ok(fragments)
}

#[test]
fn debug_output_and_errors_hold_nothing_of_the_key() -> TestResult {
    let clock = clock();
    let auth = auth("https://oauth2.googleapis.com/token", &clock)?;
    let mut printed = vec![format!("{auth:?}"), format!("{:#?}", auth.account)];
    let pem = test_key()?.serialize_pem();
    let broken = pem.replace("-----END PRIVATE KEY-----", "");
    let errors = [
        error_of(&with_field("private_key", serde_json::json!(broken))?),
        error_of(&with_field("private_key", serde_json::json!(7))?),
        error_of(&format!(
            "{{\"private_key\": \"{}\"",
            pem.replace('\n', "\\n")
        )),
    ];
    for error in errors.into_iter().flatten() {
        printed.push(format!("{error} {error:?}"));
    }
    for error in [
        TokenError::Sign,
        TokenError::Unreachable,
        TokenError::Refused { status: 400 },
        TokenError::BadResponse,
    ] {
        printed.push(format!("{error} {error:?}"));
    }
    assert!(
        printed
            .first()
            .is_some_and(|text| text.contains(CLIENT_EMAIL))
    );
    for text in &printed {
        for fragment in key_fragments()? {
            assert!(!text.contains(&fragment), "{fragment:?} leaked");
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_token_request_is_a_jwt_bearer_grant() -> TestResult {
    let endpoint = MockHttp::start(MockStatus::OK, TOKEN_ANSWER).await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    let authorization = auth.authorization(&client()?).await?;
    assert_eq!(authorization.as_str(), "Bearer ya29.minted");
    let recorded = endpoint.recorded();
    let request = recorded.first().ok_or("no token request")?;
    assert_eq!(request.path_and_query, "/token");
    assert_eq!(
        request
            .headers
            .get("content-type")
            .ok_or("no Content-Type")?,
        "application/x-www-form-urlencoded"
    );
    let form: Vec<(String, String)> = url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect();
    let [(grant_name, grant), (assertion_name, assertion)] = form.as_slice() else {
        return Err("expected two form fields".into());
    };
    assert_eq!(grant_name, "grant_type");
    assert_eq!(grant, "urn:ietf:params:oauth:grant-type:jwt-bearer");
    assert_eq!(assertion_name, "assertion");
    let (_, claims) = verified_parts(assertion)?;
    assert_eq!(
        claims.get("aud"),
        Some(&serde_json::json!(endpoint.url("/token")))
    );
    Ok(())
}

#[tokio::test]
async fn a_token_is_reused_until_five_minutes_before_it_expires() -> TestResult {
    let endpoint = MockHttp::start(MockStatus::OK, TOKEN_ANSWER).await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    let client = client()?;
    auth.authorization(&client).await?;
    assert_eq!(endpoint.recorded().len(), 1);
    // expires_in 3599, margin 300: reused until 3299 s after minting.
    clock.advance(Duration::from_secs(3298));
    auth.authorization(&client).await?;
    assert_eq!(endpoint.recorded().len(), 1);
    clock.advance(Duration::from_secs(1));
    auth.authorization(&client).await?;
    assert_eq!(endpoint.recorded().len(), 2);
    // The new assertion carries the new time.
    let recorded = endpoint.recorded();
    let body = &recorded.last().ok_or("no request")?.body;
    let assertion = url::form_urlencoded::parse(body)
        .find(|(name, _)| name == "assertion")
        .ok_or("no assertion")?
        .1;
    let (_, claims) = verified_parts(&assertion)?;
    assert_eq!(claims.get("iat"), Some(&serde_json::json!(START + 3299)));
    Ok(())
}

#[tokio::test]
async fn a_token_shorter_lived_than_the_margin_is_minted_every_time() -> TestResult {
    let endpoint = MockHttp::start(
        MockStatus::OK,
        r#"{"access_token":"ya29.short","expires_in":300}"#,
    )
    .await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    let client = client()?;
    auth.authorization(&client).await?;
    auth.authorization(&client).await?;
    assert_eq!(endpoint.recorded().len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_token_just_longer_lived_than_the_margin_is_reused() -> TestResult {
    let endpoint = MockHttp::start(
        MockStatus::OK,
        r#"{"access_token":"ya29.short","expires_in":301}"#,
    )
    .await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    let client = client()?;
    auth.authorization(&client).await?;
    auth.authorization(&client).await?;
    assert_eq!(endpoint.recorded().len(), 1);
    Ok(())
}

#[tokio::test]
async fn forgetting_the_token_mints_a_new_one() -> TestResult {
    let endpoint = MockHttp::start(MockStatus::OK, TOKEN_ANSWER).await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    let client = client()?;
    auth.authorization(&client).await?;
    auth.forget().await;
    auth.authorization(&client).await?;
    assert_eq!(endpoint.recorded().len(), 2);
    Ok(())
}

async fn token_error_for(
    status: MockStatus,
    body: &str,
) -> Result<Option<TokenError>, Box<dyn std::error::Error>> {
    let endpoint = MockHttp::start(status, body).await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    Ok(auth.authorization(&client()?).await.err())
}

#[tokio::test]
async fn unusable_token_answers_are_typed_errors() -> TestResult {
    assert_eq!(
        token_error_for(MockStatus::BAD_REQUEST, r#"{"error":"invalid_grant"}"#).await?,
        Some(TokenError::Refused { status: 400 })
    );
    assert_eq!(
        token_error_for(MockStatus::SERVICE_UNAVAILABLE, "").await?,
        Some(TokenError::Refused { status: 503 })
    );
    for body in [
        "not json",
        r#"{"access_token":"ya29.x"}"#,
        r#"{"access_token":"ya29.x","expires_in":0}"#,
        r#"{"access_token":"","expires_in":3599}"#,
        r#"{"access_token":"ya29 x","expires_in":3599}"#,
        r#"{"access_token":"ya29.x","expires_in":3599,"token_type":"MAC"}"#,
    ] {
        assert_eq!(
            token_error_for(MockStatus::OK, body).await?,
            Some(TokenError::BadResponse),
            "{body}"
        );
    }
    assert_eq!(
        token_error_for(
            MockStatus::OK,
            r#"{"access_token":"ya29.x","expires_in":1,"token_type":"bearer"}"#
        )
        .await?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_token_endpoint_is_reported_and_not_cached() -> TestResult {
    let clock = clock();
    // Port 9 on loopback: nothing listens there in the test environment.
    let auth = auth("http://127.0.0.1:9/token", &clock)?;
    let client = client()?;
    assert_eq!(
        auth.authorization(&client).await.err(),
        Some(TokenError::Unreachable)
    );
    assert_eq!(
        auth.authorization(&client).await.err(),
        Some(TokenError::Unreachable)
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_mint_is_retried_on_the_next_call() -> TestResult {
    let endpoint = MockHttp::start(MockStatus::INTERNAL_SERVER_ERROR, "").await?;
    let clock = clock();
    let auth = auth(&endpoint.url("/token"), &clock)?;
    let client = client()?;
    assert!(auth.authorization(&client).await.is_err());
    assert!(auth.authorization(&client).await.is_err());
    assert_eq!(endpoint.recorded().len(), 2);
    Ok(())
}
