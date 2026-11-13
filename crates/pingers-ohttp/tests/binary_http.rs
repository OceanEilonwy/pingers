// SPDX-License-Identifier: AGPL-3.0-only
//! Binary HTTP (RFC 9292) encoding of inner requests and responses.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod common;

use common::{TestError, request, response};
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, OhttpError, StatusCode};
use proptest::prelude::*;

fn bare_response(code: u16) -> Result<BinaryResponse, OhttpError> {
    Ok(BinaryResponse {
        status: StatusCode::new(code)?,
        headers: Vec::new(),
        body: Vec::new(),
    })
}

#[test]
fn status_codes_outside_100_to_599_are_refused() {
    assert!(StatusCode::new(99).is_err());
    assert!(StatusCode::new(600).is_err());
    assert!(matches!(StatusCode::new(100), Ok(code) if code.get() == 100));
    assert!(matches!(StatusCode::new(599), Ok(code) if code.get() == 599));
}

#[test]
fn debug_prints_neither_paths_nor_bodies_nor_header_values() -> Result<(), TestError> {
    assert_eq!(
        format!("{:?}", common::live_request()),
        "BinaryRequest { method: \"POST\", headers: [Header { name: \"authorization\", .. }], \
         body_length: 0, .. }"
    );
    assert_eq!(
        format!("{:?}", response()?),
        "BinaryResponse { status: StatusCode(200), headers: [Header { name: \"content-type\", .. }], \
         body_length: 1, .. }"
    );
    Ok(())
}

#[test]
fn request_encoding_is_known_length_with_https_scheme_and_no_authority() -> Result<(), TestError> {
    let encoded = request().encode()?;
    let expected = [
        &[0x00, 4][..],
        b"POST",
        &[5],
        b"https",
        &[0],
        &[14],
        b"/v1/board/sync",
        &[37, 13],
        b"authorization",
        &[22],
        b"PrivateToken token=abc",
        &[3, 0xa1, 0x01, 0x02],
    ]
    .concat();
    assert_eq!(encoded, expected);
    Ok(())
}

#[test]
fn trailing_empty_sections_are_omitted() -> Result<(), TestError> {
    // RFC 9292 §3.8: empty trailers always, then an empty body, then empty headers.
    let mut with_headers = response()?;
    with_headers.body.clear();
    let encoded = with_headers.encode()?;
    assert_eq!(encoded.get(..3), Some([0x01, 0x40, 0xc8].as_slice()));
    assert_eq!(encoded.len(), 3 + 1 + 1 + 12 + 1 + 16);
    let mut with_body = bare_response(204)?;
    with_body.body = vec![0];
    assert_eq!(with_body.encode()?, [0x01, 0x40, 0xcc, 0x00, 0x01, 0x00]);
    assert_eq!(bare_response(200)?.encode()?, [0x01, 0x40, 0xc8]);
    Ok(())
}

#[test]
fn decoding_accepts_untruncated_form_and_zero_padding() -> Result<(), TestError> {
    let expected = bare_response(200)?;
    assert_eq!(
        BinaryResponse::decode(&[0x01, 0x40, 0xc8, 0, 0, 0])?,
        expected
    );
    assert_eq!(
        BinaryResponse::decode(&[0x01, 0x40, 0xc8, 0, 0, 0, 0, 0])?,
        expected
    );
    Ok(())
}

#[test]
fn decoding_refuses_non_zero_bytes_after_the_message() {
    assert!(matches!(
        BinaryResponse::decode(&[0x01, 0x40, 0xc8, 0, 0, 0, 1]),
        Err(OhttpError::BadMessage)
    ));
}

#[test]
fn a_request_is_not_a_response_and_the_reverse() -> Result<(), TestError> {
    assert!(matches!(
        BinaryResponse::decode(&request().encode()?),
        Err(OhttpError::BadMessage)
    ));
    assert!(matches!(
        BinaryRequest::decode(&response()?.encode()?),
        Err(OhttpError::BadMessage)
    ));
    Ok(())
}

#[test]
fn informational_responses_are_refused() -> Result<(), TestError> {
    assert!(matches!(
        bare_response(103)?.encode(),
        Err(OhttpError::BadMessage)
    ));
    // 103 with no fields, then 200: valid RFC 9292, but Pingers never sends it.
    assert!(matches!(
        BinaryResponse::decode(&[0x01, 0x40, 0x67, 0x00, 0x40, 0xc8]),
        Err(OhttpError::BadMessage)
    ));
    Ok(())
}

#[test]
fn trailers_are_refused() {
    // 200, no headers, empty body, one trailer "a: b".
    assert!(matches!(
        BinaryResponse::decode(&[0x01, 0x40, 0xc8, 0x00, 0x00, 0x04, 0x01, b'a', 0x01, b'b']),
        Err(OhttpError::BadMessage)
    ));
}

#[test]
fn header_names_must_be_lower_case_and_non_empty() {
    for name in ["Authorization", ""] {
        let mut bad = request();
        bad.headers = vec![Header {
            name: name.to_owned(),
            value: Vec::new(),
        }];
        assert!(
            matches!(bad.encode(), Err(OhttpError::BadMessage)),
            "{name:?}"
        );
    }
    // The same rule on decode: a request with header "A: b".
    let upper = [
        &[0x00, 3][..],
        b"GET",
        &[0, 0, 1],
        b"/",
        &[4, 1],
        b"A",
        &[1],
        b"b",
    ]
    .concat();
    assert!(matches!(
        BinaryRequest::decode(&upper),
        Err(OhttpError::BadMessage)
    ));
}

#[test]
fn request_lines_need_a_method_and_an_absolute_path() {
    let mut no_method = request();
    no_method.method.clear();
    let mut relative = request();
    relative.path = "v1/board".to_owned();
    for bad in [no_method, relative] {
        assert!(matches!(bad.encode(), Err(OhttpError::BadMessage)));
    }
    // Decoding enforces the same: GET with an empty path; then an empty method.
    for bytes in [
        [&[0x00, 3][..], b"GET", &[0, 0, 0]].concat(),
        [&[0x00, 0, 0, 0, 1][..], b"/"].concat(),
    ] {
        assert!(matches!(
            BinaryRequest::decode(&bytes),
            Err(OhttpError::BadMessage)
        ));
    }
}

#[test]
fn non_utf8_names_are_refused() {
    let bytes = [&[0x00, 1][..], &[0xff], &[0, 0, 1], b"/"].concat();
    assert!(matches!(
        BinaryRequest::decode(&bytes),
        Err(OhttpError::BadMessage)
    ));
}

fn header_strategy() -> impl Strategy<Value = Header> {
    (
        "[a-z][a-z0-9-]{0,15}",
        proptest::collection::vec(any::<u8>(), 0..32),
    )
        .prop_map(|(name, value)| Header { name, value })
}

proptest! {
    #[test]
    fn request_decode_of_encode_is_identity(
        method in "[A-Z]{1,8}",
        path in "/[a-z0-9/?=&]{0,40}",
        headers in proptest::collection::vec(header_strategy(), 0..4),
        body in proptest::collection::vec(any::<u8>(), 0..300),
    ) {
        let original = BinaryRequest { method, path, headers, body };
        prop_assert_eq!(BinaryRequest::decode(&original.encode()?)?, original);
    }

    #[test]
    fn response_decode_of_encode_is_identity(
        code in 200_u16..600,
        headers in proptest::collection::vec(header_strategy(), 0..4),
        body in proptest::collection::vec(any::<u8>(), 0..300),
        padding in 0_usize..8,
    ) {
        let original = BinaryResponse { status: StatusCode::new(code)?, headers, body };
        let mut encoded = original.encode()?;
        encoded.extend(std::iter::repeat_n(0, padding));
        prop_assert_eq!(BinaryResponse::decode(&encoded)?, original);
    }
}

#[test]
fn header_debug_shows_the_name_but_never_the_value() {
    let header = Header {
        name: "authorization".to_owned(),
        value: b"PrivateToken token=secret".to_vec(),
    };
    let shown = format!("{header:?}");
    assert!(shown.contains("authorization"));
    assert!(!shown.contains("secret"));
}
