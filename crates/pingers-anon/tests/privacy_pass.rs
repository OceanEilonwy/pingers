// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass end to end: batched issuance and single-use redemption (design B.4.8).
#![cfg(all(feature = "prover", feature = "issuer"))]
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::HashSet;

use pingers_anon::{
    AnonError, IssuerKey, PrivacyPassToken, TokenCount, begin_token_issuance, check_redemption,
    issue_tokens,
};
use pingers_testkit::seeded_rng;
use zeroize::Zeroizing;

/// Issues `count` tokens from `key`.
fn issue(key: &IssuerKey, count: u16, seed: u64) -> Result<Vec<PrivacyPassToken>, AnonError> {
    let mut rng = seeded_rng(seed);
    let pending = begin_token_issuance(&key.public_key(), TokenCount::new(count)?, &mut rng)?;
    let response = issue_tokens(key, pending.request(), &mut rng)?;
    pending.finish(&response)
}

/// The server's side of a redemption: verify, then check and record the nonce.
fn redeem(
    key: &IssuerKey,
    spent: &mut HashSet<[u8; 32]>,
    token: &PrivacyPassToken,
) -> Result<bool, AnonError> {
    let nonce = check_redemption(key, token.as_bytes())?;
    Ok(spent.insert(nonce))
}

#[test]
fn issued_tokens_redeem_once_each() -> Result<(), AnonError> {
    let key = IssuerKey::generate(&mut seeded_rng(1))?;
    let tokens = issue(&key, 3, 2)?;
    assert_eq!(tokens.len(), 3);
    let mut spent = HashSet::new();
    for token in &tokens {
        assert!(redeem(&key, &mut spent, token)?, "first spend accepted");
    }
    for token in &tokens {
        assert!(!redeem(&key, &mut spent, token)?, "second spend refused");
    }
    Ok(())
}

#[test]
fn a_token_from_another_key_is_rejected() -> Result<(), AnonError> {
    let key = IssuerKey::generate(&mut seeded_rng(1))?;
    let other = IssuerKey::generate(&mut seeded_rng(3))?;
    let tokens = issue(&other, 1, 2)?;
    for token in &tokens {
        assert!(matches!(
            check_redemption(&key, token.as_bytes()),
            Err(AnonError::BadToken)
        ));
    }
    Ok(())
}

#[test]
fn a_forged_authenticator_is_rejected() -> Result<(), AnonError> {
    let key = IssuerKey::generate(&mut seeded_rng(1))?;
    let token = issue(&key, 1, 2)?.pop().ok_or(AnonError::Internal)?;
    let mut forged = token.as_bytes().to_vec();
    if let Some(last) = forged.last_mut() {
        *last ^= 1;
    }
    assert!(matches!(
        check_redemption(&key, &forged),
        Err(AnonError::BadToken)
    ));
    // Another nonce under the same authenticator is a forgery too.
    let mut renonced = token.as_bytes().to_vec();
    if let Some(byte) = renonced.get_mut(2) {
        *byte ^= 1;
    }
    assert!(matches!(
        check_redemption(&key, &renonced),
        Err(AnonError::BadToken)
    ));
    Ok(())
}

#[test]
fn a_request_for_another_key_is_refused() -> Result<(), AnonError> {
    let mut rng = seeded_rng(4);
    let key = IssuerKey::generate(&mut rng)?;
    let other = IssuerKey::generate(&mut rng)?;
    // The seed gives keys whose truncated ids differ, so the check is what refuses.
    assert_ne!(key.key_id().last(), other.key_id().last());
    let pending = begin_token_issuance(&other.public_key(), TokenCount::new(2)?, &mut rng)?;
    assert!(matches!(
        issue_tokens(&key, pending.request(), &mut rng),
        Err(AnonError::BadIssuance)
    ));
    Ok(())
}

#[test]
fn a_response_from_another_key_fails_the_batch_proof() -> Result<(), AnonError> {
    let mut rng = seeded_rng(5);
    let key = IssuerKey::generate(&mut rng)?;
    let other = IssuerKey::generate(&mut rng)?;
    let pending = begin_token_issuance(&key.public_key(), TokenCount::new(2)?, &mut rng)?;
    let decoy = begin_token_issuance(&other.public_key(), TokenCount::new(2)?, &mut rng)?;
    let response = issue_tokens(&other, decoy.request(), &mut rng)?;
    assert!(matches!(
        pending.finish(&response),
        Err(AnonError::BadIssuance)
    ));
    Ok(())
}

#[test]
fn a_stored_key_loads_back_and_issues_the_same_tokens() -> Result<(), AnonError> {
    let key = IssuerKey::generate(&mut seeded_rng(10))?;
    let restored = IssuerKey::from_bytes(Zeroizing::new(key.expose_bytes().to_vec()))?;
    assert_eq!(restored.public_key(), key.public_key());
    assert_eq!(restored.key_id(), key.key_id());
    let token = issue(&key, 1, 11)?.pop().ok_or(AnonError::Internal)?;
    check_redemption(&restored, token.as_bytes())?;
    Ok(())
}

#[test]
fn a_response_with_the_wrong_count_is_refused() -> Result<(), AnonError> {
    let mut rng = seeded_rng(6);
    let key = IssuerKey::generate(&mut rng)?;
    let pending = begin_token_issuance(&key.public_key(), TokenCount::new(2)?, &mut rng)?;
    let three = begin_token_issuance(&key.public_key(), TokenCount::new(3)?, &mut rng)?;
    let response = issue_tokens(&key, three.request(), &mut rng)?;
    assert!(matches!(
        pending.finish(&response),
        Err(AnonError::BadIssuance)
    ));
    Ok(())
}

#[test]
fn a_bad_issuer_key_is_refused() -> Result<(), AnonError> {
    let mut rng = seeded_rng(7);
    assert!(matches!(
        begin_token_issuance(&[2; 49], TokenCount::new(1)?, &mut rng),
        Err(AnonError::BadIssuance)
    ));
    assert!(matches!(
        IssuerKey::from_bytes(Zeroizing::new(vec![0xff; 48])),
        Err(AnonError::NotAFieldElement)
    ));
    Ok(())
}

#[test]
fn token_counts_are_one_to_three_hundred() {
    assert!(TokenCount::new(0).is_err());
    assert_eq!(TokenCount::new(1).map(TokenCount::get).ok(), Some(1));
    assert_eq!(TokenCount::new(300).map(TokenCount::get).ok(), Some(300));
    assert!(TokenCount::new(301).is_err());
}

#[test]
fn a_full_batch_is_issued() -> Result<(), AnonError> {
    let key = IssuerKey::generate(&mut seeded_rng(8))?;
    let tokens = issue(&key, TokenCount::MAX, 9)?;
    let nonces: HashSet<[u8; 32]> = tokens
        .iter()
        .map(|token| check_redemption(&key, token.as_bytes()))
        .collect::<Result<_, _>>()?;
    assert_eq!(nonces.len(), usize::from(TokenCount::MAX));
    Ok(())
}

#[test]
fn the_authorization_header_carries_the_token() {
    let token = PrivacyPassToken::from_bytes(vec![0xfb, 0xff, 0x00]);
    assert_eq!(token.authorization().as_str(), "PrivateToken token=\"-_8A\"");
    assert_eq!(format!("{token:?}"), "PrivacyPassToken(redacted)");
}
