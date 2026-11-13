// SPDX-License-Identifier: AGPL-3.0-only
//! The worked example of RFC 9458 Appendix A, from the gateway's side.
//!
//! The client side cannot be replayed: the RFC gives the ephemeral secret key, while HPKE here
//! derives it from 32 bytes of randomness. The gateway side is deterministic once the response
//! nonce is fixed, so it checks decapsulation and response sealing byte for byte.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod common;

use common::{SECRET, TestError, gateway_key};
use pingers_ohttp::{BinaryRequest, BinaryResponse, Gateway, StatusCode};
use pingers_testkit::FixedRng;

const ENCAPSULATED_REQUEST: &str = "010020000100014b28f881333e7c164ffc499ad9796f877f4e1051ee6d31bad1\
                                    9dec96c208b4726374e469135906992e1268c594d2a10c695d858c40a026e796\
                                    5e7d86b83dd440b2c0185204b4d63525";
const RESPONSE_NONCE: &str = "c789e7151fcba46158ca84b04464910d";
const ENCAPSULATED_RESPONSE: &str =
    "c789e7151fcba46158ca84b04464910d86f9013e404feea014e7be4a441f234f857fbd";

#[test]
fn gateway_reproduces_rfc_9458_appendix_a() -> Result<(), TestError> {
    let gateway = Gateway::new(vec![gateway_key(1, SECRET)?]);
    let decapsulated = gateway.decapsulate(&hex::decode(ENCAPSULATED_REQUEST)?)?;
    // GET https://example.com/ ; the authority is not part of BinaryRequest.
    assert_eq!(
        decapsulated.request,
        BinaryRequest {
            method: "GET".to_owned(),
            path: "/".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    );
    let response = BinaryResponse {
        status: StatusCode::new(200)?,
        headers: Vec::new(),
        body: Vec::new(),
    };
    assert_eq!(response.encode()?, [0x01, 0x40, 0xc8]);
    let mut rng = FixedRng::new(&hex::decode(RESPONSE_NONCE)?);
    let sealed = decapsulated.responder.seal(&response, &mut rng)?;
    assert!(rng.exhausted_exactly());
    assert_eq!(hex::encode(sealed), ENCAPSULATED_RESPONSE);
    Ok(())
}
