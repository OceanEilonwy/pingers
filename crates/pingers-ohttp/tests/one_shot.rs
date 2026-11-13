// SPDX-License-Identifier: AGPL-3.0-only
//! One-shot OHTTP (RFC 9458) through the public API, and key configs.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod common;

use common::{SECRET, TestError, gateway_key, request, response, setup};
use pingers_ohttp::{Gateway, GatewayKey, KeyConfig, OhttpError, check_pin, encapsulate_request};
use pingers_testkit::{FixedRng, seeded_rng};

#[test]
fn request_and_response_round_trip() -> Result<(), TestError> {
    let setup = setup()?;
    let mut rng = seeded_rng(1);
    let encapsulated = encapsulate_request(&setup.config, &request(), &mut rng)?;
    let decapsulated = setup.gateway.decapsulate(&encapsulated.message)?;
    assert_eq!(decapsulated.request, request());
    let sealed = decapsulated.responder.seal(&response()?, &mut rng)?;
    assert_eq!(encapsulated.response_context.open(&sealed)?, response()?);
    Ok(())
}

#[test]
fn each_request_uses_fresh_hpke_randomness() -> Result<(), TestError> {
    // Unlinkability (design B.12): two requests with the same content share no bytes after
    // the header, because each draws its own ephemeral key.
    let setup = setup()?;
    let mut rng = seeded_rng(2);
    let first = encapsulate_request(&setup.config, &request(), &mut rng)?;
    let second = encapsulate_request(&setup.config, &request(), &mut rng)?;
    assert_eq!(first.message.get(..7), second.message.get(..7));
    assert_ne!(first.message.get(7..39), second.message.get(7..39));
    Ok(())
}

#[test]
fn encapsulation_takes_its_randomness_from_the_caller() -> Result<(), TestError> {
    let setup = setup()?;
    let first = encapsulate_request(&setup.config, &request(), &mut seeded_rng(3))?;
    let again = encapsulate_request(&setup.config, &request(), &mut seeded_rng(3))?;
    assert_eq!(first.message, again.message);
    Ok(())
}

#[test]
fn request_header_names_key_and_pinned_suite() -> Result<(), TestError> {
    let setup = setup()?;
    let encapsulated = encapsulate_request(&setup.config, &request(), &mut seeded_rng(4))?;
    assert_eq!(
        encapsulated.message.get(..7),
        Some([1, 0x00, 0x20, 0x00, 0x01, 0x00, 0x01].as_slice())
    );
    Ok(())
}

#[test]
fn unknown_key_id_is_reported() -> Result<(), TestError> {
    let setup = setup()?;
    let mut message = encapsulate_request(&setup.config, &request(), &mut seeded_rng(5))?.message;
    if let Some(key_id) = message.first_mut() {
        *key_id = 9;
    }
    assert!(matches!(
        setup.gateway.decapsulate(&message),
        Err(OhttpError::UnknownKeyId { key_id: 9 })
    ));
    Ok(())
}

#[test]
fn other_suites_are_refused() -> Result<(), TestError> {
    let setup = setup()?;
    let message = encapsulate_request(&setup.config, &request(), &mut seeded_rng(6))?.message;
    // KEM, KDF and AEAD each changed in turn; ChaCha20-Poly1305 (AEAD 3) included.
    for (index, value) in [(2, 0x10), (4, 0x02), (6, 0x03)] {
        let mut changed = message.clone();
        if let Some(byte) = changed.get_mut(index) {
            *byte = value;
        }
        assert!(matches!(
            setup.gateway.decapsulate(&changed),
            Err(OhttpError::UnsupportedSuite)
        ));
    }
    Ok(())
}

#[test]
fn tampered_request_does_not_open() -> Result<(), TestError> {
    let setup = setup()?;
    let mut message = encapsulate_request(&setup.config, &request(), &mut seeded_rng(7))?.message;
    if let Some(last) = message.last_mut() {
        *last ^= 1;
    }
    assert!(matches!(
        setup.gateway.decapsulate(&message),
        Err(OhttpError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn short_requests_do_not_open() -> Result<(), TestError> {
    let setup = setup()?;
    let message = encapsulate_request(&setup.config, &request(), &mut seeded_rng(8))?.message;
    for length in [0, 1, 6, 7, 39, 54] {
        let short = message
            .get(..length)
            .ok_or(TestError::Setup("short message"))?;
        assert!(
            matches!(
                setup.gateway.decapsulate(short),
                Err(OhttpError::OpenFailed)
            ),
            "length {length}"
        );
    }
    Ok(())
}

#[test]
fn tampered_and_short_responses_do_not_open() -> Result<(), TestError> {
    let setup = setup()?;
    let mut rng = seeded_rng(9);
    let first = encapsulate_request(&setup.config, &request(), &mut rng)?;
    let sealed = setup
        .gateway
        .decapsulate(&first.message)?
        .responder
        .seal(&response()?, &mut rng)?;
    let mut tampered = sealed.clone();
    if let Some(byte) = tampered.get_mut(20) {
        *byte ^= 0x80;
    }
    assert!(matches!(
        first.response_context.open(&tampered),
        Err(OhttpError::OpenFailed)
    ));
    let second = encapsulate_request(&setup.config, &request(), &mut rng)?;
    assert!(matches!(
        second
            .response_context
            .open(sealed.get(..16).unwrap_or_default()),
        Err(OhttpError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn a_response_opens_only_for_its_own_request() -> Result<(), TestError> {
    let setup = setup()?;
    let mut rng = seeded_rng(10);
    let first = encapsulate_request(&setup.config, &request(), &mut rng)?;
    let second = encapsulate_request(&setup.config, &request(), &mut rng)?;
    let sealed_for_first = setup
        .gateway
        .decapsulate(&first.message)?
        .responder
        .seal(&response()?, &mut rng)?;
    assert!(matches!(
        second.response_context.open(&sealed_for_first),
        Err(OhttpError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn malformed_inner_request_is_bad_message() -> Result<(), TestError> {
    // Valid HPKE around bytes that are not Binary HTTP: the gateway must say BadMessage, not
    // OpenFailed, so the server can tell a broken client from an attack.
    let setup = setup()?;
    let client = ohttp::ClientRequest::from_encoded_config(setup.config.encoded())
        .map_err(|_| TestError::Setup("upstream client"))?;
    let (message, _) = client
        .encapsulate(b"\x05not binary http", &mut seeded_rng(11))
        .map_err(|_| TestError::Setup("upstream encapsulate"))?;
    assert!(matches!(
        setup.gateway.decapsulate(&message),
        Err(OhttpError::BadMessage)
    ));
    Ok(())
}

#[test]
fn rotation_accepts_requests_to_either_key() -> Result<(), TestError> {
    let mut rng = seeded_rng(12);
    let old = GatewayKey::generate(1, &mut rng)?;
    let new = GatewayKey::generate(2, &mut rng)?;
    let (old_config, new_config) = (old.config(), new.config());
    let gateway = Gateway::new(vec![old, new]);
    for config in [old_config, new_config] {
        let encapsulated = encapsulate_request(&config, &request(), &mut rng)?;
        assert_eq!(
            gateway.decapsulate(&encapsulated.message)?.request,
            request()
        );
    }
    Ok(())
}

#[test]
fn a_request_to_a_key_with_the_same_id_but_other_secret_does_not_open() -> Result<(), TestError> {
    let mut rng = seeded_rng(13);
    let ours = GatewayKey::generate(1, &mut rng)?;
    let theirs = GatewayKey::generate(1, &mut rng)?;
    let encapsulated = encapsulate_request(&theirs.config(), &request(), &mut rng)?;
    assert!(matches!(
        Gateway::new(vec![ours]).decapsulate(&encapsulated.message),
        Err(OhttpError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn generated_keys_are_deterministic_for_a_seed_and_differ_across_seeds() -> Result<(), TestError> {
    let first = GatewayKey::generate(1, &mut seeded_rng(14))?.config();
    let again = GatewayKey::generate(1, &mut seeded_rng(14))?.config();
    let other = GatewayKey::generate(1, &mut seeded_rng(15))?.config();
    assert_eq!(first, again);
    assert_ne!(first, other);
    Ok(())
}

#[test]
fn generate_draws_exactly_one_key_worth_of_randomness() -> Result<(), TestError> {
    let mut rng = FixedRng::new(&[7; 32]);
    let key = GatewayKey::generate(3, &mut rng)?;
    assert!(rng.exhausted_exactly());
    assert_eq!(key.config().key_id(), 3);
    Ok(())
}

#[test]
fn served_config_offers_exactly_the_pinned_suite() -> Result<(), TestError> {
    let config = gateway_key(5, SECRET)?.config();
    let encoded = config.encoded();
    assert_eq!(encoded.len(), 41);
    assert_eq!(encoded.get(..3), Some([5, 0x00, 0x20].as_slice()));
    assert_eq!(
        encoded.get(35..),
        Some([0x00, 0x04, 0x00, 0x01, 0x00, 0x01].as_slice())
    );
    assert_eq!(config.key_id(), 5);
    Ok(())
}

#[test]
fn pin_check_accepts_the_same_config_and_refuses_another() -> Result<(), TestError> {
    let config = gateway_key(1, SECRET)?.config();
    let other = GatewayKey::generate(1, &mut seeded_rng(16))?.config();
    check_pin(&config, &config.hash())?;
    assert!(matches!(
        check_pin(&other, &config.hash()),
        Err(OhttpError::KeyMismatch)
    ));
    Ok(())
}

/// RFC 9458 Appendix A's key config: the same key offering AES-128-GCM and ChaCha20-Poly1305.
const RFC_CONFIG: &str =
    "01002031e1f05a740102115220e9af918f738674aec95f54db6e04eb705aae8e79815500080001000100010003";

#[test]
fn config_with_extra_suites_is_accepted_and_kept_byte_for_byte() -> Result<(), TestError> {
    let encoded = hex::decode(RFC_CONFIG)?;
    let config = KeyConfig::decode(&encoded)?;
    assert_eq!(config.encoded(), encoded.as_slice());
    assert_eq!(config.key_id(), 1);
    // The pin covers the encoding as served, so it differs from our single-suite encoding of
    // the same key.
    assert_ne!(config.hash(), gateway_key(1, SECRET)?.config().hash());
    Ok(())
}

#[test]
fn requests_to_a_config_with_extra_suites_still_use_aes_gcm() -> Result<(), TestError> {
    let config = KeyConfig::decode(&hex::decode(RFC_CONFIG)?)?;
    let encapsulated = encapsulate_request(&config, &request(), &mut seeded_rng(17))?;
    let gateway = Gateway::new(vec![gateway_key(1, SECRET)?]);
    assert_eq!(
        gateway.decapsulate(&encapsulated.message)?.request,
        request()
    );
    Ok(())
}

#[test]
fn malformed_or_unsupported_configs_are_refused() -> Result<(), TestError> {
    let good = hex::decode(RFC_CONFIG)?;
    let mut cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        good.get(..40).unwrap_or_default().to_vec(), // cut inside the suite list
        good.get(..35).unwrap_or_default().to_vec(), // no suite list length
        [good.as_slice(), &[0]].concat(),            // trailing byte
    ];
    let mut set = |index: usize, value: u8| {
        let mut changed = good.clone();
        if let Some(byte) = changed.get_mut(index) {
            *byte = value;
        }
        cases.push(changed);
    };
    set(2, 0x10); // P-256 KEM
    set(36, 0x00); // suite list length 0, so the suites become trailing bytes
    set(36, 0x07); // suite list length not a multiple of four
    set(38, 0x02); // HKDF-SHA384 with AES-128-GCM, leaving no supported suite
    set(40, 0x02); // AES-256-GCM, leaving no supported suite
    for case in &cases {
        assert!(
            matches!(KeyConfig::decode(case), Err(OhttpError::BadKeyConfig)),
            "{}",
            hex::encode(case)
        );
    }
    Ok(())
}

#[test]
fn config_offering_only_chacha_is_refused() -> Result<(), TestError> {
    let chacha_only = hex::decode(
        "01002031e1f05a740102115220e9af918f738674aec95f54db6e04eb705aae8e7981550004\
         00010003",
    )?;
    assert!(matches!(
        KeyConfig::decode(&chacha_only),
        Err(OhttpError::BadKeyConfig)
    ));
    Ok(())
}

#[test]
fn config_offering_aes_gcm_second_is_accepted() -> Result<(), TestError> {
    let aes_second = hex::decode(
        "01002031e1f05a740102115220e9af918f738674aec95f54db6e04eb705aae8e7981550008\
         0001000300010001",
    )?;
    assert_eq!(KeyConfig::decode(&aes_second)?.key_id(), 1);
    Ok(())
}

#[test]
fn from_bytes_derives_the_public_key() -> Result<(), TestError> {
    // RFC 9458 Appendix A: this secret key's public key is 31e1f05a...8155.
    let config = gateway_key(1, SECRET)?.config();
    assert_eq!(
        config.encoded().get(3..35),
        hex::decode(RFC_CONFIG)?.get(3..35)
    );
    Ok(())
}
