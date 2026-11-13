// SPDX-License-Identifier: AGPL-3.0-only
//! Proptest strategies for every wire type, covering the full range each field allows.

use core::time::Duration;

use pingers_proto::{
    AccountId, AccuracyMeters, AnonymousCredential, AppMessage, AuthRequest, AuthResponse,
    BoardCursor, BoardPage, BoardSyncRequest, BoardSyncResponse, ChatBody, ChatMessageId,
    CommitRequest, CommitResponse, ContentType, CoverReadRatio, CreateGroup, CustomPushRelays,
    Delivery, DisplayName, E7Coordinate, Epoch, GroupAuthKey, GroupId, GroupLogEntry,
    GroupMessages, GroupState, HeadingDegrees, HourlyRoot, InvitePayload, InviteUses,
    InviterFingerprint, JoinRequest, KeyConfigHash, KeyPackageBytes, Knock, KnockSignature,
    Lamport, LeafIndex, Lease, LeaseId, LeaseReplacement, LiveFrame, LocationGeneration,
    LocationPlaintext, MailboxId, MailboxMessage, MailboxMessages, MailboxSpec, MapHostUrl,
    MeetPoint, MeetPointLabel, MeetupMessage, MembershipIndex, MembershipLeafRequest,
    MembershipLeafResponse, MembershipLeaves, MessageAccepted, MlsMessageBytes, Padded, Params,
    PrivacyPassIssueRequest, PrivacyPassIssueResponse, PushBind, PushClass, PushHandle,
    PushPlatform, PushRelayConfig, PushRelayInfo, PushRelayKeyConfig, PushRelayKeyId, PushRequest,
    PushToken, PushTokenPlaintext, ReadBoard, Record, RecordRandomness, RecordTag, Redacted,
    RegisterRequest, RegisterResponse, RelayId, RelayRef, RelayUrl, RevokeCommitment, RlnAuth,
    RlnScope, RootEpoch, SealedAsk, SealedBeacon, SealedGrant, SealedLocationFrame, SealedPush,
    SealedPushBlob, SealedSnapshot, SecretBytes, SecretText, SeenSegment, SegmentHash, SegmentInfo,
    SequenceNumber, ServerId, SizeClass, Slashing, SpeedCentimetresPerSecond, Subscription,
    SubscriptionId, TimeToLive, Urgency,
};
use pingers_time::{Bucket, UnixSeconds};
use proptest::array::uniform;
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;

/// The largest bucket index whose start fits in `UnixSeconds`.
const MAX_BUCKET: u64 = u64::MAX / 900;

fn bytes32() -> impl Strategy<Value = [u8; 32]> {
    uniform::<_, 32>(any::<u8>())
}

fn bytes16() -> impl Strategy<Value = [u8; 16]> {
    uniform::<_, 16>(any::<u8>())
}

fn bytes64() -> impl Strategy<Value = [u8; 64]> {
    uniform::<_, 64>(any::<u8>())
}

fn opaque() -> impl Strategy<Value = Vec<u8>> {
    vec(any::<u8>(), 0..300)
}

fn text(max: usize) -> impl Strategy<Value = String> {
    vec(any::<char>(), 0..max).prop_map(|characters| characters.into_iter().collect())
}

pub(crate) fn bucket() -> impl Strategy<Value = Bucket> {
    (0..=MAX_BUCKET).prop_filter_map("bucket in range", |index| Bucket::from_index(index).ok())
}

pub(crate) fn unix_seconds() -> impl Strategy<Value = UnixSeconds> {
    any::<u64>().prop_map(UnixSeconds::new)
}

pub(crate) fn record() -> impl Strategy<Value = Record> {
    (bytes16(), bytes16(), bytes64()).prop_map(|(randomness, tag, sealed)| Record {
        randomness: RecordRandomness::from_bytes(randomness),
        tag: RecordTag::from_bytes(tag),
        sealed: SealedBeacon::from_bytes(sealed),
    })
}

pub(crate) fn lease() -> impl Strategy<Value = Lease> {
    (bucket(), uniform::<_, 4>(record()), bytes32()).prop_map(|(start, records, commitment)| {
        Lease {
            start,
            records,
            revoke_commitment: RevokeCommitment::from_bytes(commitment),
        }
    })
}

pub(crate) fn board_page() -> impl Strategy<Value = BoardPage> {
    let segment = (bucket(), 0_u32..6, option::of(bytes32()));
    (vec(segment, 0..5), option::of(opaque())).prop_flat_map(|(segments, next)| {
        let total: u32 = segments.iter().map(|segment| segment.1).sum();
        let count = usize::try_from(total).unwrap_or(0);
        vec(record(), count..=count).prop_map(move |records| BoardPage {
            segments: segments
                .iter()
                .map(|(start, record_count, hash)| SegmentInfo {
                    start: *start,
                    record_count: *record_count,
                    closed_hash: hash.map(SegmentHash::from_bytes),
                })
                .collect(),
            records,
            next: next.clone().map(BoardCursor::from_bytes),
        })
    })
}

pub(crate) fn time_to_live() -> impl Strategy<Value = TimeToLive> {
    (1..=u32::MAX).prop_filter_map("ttl in range", |seconds| {
        TimeToLive::new(Duration::from_secs(u64::from(seconds))).ok()
    })
}

pub(crate) fn urgency() -> impl Strategy<Value = Urgency> {
    prop_oneof![Just(Urgency::Alert), Just(Urgency::Quiet)]
}

pub(crate) fn relay_url() -> impl Strategy<Value = RelayUrl> {
    text(60).prop_filter_map("https url", |rest| {
        RelayUrl::new(format!("https://{rest}")).ok()
    })
}

pub(crate) fn relay_ref() -> impl Strategy<Value = RelayRef> {
    prop_oneof![
        any::<u16>().prop_map(|id| RelayRef::Listed(RelayId::new(id))),
        (relay_url(), bytes32()).prop_map(|(url, hash)| RelayRef::Custom {
            url,
            key_config_hash: KeyConfigHash::from_bytes(hash),
        }),
    ]
}

pub(crate) fn sealed_push() -> impl Strategy<Value = SealedPush> {
    (
        (relay_ref(), any::<u8>()),
        uniform::<_, 320>(any::<u8>()),
        urgency(),
        uniform::<_, 8>(any::<u8>()),
    )
        .prop_map(|((relay, key_id), blob, urgency, handle)| SealedPush {
            relay,
            key_id: PushRelayKeyId::new(key_id),
            blob: SealedPushBlob::from_bytes(blob),
            urgency,
            handle: PushHandle::from_bytes(handle),
        })
}

pub(crate) fn push_request() -> impl Strategy<Value = PushRequest> {
    (
        any::<u8>(),
        uniform::<_, 320>(any::<u8>()),
        bytes32(),
        urgency(),
        uniform::<_, 8>(any::<u8>()),
    )
        .prop_map(|(key_id, blob, bind, urgency, handle)| PushRequest {
            key_id: PushRelayKeyId::new(key_id),
            blob: SealedPushBlob::from_bytes(blob),
            bind: PushBind::from_bytes(bind),
            urgency,
            handle: PushHandle::from_bytes(handle),
        })
}

pub(crate) fn push_relay_config() -> impl Strategy<Value = PushRelayConfig> {
    (
        any::<u8>(),
        bytes32(),
        vec(
            prop_oneof![Just(PushPlatform::Fcm), Just(PushPlatform::UnifiedPush)],
            0..4,
        ),
        time_to_live(),
        text(40),
    )
        .prop_map(
            |(key_id, public_key, platforms, max_time_to_live, operator)| PushRelayConfig {
                key_config: PushRelayKeyConfig {
                    key_id: PushRelayKeyId::new(key_id),
                    public_key,
                },
                platforms,
                max_time_to_live,
                operator,
            },
        )
}

pub(crate) fn privacy_pass_issue_request() -> impl Strategy<Value = PrivacyPassIssueRequest> {
    vec(any::<u8>(), 1..200).prop_filter_map("fits a batch", |bytes| {
        PrivacyPassIssueRequest::new(bytes).ok()
    })
}

pub(crate) fn privacy_pass_issue_response() -> impl Strategy<Value = PrivacyPassIssueResponse> {
    vec(any::<u8>(), 1..200).prop_filter_map("fits a batch", |bytes| {
        PrivacyPassIssueResponse::new(bytes).ok()
    })
}

pub(crate) fn invite_payload() -> impl Strategy<Value = InvitePayload> {
    (
        text(40).prop_filter_map("https origin", |host| {
            ServerId::new(format!("https://{host}")).ok()
        }),
        bytes16(),
        bytes32(),
        uniform::<_, 8>(any::<u8>()),
        unix_seconds(),
        prop_oneof![Just(InviteUses::Multiple), Just(InviteUses::Single)],
    )
        .prop_map(
            |(server, mailbox, secret, fingerprint, expires, uses)| InvitePayload {
                server,
                invite_mailbox: MailboxId::from_bytes(mailbox),
                invite_secret: SecretBytes::new(secret),
                inviter_fingerprint: InviterFingerprint::from_bytes(fingerprint),
                expires,
                uses,
            },
        )
}

pub(crate) fn join_request() -> impl Strategy<Value = JoinRequest> {
    (
        opaque(),
        text(DisplayName::MAX_BYTES)
            .prop_filter_map("1 to 200 bytes", |name| DisplayName::new(name).ok()),
        bytes16(),
        subscription(),
    )
        .prop_map(
            |(key_package, display_name, reply_mailbox, party_subscription)| JoinRequest {
                key_package: KeyPackageBytes::from_bytes(key_package),
                display_name,
                reply_mailbox: MailboxId::from_bytes(reply_mailbox),
                party_subscription,
            },
        )
}

pub(crate) fn mailbox_spec() -> impl Strategy<Value = MailboxSpec> {
    (bytes16(), time_to_live(), option::of(sealed_push())).prop_map(
        |(mailbox_id, time_to_live, push)| MailboxSpec {
            mailbox_id: MailboxId::from_bytes(mailbox_id),
            time_to_live,
            push,
        },
    )
}

pub(crate) fn subscription() -> impl Strategy<Value = Subscription> {
    (bytes16(), sealed_push(), time_to_live()).prop_map(|(id, push, time_to_live)| Subscription {
        id: SubscriptionId::from_bytes(id),
        push,
        time_to_live,
    })
}

pub(crate) fn push_token_plaintext() -> impl Strategy<Value = PushTokenPlaintext> {
    (
        prop_oneof![Just(PushPlatform::Fcm), Just(PushPlatform::UnifiedPush)],
        prop_oneof![Just(PushClass::Standard), Just(PushClass::Location)],
        vec(any::<u8>(), 0..=PushTokenPlaintext::MAX_TOKEN_BYTES),
        unix_seconds(),
    )
        .prop_map(|(platform, class, token, not_after)| PushTokenPlaintext {
            platform,
            class,
            token: PushToken::from_bytes(token),
            not_after,
        })
}

pub(crate) fn knock() -> impl Strategy<Value = Knock> {
    (
        bytes16(),
        option::of(bytes16()),
        any::<u32>(),
        any::<u64>(),
        opaque(),
        bytes16(),
        option::of(subscription()),
        bytes64(),
    )
        .prop_map(
            |(matched, own, leaf, epoch, key_package, reply, subscription, signature)| Knock {
                matched_randomness: RecordRandomness::from_bytes(matched),
                own_randomness: own.map(RecordRandomness::from_bytes),
                leaf_index: LeafIndex::new(leaf),
                party_epoch: Epoch::new(epoch),
                key_package: KeyPackageBytes::from_bytes(key_package),
                reply_mailbox: MailboxId::from_bytes(reply),
                meetup_subscription: subscription,
                signature: KnockSignature::from_bytes(signature),
            },
        )
}

fn snapshot() -> impl Strategy<Value = Option<SealedSnapshot>> {
    option::of(opaque().prop_map(SealedSnapshot::from_bytes))
}

fn message() -> impl Strategy<Value = MlsMessageBytes> {
    opaque().prop_map(MlsMessageBytes::from_bytes)
}

fn delivery() -> impl Strategy<Value = Delivery> {
    prop_oneof![Just(Delivery::Stored), Just(Delivery::Ephemeral)]
}

pub(crate) fn create_group() -> impl Strategy<Value = CreateGroup> {
    (bytes32(), bytes32(), vec(subscription(), 0..3), snapshot()).prop_map(
        |(group_id, auth_key, subscriptions, state)| CreateGroup {
            group_id: GroupId::from_bytes(group_id),
            auth_key: GroupAuthKey::from_bytes(auth_key),
            subscriptions,
            state,
        },
    )
}

pub(crate) fn commit_request() -> impl Strategy<Value = CommitRequest> {
    (
        any::<u64>(),
        message(),
        bytes32(),
        snapshot(),
        vec(subscription(), 0..3),
        vec(bytes16().prop_map(SubscriptionId::from_bytes), 0..4),
        urgency(),
    )
        .prop_map(
            |(epoch, commit, next_auth_key, state, added, removed, wake)| CommitRequest {
                epoch: Epoch::new(epoch),
                commit,
                next_auth_key: GroupAuthKey::from_bytes(next_auth_key),
                state,
                subscriptions_added: added,
                subscriptions_removed: removed,
                wake,
            },
        )
}

pub(crate) fn commit_response() -> impl Strategy<Value = CommitResponse> {
    prop_oneof![
        any::<u64>().prop_map(|sequence| CommitResponse::Accepted {
            sequence: SequenceNumber::new(sequence)
        }),
        any::<u64>().prop_map(|epoch| CommitResponse::Conflict {
            head_epoch: Epoch::new(epoch)
        }),
    ]
}

pub(crate) fn app_message() -> impl Strategy<Value = AppMessage> {
    (any::<u64>(), message(), delivery(), option::of(urgency())).prop_map(
        |(epoch, body, delivery, wake)| AppMessage {
            epoch: Epoch::new(epoch),
            body,
            delivery,
            wake,
        },
    )
}

pub(crate) fn group_log_entry() -> impl Strategy<Value = GroupLogEntry> {
    (
        any::<u64>(),
        any::<u64>(),
        prop_oneof![
            Just(ContentType::Application),
            Just(ContentType::Proposal),
            Just(ContentType::Commit)
        ],
        message(),
    )
        .prop_map(|(sequence, epoch, content_type, body)| GroupLogEntry {
            sequence: SequenceNumber::new(sequence),
            epoch: Epoch::new(epoch),
            content_type,
            body,
        })
}

pub(crate) fn live_frame() -> impl Strategy<Value = LiveFrame> {
    (any::<u64>(), message(), delivery()).prop_map(|(epoch, body, delivery)| LiveFrame {
        epoch: Epoch::new(epoch),
        body,
        delivery,
    })
}

pub(crate) fn coordinate() -> impl Strategy<Value = E7Coordinate> {
    (
        -900_000_000_i32..=900_000_000,
        -1_800_000_000_i32..=1_800_000_000,
    )
        .prop_filter_map("coordinate in range", |(latitude, longitude)| {
            E7Coordinate::new(latitude, longitude).ok()
        })
}

pub(crate) fn meet_point() -> impl Strategy<Value = MeetPoint> {
    prop_oneof![
        (
            coordinate(),
            option::of(text(40).prop_filter_map("label", |label| MeetPointLabel::new(label).ok())),
            any::<u64>()
        )
            .prop_map(|(position, label, lamport)| MeetPoint::Fixed {
                position,
                label,
                lamport: Lamport::new(lamport),
            }),
        (any::<u32>(), any::<u64>()).prop_map(|(leaf, lamport)| MeetPoint::AtMember {
            leaf: LeafIndex::new(leaf),
            lamport: Lamport::new(lamport),
        }),
        any::<u64>().prop_map(|lamport| MeetPoint::Cleared {
            lamport: Lamport::new(lamport)
        }),
    ]
}

pub(crate) fn meetup_message() -> impl Strategy<Value = MeetupMessage> {
    prop_oneof![
        (any::<u32>(), uniform::<_, 12>(any::<u8>()), opaque()).prop_map(
            |(generation, nonce, ciphertext)| MeetupMessage::SealedLocation(SealedLocationFrame {
                generation: LocationGeneration::new(generation),
                nonce,
                ciphertext,
            })
        ),
        opaque().prop_map(|sealed| MeetupMessage::LocationGrant(SealedGrant::from_bytes(sealed))),
        opaque().prop_map(|sealed| MeetupMessage::LocationAsk(SealedAsk::from_bytes(sealed))),
        (
            bytes16(),
            option::of(bytes16()),
            text(200).prop_filter_map("chat body", |body| ChatBody::new(body).ok()),
            unix_seconds()
        )
            .prop_map(|(id, parent, body, written_at)| MeetupMessage::Chat {
                id: ChatMessageId::from_bytes(id),
                parent: parent.map(ChatMessageId::from_bytes),
                body,
                written_at,
            }),
        meet_point().prop_map(MeetupMessage::MeetPoint),
        (option::of(meet_point()), any::<u64>()).prop_map(|(meet_point, lamport)| {
            MeetupMessage::Snapshot {
                meet_point,
                lamport: Lamport::new(lamport),
            }
        }),
        vec(
            (bucket(), bytes32()).prop_map(|(start, hash)| SeenSegment {
                start,
                hash: SegmentHash::from_bytes(hash),
            }),
            0..4
        )
        .prop_map(|segments| MeetupMessage::BoardSeen { segments }),
        Just(MeetupMessage::KeepAlive),
    ]
}

pub(crate) fn location_plaintext() -> impl Strategy<Value = LocationPlaintext> {
    (
        coordinate(),
        any::<u16>(),
        option::of(
            (0_u16..360).prop_filter_map("heading", |degrees| HeadingDegrees::new(degrees).ok()),
        ),
        option::of(any::<u16>()),
        unix_seconds(),
    )
        .prop_map(
            |(position, accuracy, heading, speed, taken_at)| LocationPlaintext {
                position,
                accuracy: AccuracyMeters::new(accuracy),
                heading,
                speed: speed.map(SpeedCentimetresPerSecond::new),
                taken_at,
            },
        )
}

pub(crate) fn rln_auth() -> impl Strategy<Value = RlnAuth> {
    (
        bytes32(),
        prop_oneof![
            Just(RlnScope::Board),
            Just(RlnScope::Mailbox),
            Just(RlnScope::Group)
        ],
        bucket(),
        bytes32(),
        bytes32(),
        bytes32(),
        uniform::<_, 128>(any::<u8>()),
    )
        .prop_map(|(root, scope, bucket, x, y, nullifier, proof)| RlnAuth {
            root,
            scope,
            bucket,
            x,
            y,
            nullifier,
            proof,
        })
}

pub(crate) fn params() -> impl Strategy<Value = Params> {
    (
        vec(
            prop_oneof![
                Just(AnonymousCredential::MembershipProof),
                Just(AnonymousCredential::PrivacyPass)
            ],
            0..3,
        ),
        prop_oneof![Just(Slashing::On), Just(Slashing::Off)],
        (0.0_f32..=1.0).prop_filter_map("ratio", |ratio| CoverReadRatio::new(ratio).ok()),
        any::<u32>(),
        vec(relay_url(), 0..3),
        vec(
            (any::<u16>(), relay_url(), bytes32(), text(30)).prop_map(
                |(id, url, hash, operator)| PushRelayInfo {
                    relay_id: RelayId::new(id),
                    url,
                    key_config_hash: KeyConfigHash::from_bytes(hash),
                    operator,
                },
            ),
            0..3,
        ),
        prop_oneof![
            Just(CustomPushRelays::Allowed),
            Just(CustomPushRelays::NotAllowed)
        ],
        text(30),
        unix_seconds(),
        option::of(relay_url().prop_filter_map("map host", |url| {
            MapHostUrl::new(url.as_str().to_owned()).ok()
        })),
    )
        .prop_map(
            |(
                credentials,
                slashing,
                ratio,
                page_records,
                ohttp,
                push,
                custom,
                operator,
                time,
                map_host,
            )| {
                Params {
                    anonymous_credentials: credentials,
                    rln_slashing: slashing,
                    cover_read_ratio: ratio,
                    board_page_records: page_records,
                    ohttp_relays: ohttp,
                    push_relays: push,
                    custom_push_relays: custom,
                    server_operator: operator,
                    server_time: time,
                    map_host,
                }
            },
        )
}

pub(crate) fn register_request() -> impl Strategy<Value = RegisterRequest> {
    (bytes32(), bytes64(), option::of(text(30))).prop_map(|(key, proof, code)| RegisterRequest {
        signature_key: key,
        proof,
        registration_code: code.map(SecretText::new),
    })
}

pub(crate) fn auth_request() -> impl Strategy<Value = AuthRequest> {
    (bytes16(), bytes64()).prop_map(|(account, signature)| AuthRequest {
        account_id: AccountId::from_bytes(account),
        nonce_signature: signature,
    })
}

pub(crate) fn board_sync_request() -> impl Strategy<Value = BoardSyncRequest> {
    (
        lease(),
        option::of((bytes16(), bytes32())),
        prop_oneof![Just(ReadBoard::Read), Just(ReadBoard::Skip)],
    )
        .prop_map(|(lease, replaces, read)| BoardSyncRequest {
            lease,
            replaces: replaces.map(|(lease_id, revoke_key)| LeaseReplacement {
                lease_id: LeaseId::from_bytes(lease_id),
                revoke_key: SecretBytes::new(revoke_key),
            }),
            read,
        })
}

pub(crate) fn board_sync_response() -> impl Strategy<Value = BoardSyncResponse> {
    (bytes16(), unix_seconds(), option::of(board_page())).prop_map(
        |(lease_id, server_time, page)| BoardSyncResponse {
            lease_id: LeaseId::from_bytes(lease_id),
            server_time,
            page,
        },
    )
}

pub(crate) fn register_response() -> impl Strategy<Value = RegisterResponse> {
    bytes16().prop_map(|account| RegisterResponse {
        account_id: AccountId::from_bytes(account),
    })
}

pub(crate) fn auth_response() -> impl Strategy<Value = AuthResponse> {
    (text(60), unix_seconds()).prop_map(|(bearer, server_time)| AuthResponse {
        bearer: SecretText::new(bearer),
        server_time,
    })
}

pub(crate) fn membership_leaf_request() -> impl Strategy<Value = MembershipLeafRequest> {
    bytes32().prop_map(|commitment| MembershipLeafRequest {
        id_commitment: Redacted::new(commitment),
    })
}

pub(crate) fn membership_leaf_response() -> impl Strategy<Value = MembershipLeafResponse> {
    (any::<u32>(), any::<u32>()).prop_map(|(index, epoch)| MembershipLeafResponse {
        index: MembershipIndex::new(index),
        live_from: RootEpoch::new(epoch),
    })
}

pub(crate) fn membership_leaves() -> impl Strategy<Value = MembershipLeaves> {
    (
        vec(bytes32(), 0..4),
        vec(any::<u32>().prop_map(MembershipIndex::new), 0..4),
        vec(
            (any::<u32>(), bytes32()).prop_map(|(epoch, root)| HourlyRoot {
                epoch: RootEpoch::new(epoch),
                root,
            }),
            0..3,
        ),
        any::<u32>(),
    )
        .prop_map(|(leaves, zeroed, roots, next)| MembershipLeaves {
            leaves,
            zeroed,
            roots,
            next: MembershipIndex::new(next),
        })
}

pub(crate) fn mailbox_messages() -> impl Strategy<Value = MailboxMessages> {
    let message = (any::<u64>(), vec(any::<u8>(), 0..1_100)).prop_filter_map(
        "post fits",
        |(sequence, payload)| {
            Padded::pad_smallest(&payload, &SizeClass::MAILBOX_POST)
                .ok()
                .map(|body| MailboxMessage {
                    sequence: SequenceNumber::new(sequence),
                    body,
                })
        },
    );
    vec(message, 0..3).prop_map(|messages| MailboxMessages { messages })
}

pub(crate) fn message_accepted() -> impl Strategy<Value = MessageAccepted> {
    any::<u64>().prop_map(|sequence| MessageAccepted {
        sequence: SequenceNumber::new(sequence),
    })
}

pub(crate) fn group_messages() -> impl Strategy<Value = GroupMessages> {
    (vec(group_log_entry(), 0..4), any::<u64>()).prop_map(|(messages, epoch)| GroupMessages {
        messages,
        head_epoch: Epoch::new(epoch),
    })
}

pub(crate) fn group_state() -> impl Strategy<Value = GroupState> {
    (any::<u64>(), opaque()).prop_map(|(epoch, state)| GroupState {
        epoch: Epoch::new(epoch),
        state: SealedSnapshot::from_bytes(state),
    })
}
