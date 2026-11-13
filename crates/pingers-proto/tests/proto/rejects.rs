// SPDX-License-Identifier: AGPL-3.0-only
//! Malformed input is rejected with the error variant that names what is wrong.
//!
//! Inputs are built by hand with the small CBOR writer below, independent of the crate's own
//! encoder, so a bug shared by encoder and decoder still shows up here.

use pingers_proto::{
    AppMessage, BoardCursor, BoardPage, BoardSyncRequest, CommitResponse, CoverReadRatio,
    GroupLogEntry, Knock, LocationPlaintext, MailboxMessages, MailboxSpec, MeetupMessage,
    MembershipLeaves, Params, ProtoError, Record, RegisterRequest, Slashing, WireDecode,
    WireEncode,
};
use pingers_time::UnixSeconds;

const NULL: u8 = 0xf6;

fn head(major: u8, value: usize) -> Vec<u8> {
    if let Ok(small) = u8::try_from(value) {
        if small < 24 {
            return vec![major | small];
        }
        return vec![major | 0x18, small];
    }
    let medium = u16::try_from(value).unwrap_or(u16::MAX);
    let mut bytes = vec![major | 0x19];
    bytes.extend_from_slice(&medium.to_be_bytes());
    bytes
}

fn uint(value: usize) -> Vec<u8> {
    head(0x00, value)
}

fn bstr(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = head(0x40, bytes.len());
    encoded.extend_from_slice(bytes);
    encoded
}

fn tstr(text: &[u8]) -> Vec<u8> {
    let mut encoded = head(0x60, text.len());
    encoded.extend_from_slice(text);
    encoded
}

fn array(items: &[Vec<u8>]) -> Vec<u8> {
    let mut encoded = head(0x80, items.len());
    for item in items {
        encoded.extend_from_slice(item);
    }
    encoded
}

fn versioned(body: Vec<u8>) -> Vec<u8> {
    array(&[uint(1), body])
}

fn null() -> Vec<u8> {
    vec![NULL]
}

fn float(value: f32) -> Vec<u8> {
    let mut encoded = vec![0xfa];
    encoded.extend_from_slice(&value.to_be_bytes());
    encoded
}

fn keep_alive() -> Vec<u8> {
    versioned(array(&[uint(7)]))
}

fn chat(id: &[u8], body: &[u8]) -> Vec<u8> {
    versioned(array(&[uint(3), bstr(id), null(), tstr(body), uint(5)]))
}

fn location(latitude: Vec<u8>, heading: Vec<u8>) -> Vec<u8> {
    versioned(array(&[
        latitude,
        uint(0),
        uint(10),
        heading,
        null(),
        uint(0),
    ]))
}

fn sealed_push(relay: Vec<u8>, urgency: usize) -> Vec<u8> {
    array(&[
        relay,
        uint(1),
        bstr(&[0; 320]),
        uint(urgency),
        bstr(&[0; 8]),
    ])
}

fn mailbox_spec(ttl: usize, push: Vec<u8>) -> Vec<u8> {
    versioned(array(&[bstr(&[0; 16]), uint(ttl), push]))
}

fn params(ratio: Vec<u8>, slashing: usize) -> Vec<u8> {
    versioned(array(&[
        array(&[]),
        uint(slashing),
        ratio,
        uint(100),
        array(&[]),
        array(&[]),
        uint(0),
        tstr(b"op"),
        uint(0),
    ]))
}

fn segment(record_count: usize) -> Vec<u8> {
    array(&[uint(1), uint(record_count), null()])
}

fn page(segments: &[Vec<u8>], records: &[u8]) -> Vec<u8> {
    versioned(array(&[array(segments), bstr(records), null()]))
}

#[test]
fn hand_built_keep_alive_decodes() -> Result<(), ProtoError> {
    assert_eq!(
        MeetupMessage::decode(&keep_alive())?,
        MeetupMessage::KeepAlive
    );
    assert_eq!(MeetupMessage::KeepAlive.encode()?, keep_alive());
    Ok(())
}

#[test]
fn unknown_version_is_rejected() {
    let bytes = array(&[uint(2), array(&[uint(7)])]);
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::UnsupportedVersion { version: 2 })
    ));
}

#[test]
fn version_zero_is_rejected() {
    let bytes = array(&[uint(0), array(&[uint(7)])]);
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::UnsupportedVersion { version: 0 })
    ));
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut bytes = keep_alive();
    bytes.push(0);
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::TrailingBytes)
    ));
}

#[test]
fn long_form_version_is_not_canonical() {
    let bytes = [0x82, 0x18, 0x01, 0x81, 0x07];
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::NonCanonical)
    ));
}

#[test]
fn long_form_integer_in_the_body_is_not_canonical() {
    let bytes = [0x82, 0x01, 0x81, 0x18, 0x07];
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::NonCanonical)
    ));
}

#[test]
fn indefinite_length_array_is_not_canonical() {
    let bytes = [0x9f, 0x01, 0x81, 0x07, 0xff];
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::NonCanonical)
    ));
}

#[test]
fn empty_input_is_malformed_cbor() {
    assert!(matches!(
        MeetupMessage::decode(&[]),
        Err(ProtoError::Cbor(_))
    ));
}

#[test]
fn top_level_must_be_a_pair() {
    let bytes = array(&[uint(1), array(&[uint(7)]), uint(0)]);
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::Cbor(_))
    ));
}

#[test]
fn unknown_meetup_message_kind_is_rejected() {
    let bytes = versioned(array(&[uint(8)]));
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "meetup message kind"
        })
    ));
}

#[test]
fn meetup_message_with_extra_field_is_rejected() {
    let bytes = versioned(array(&[uint(7), uint(0)]));
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::Cbor(_))
    ));
}

#[test]
fn every_meetup_kind_checks_its_field_count() {
    for kind in 0..=7 {
        let bytes = versioned(array(&[
            uint(kind),
            uint(0),
            uint(0),
            uint(0),
            uint(0),
            uint(0),
        ]));
        assert!(
            matches!(MeetupMessage::decode(&bytes), Err(ProtoError::Cbor(_))),
            "kind {kind}"
        );
    }
}

#[test]
fn chat_with_short_id_is_rejected() {
    assert!(matches!(
        MeetupMessage::decode(&chat(&[0; 15], b"hi")),
        Err(ProtoError::WrongLength {
            expected: 16,
            actual: 15
        })
    ));
}

#[test]
fn hand_built_chat_decodes() -> Result<(), ProtoError> {
    let decoded = MeetupMessage::decode(&chat(&[0; 16], b"hi"))?;
    assert!(matches!(
        decoded,
        MeetupMessage::Chat { ref body, written_at, .. }
            if body.as_str() == "hi" && written_at == UnixSeconds::new(5)
    ));
    Ok(())
}

#[test]
fn empty_chat_body_is_rejected() {
    assert!(matches!(
        MeetupMessage::decode(&chat(&[0; 16], b"")),
        Err(ProtoError::OutOfRange { field: "chat body" })
    ));
}

#[test]
fn chat_body_over_the_limit_is_rejected() {
    assert!(matches!(
        MeetupMessage::decode(&chat(&[0; 16], &[b'a'; 3_501])),
        Err(ProtoError::OutOfRange { field: "chat body" })
    ));
}

#[test]
fn chat_body_that_is_not_utf8_is_rejected() {
    assert!(matches!(
        MeetupMessage::decode(&chat(&[0; 16], &[0xff, 0xfe])),
        Err(ProtoError::Cbor(_))
    ));
}

#[test]
fn meet_point_label_over_the_limit_is_rejected() {
    let bytes = versioned(array(&[
        uint(4),
        array(&[uint(0), uint(0), uint(0), tstr(&[b'a'; 121]), uint(1)]),
    ]));
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "meet point label"
        })
    ));
}

#[test]
fn unknown_meet_point_kind_is_rejected() {
    let bytes = versioned(array(&[uint(4), array(&[uint(3), uint(1)])]));
    assert!(matches!(
        MeetupMessage::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "meet point kind"
        })
    ));
}

#[test]
fn every_meet_point_kind_checks_its_field_count() {
    for kind in 0..=2 {
        let bytes = versioned(array(&[
            uint(4),
            array(&[uint(kind), uint(0), uint(0), uint(0), uint(0), uint(0)]),
        ]));
        assert!(
            matches!(MeetupMessage::decode(&bytes), Err(ProtoError::Cbor(_))),
            "kind {kind}"
        );
    }
}

#[test]
fn latitude_beyond_the_pole_is_rejected() {
    // 900_000_001 as a four-byte unsigned integer.
    let latitude = vec![0x1a, 0x35, 0xa4, 0xe9, 0x01];
    assert!(matches!(
        LocationPlaintext::decode(&location(latitude, null())),
        Err(ProtoError::OutOfRange { field: "latitude" })
    ));
}

#[test]
fn heading_of_360_is_rejected() {
    assert!(matches!(
        LocationPlaintext::decode(&location(uint(0), uint(360))),
        Err(ProtoError::OutOfRange { field: "heading" })
    ));
}

#[test]
fn hand_built_location_decodes() -> Result<(), ProtoError> {
    // -1 latitude is the single byte 0x20.
    let location = LocationPlaintext::decode(&location(vec![0x20], uint(359)))?;
    assert_eq!(location.position.latitude_e7(), -1);
    assert_eq!(location.accuracy.get(), 10);
    assert_eq!(
        location.heading.map(pingers_proto::HeadingDegrees::get),
        Some(359)
    );
    Ok(())
}

#[test]
fn zero_time_to_live_is_rejected() {
    assert!(matches!(
        MailboxSpec::decode(&mailbox_spec(0, null())),
        Err(ProtoError::OutOfRange { field: "ttl" })
    ));
}

#[test]
fn custom_relay_must_be_https() {
    let relay = array(&[uint(1), tstr(b"http://relay.example"), bstr(&[0; 32])]);
    assert!(matches!(
        MailboxSpec::decode(&mailbox_spec(60, sealed_push(relay, 0))),
        Err(ProtoError::OutOfRange { field: "relay url" })
    ));
}

#[test]
fn unknown_relay_kind_is_rejected() {
    let relay = array(&[uint(2), uint(0)]);
    assert!(matches!(
        MailboxSpec::decode(&mailbox_spec(60, sealed_push(relay, 0))),
        Err(ProtoError::OutOfRange { field: "relay ref" })
    ));
}

#[test]
fn relay_kinds_check_their_field_count() {
    for relay in [
        array(&[uint(0), uint(1), uint(2)]),
        array(&[uint(1), tstr(b"https://relay.example")]),
    ] {
        assert!(matches!(
            MailboxSpec::decode(&mailbox_spec(60, sealed_push(relay, 0))),
            Err(ProtoError::Cbor(_))
        ));
    }
}

#[test]
fn unknown_urgency_is_rejected() {
    let relay = array(&[uint(0), uint(3)]);
    assert!(matches!(
        MailboxSpec::decode(&mailbox_spec(60, sealed_push(relay, 2))),
        Err(ProtoError::OutOfRange { field: "urgency" })
    ));
}

#[test]
fn short_sealed_push_blob_is_rejected() {
    let push = array(&[
        array(&[uint(0), uint(3)]),
        uint(1),
        bstr(&[0; 319]),
        uint(0),
        bstr(&[0; 8]),
    ]);
    assert!(matches!(
        MailboxSpec::decode(&mailbox_spec(60, push)),
        Err(ProtoError::WrongLength {
            expected: 320,
            actual: 319
        })
    ));
}

#[test]
fn hand_built_mailbox_spec_decodes() -> Result<(), ProtoError> {
    let spec = MailboxSpec::decode(&mailbox_spec(
        60,
        sealed_push(array(&[uint(0), uint(3)]), 1),
    ))?;
    assert_eq!(spec.time_to_live.duration().as_secs(), 60);
    assert!(spec.push.is_some());
    Ok(())
}

#[test]
fn page_whose_counts_disagree_with_its_records_is_rejected() {
    let bytes = page(&[segment(2)], &[0; 96]);
    assert!(matches!(
        BoardPage::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "segment record count"
        })
    ));
}

#[test]
fn page_whose_counts_disagree_is_not_encoded() {
    let page = BoardPage {
        segments: Vec::new(),
        records: vec![Record::from_bytes(&[0; 96])],
        next: None,
    };
    assert!(matches!(
        page.encode(),
        Err(ProtoError::OutOfRange {
            field: "segment record count"
        })
    ));
}

#[test]
fn page_over_the_record_limit_is_not_encoded() -> Result<(), ProtoError> {
    let count = BoardPage::MAX_RECORDS.saturating_add(1);
    let page = BoardPage {
        segments: vec![pingers_proto::SegmentInfo {
            start: pingers_time::Bucket::from_index(1).map_err(ProtoError::Bucket)?,
            record_count: u32::try_from(count).unwrap_or(u32::MAX),
            closed_hash: None,
        }],
        records: vec![Record::from_bytes(&[0; 96]); count],
        next: None,
    };
    assert!(matches!(
        page.encode(),
        Err(ProtoError::OutOfRange {
            field: "board page records"
        })
    ));
    Ok(())
}

#[test]
fn page_at_the_record_limit_round_trips() -> Result<(), ProtoError> {
    let count = BoardPage::MAX_RECORDS;
    let page = BoardPage {
        segments: vec![pingers_proto::SegmentInfo {
            start: pingers_time::Bucket::from_index(1).map_err(ProtoError::Bucket)?,
            record_count: u32::try_from(count).unwrap_or(0),
            closed_hash: None,
        }],
        records: vec![Record::from_bytes(&[7; 96]); count],
        next: Some(BoardCursor::from_bytes(vec![1, 2, 3])),
    };
    assert_eq!(BoardPage::decode(&page.encode()?)?, page);
    Ok(())
}

#[test]
fn page_with_partial_record_is_rejected() {
    let bytes = page(&[segment(1)], &[0; 95]);
    assert!(matches!(
        BoardPage::decode(&bytes),
        Err(ProtoError::WrongLength {
            expected: 96,
            actual: 95
        })
    ));
}

#[test]
fn segment_bucket_beyond_range_is_rejected() {
    let mut huge = vec![0x1b];
    huge.extend_from_slice(&u64::MAX.to_be_bytes());
    let segment = array(&[huge, uint(0), null()]);
    assert!(matches!(
        BoardPage::decode(&page(&[segment], &[])),
        Err(ProtoError::Bucket(_))
    ));
}

#[test]
fn board_sync_with_short_lease_is_rejected() {
    let bytes = versioned(array(&[bstr(&[0; 423]), null(), uint(1)]));
    assert!(matches!(
        BoardSyncRequest::decode(&bytes),
        Err(ProtoError::WrongLength {
            expected: 424,
            actual: 423
        })
    ));
}

#[test]
fn unknown_read_board_value_is_rejected() {
    let bytes = versioned(array(&[bstr(&[0; 424]), null(), uint(2)]));
    assert!(matches!(
        BoardSyncRequest::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "read board"
        })
    ));
}

#[test]
fn cover_read_ratio_above_one_is_rejected() {
    assert!(matches!(
        Params::decode(&params(float(1.5), 0)),
        Err(ProtoError::OutOfRange {
            field: "cover read ratio"
        })
    ));
}

#[test]
fn cover_read_ratio_nan_is_rejected() {
    assert!(matches!(
        Params::decode(&params(float(f32::NAN), 0)),
        Err(ProtoError::OutOfRange {
            field: "cover read ratio"
        })
    ));
}

#[test]
fn negative_zero_ratio_is_not_canonical() {
    assert!(matches!(
        Params::decode(&params(float(-0.0), 0)),
        Err(ProtoError::NonCanonical)
    ));
}

#[test]
fn negative_zero_ratio_is_encoded_as_zero() -> Result<(), ProtoError> {
    let mut decoded = Params::decode(&params(float(0.0), 1))?;
    assert_eq!(decoded.rln_slashing, Slashing::On);
    decoded.cover_read_ratio = CoverReadRatio::new(-0.0)?;
    assert_eq!(decoded.encode()?, params(float(0.0), 1));
    Ok(())
}

#[test]
fn half_precision_ratio_is_rejected() {
    assert!(matches!(
        Params::decode(&params(vec![0xf9, 0x3c, 0x00], 0)),
        Err(ProtoError::Cbor(_) | ProtoError::NonCanonical)
    ));
}

#[test]
fn unknown_slashing_value_is_rejected() {
    assert!(matches!(
        Params::decode(&params(float(1.0), 2)),
        Err(ProtoError::OutOfRange { field: "slashing" })
    ));
}

#[test]
fn unknown_credential_and_custom_relay_values_are_rejected() {
    let unknown_credential = versioned(array(&[
        array(&[uint(2)]),
        uint(0),
        float(1.0),
        uint(0),
        array(&[]),
        array(&[]),
        uint(0),
        tstr(b""),
        uint(0),
    ]));
    assert!(matches!(
        Params::decode(&unknown_credential),
        Err(ProtoError::OutOfRange {
            field: "anonymous credential"
        })
    ));
    let unknown_custom = versioned(array(&[
        array(&[]),
        uint(0),
        float(1.0),
        uint(0),
        array(&[]),
        array(&[]),
        uint(2),
        tstr(b""),
        uint(0),
    ]));
    assert!(matches!(
        Params::decode(&unknown_custom),
        Err(ProtoError::OutOfRange {
            field: "custom push relays"
        })
    ));
}

#[test]
fn unknown_commit_response_is_rejected() {
    let bytes = versioned(array(&[uint(2), uint(0)]));
    assert!(matches!(
        CommitResponse::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "commit response"
        })
    ));
}

#[test]
fn content_type_zero_is_rejected() {
    let bytes = versioned(array(&[uint(1), uint(1), uint(0), bstr(&[])]));
    assert!(matches!(
        GroupLogEntry::decode(&bytes),
        Err(ProtoError::OutOfRange {
            field: "content type"
        })
    ));
}

#[test]
fn unknown_delivery_is_rejected() {
    let bytes = versioned(array(&[uint(1), bstr(&[]), uint(2), null()]));
    assert!(matches!(
        AppMessage::decode(&bytes),
        Err(ProtoError::OutOfRange { field: "delivery" })
    ));
}

#[test]
fn knock_with_short_signature_is_rejected() {
    let bytes = versioned(array(&[
        bstr(&[0; 16]),
        null(),
        uint(0),
        uint(0),
        bstr(&[]),
        bstr(&[0; 16]),
        null(),
        bstr(&[0; 63]),
    ]));
    assert!(matches!(
        Knock::decode(&bytes),
        Err(ProtoError::WrongLength {
            expected: 64,
            actual: 63
        })
    ));
}

#[test]
fn registration_code_must_be_text() {
    let bytes = versioned(array(&[bstr(&[0; 32]), bstr(&[0; 64]), bstr(b"code")]));
    assert!(matches!(
        RegisterRequest::decode(&bytes),
        Err(ProtoError::Cbor(_))
    ));
}

#[test]
fn mailbox_message_must_be_padded_to_a_class() {
    let bytes = versioned(array(&[array(&[array(&[uint(1), bstr(&[0; 300])])])]));
    assert!(matches!(
        MailboxMessages::decode(&bytes),
        Err(ProtoError::NotASizeClass { length: 300 })
    ));
}

#[test]
fn membership_root_with_short_hash_is_rejected() {
    let bytes = versioned(array(&[
        array(&[]),
        array(&[]),
        array(&[array(&[uint(1), bstr(&[0; 31])])]),
        uint(0),
    ]));
    assert!(matches!(
        MembershipLeaves::decode(&bytes),
        Err(ProtoError::WrongLength {
            expected: 32,
            actual: 31
        })
    ));
}

#[test]
fn huge_declared_list_length_fails_without_allocating() {
    // A message list claiming 2^32 - 1 elements, followed by nothing.
    let bytes = versioned(array(&[vec![0x9a, 0xff, 0xff, 0xff, 0xff]]));
    assert!(matches!(
        MailboxMessages::decode(&bytes),
        Err(ProtoError::Cbor(_))
    ));
}
