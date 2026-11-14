// SPDX-License-Identifier: AGPL-3.0-only
//! `decode(encode(value)) == value` for every type, and every encoding is the only one: any
//! corruption of a valid encoding either fails to decode or decodes to a value whose
//! encoding is exactly the corrupted bytes.

use core::fmt::Debug;

use pingers_proto::{
    AppMessage, AuthRequest, AuthResponse, BoardPage, BoardSyncRequest, BoardSyncResponse,
    CommitRequest, CommitResponse, CreateGroup, GroupLogEntry, GroupMessages, GroupState,
    InvitePayload, InvitePost, JoinRequest, Knock, Lease, LiveFrame, LocationPlaintext,
    MailboxMessages, MailboxPush, MailboxSpec, MeetupMessage, MembershipLeafRequest,
    MembershipLeafResponse, MembershipLeaves, MessageAccepted, Params, PrivacyPassAllowance,
    PrivacyPassIssueRequest, PrivacyPassIssueResponse, PushRelayConfig, PushRequest,
    PushTokenPlaintext, Record, RegisterRequest, RegisterResponse, RlnAuth,
    ScheduledPushRelayConfig, Subscription, WireDecode, WireEncode,
};
use proptest::prelude::*;
use proptest::sample::Index;

use crate::strategies;

/// How a valid encoding is corrupted.
#[derive(Clone, Debug)]
enum Corruption {
    /// Replace one byte.
    Replace(Index, u8),
    /// Cut the encoding short.
    Truncate(Index),
    /// Append bytes.
    Extend(Vec<u8>),
    /// Insert one byte.
    Insert(Index, u8),
}

fn corruption() -> impl Strategy<Value = Corruption> {
    prop_oneof![
        (any::<Index>(), any::<u8>()).prop_map(|(at, byte)| Corruption::Replace(at, byte)),
        any::<Index>().prop_map(Corruption::Truncate),
        proptest::collection::vec(any::<u8>(), 1..4).prop_map(Corruption::Extend),
        (any::<Index>(), any::<u8>()).prop_map(|(at, byte)| Corruption::Insert(at, byte)),
    ]
}

fn corrupt(mut bytes: Vec<u8>, corruption: &Corruption) -> Vec<u8> {
    match corruption {
        Corruption::Replace(at, byte) if !bytes.is_empty() => {
            let position = at.index(bytes.len());
            if let Some(target) = bytes.get_mut(position) {
                *target = *byte;
            }
        }
        Corruption::Truncate(at) if !bytes.is_empty() => {
            bytes.truncate(at.index(bytes.len()));
        }
        Corruption::Extend(extra) => bytes.extend_from_slice(extra),
        Corruption::Insert(at, byte) => {
            let position = at.index(bytes.len().saturating_add(1));
            bytes.insert(position, *byte);
        }
        Corruption::Replace(..) | Corruption::Truncate(_) => {}
    }
    bytes
}

fn check_round_trip<T>(value: &T) -> Result<(), TestCaseError>
where
    T: WireEncode + WireDecode + PartialEq + Debug,
{
    let bytes = value
        .encode()
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    let decoded = T::decode(&bytes).map_err(|error| TestCaseError::fail(error.to_string()))?;
    prop_assert_eq!(&decoded, value);
    Ok(())
}

fn check_corruption<T>(value: &T, corruption: &Corruption) -> Result<(), TestCaseError>
where
    T: WireEncode + WireDecode + PartialEq + Debug,
{
    let bytes = value
        .encode()
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    let corrupted = corrupt(bytes, corruption);
    if let Ok(decoded) = T::decode(&corrupted) {
        let encoded = decoded
            .encode()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(encoded, corrupted);
    }
    Ok(())
}

/// One round-trip and one corruption property per type.
macro_rules! wire_properties {
    ($($module:ident: $type:ty = $strategy:expr;)*) => {
        $(
            mod $module {
                use super::*;

                proptest! {
                    #[test]
                    fn decode_of_encode_is_identity(value in $strategy) {
                        check_round_trip::<$type>(&value)?;
                    }

                    #[test]
                    fn corrupted_encoding_is_rejected_or_canonical(
                        value in $strategy,
                        corruption in corruption(),
                    ) {
                        check_corruption::<$type>(&value, &corruption)?;
                    }

                    #[test]
                    fn garbage_is_rejected_or_canonical(
                        bytes in proptest::collection::vec(any::<u8>(), 0..64),
                    ) {
                        if let Ok(decoded) = <$type>::decode(&bytes) {
                            let encoded = decoded
                                .encode()
                                .map_err(|error| TestCaseError::fail(error.to_string()))?;
                            prop_assert_eq!(encoded, bytes);
                        }
                    }
                }
            }
        )*
    };
}

wire_properties! {
    board_page: BoardPage = strategies::board_page();
    knock: Knock = strategies::knock();
    mailbox_spec: MailboxSpec = strategies::mailbox_spec();
    mailbox_push: MailboxPush = strategies::mailbox_push();
    subscription: Subscription = strategies::subscription();
    push_request: PushRequest = strategies::push_request();
    push_relay_config: PushRelayConfig = strategies::push_relay_config();
    scheduled_push_relay_config: ScheduledPushRelayConfig =
        strategies::scheduled_push_relay_config();
    invite_payload: InvitePayload = strategies::invite_payload();
    join_request: JoinRequest = strategies::join_request();
    invite_post: InvitePost = strategies::invite_post();
    privacy_pass_issue_request: PrivacyPassIssueRequest = strategies::privacy_pass_issue_request();
    privacy_pass_issue_response: PrivacyPassIssueResponse =
        strategies::privacy_pass_issue_response();
    privacy_pass_allowance: PrivacyPassAllowance = strategies::privacy_pass_allowance();
    create_group: CreateGroup = strategies::create_group();
    commit_request: CommitRequest = strategies::commit_request();
    commit_response: CommitResponse = strategies::commit_response();
    app_message: AppMessage = strategies::app_message();
    group_log_entry: GroupLogEntry = strategies::group_log_entry();
    live_frame: LiveFrame = strategies::live_frame();
    meetup_message: MeetupMessage = strategies::meetup_message();
    location_plaintext: LocationPlaintext = strategies::location_plaintext();
    rln_auth: RlnAuth = strategies::rln_auth();
    params: Params = strategies::params();
    register_request: RegisterRequest = strategies::register_request();
    register_response: RegisterResponse = strategies::register_response();
    auth_request: AuthRequest = strategies::auth_request();
    auth_response: AuthResponse = strategies::auth_response();
    membership_leaf_request: MembershipLeafRequest = strategies::membership_leaf_request();
    membership_leaf_response: MembershipLeafResponse = strategies::membership_leaf_response();
    membership_leaves: MembershipLeaves = strategies::membership_leaves();
    board_sync_request: BoardSyncRequest = strategies::board_sync_request();
    board_sync_response: BoardSyncResponse = strategies::board_sync_response();
    mailbox_messages: MailboxMessages = strategies::mailbox_messages();
    message_accepted: MessageAccepted = strategies::message_accepted();
    group_messages: GroupMessages = strategies::group_messages();
    group_state: GroupState = strategies::group_state();
}

proptest! {
    #[test]
    fn record_bytes_round_trip(record in strategies::record()) {
        prop_assert_eq!(Record::from_bytes(&record.to_bytes()), record);
    }

    #[test]
    fn concatenated_records_round_trip(records in proptest::collection::vec(strategies::record(), 0..8)) {
        let bytes: Vec<u8> = records.iter().flat_map(Record::to_bytes).collect();
        let parsed = Record::parse_concatenated(&bytes)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(parsed, records);
    }

    #[test]
    fn lease_bytes_round_trip(lease in strategies::lease()) {
        let parsed = Lease::from_bytes(&lease.to_bytes())
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(parsed, lease);
    }

    #[test]
    fn push_token_plaintext_round_trips(plaintext in strategies::push_token_plaintext()) {
        let bytes = plaintext
            .encode_padded()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let parsed = PushTokenPlaintext::decode_padded(&bytes)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(parsed, plaintext);
    }

    #[test]
    fn push_token_plaintext_decodes_only_its_own_encoding(
        bytes in proptest::array::uniform::<_, 272>(any::<u8>()),
    ) {
        if let Ok(parsed) = PushTokenPlaintext::decode_padded(&bytes) {
            let encoded = parsed
                .encode_padded()
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(encoded, bytes);
        }
    }
}
