// SPDX-License-Identifier: AGPL-3.0-only
//! Hand-written `Debug` impls print a fixed shape and never the value (style guide §9).

use pingers_proto::{
    BoardCursor, ChatBody, KeyPackageBytes, KnockSignature, LocationGeneration, MeetPointLabel,
    MeetupHint, MlsMessageBytes, ProtoError, PushToken, RecordTag, SealedAsk, SealedBeacon,
    SealedGrant, SealedLocationFrame, SealedPushBlob, SealedSnapshot, WelcomeBytes,
};

#[test]
fn fixed_size_secrets_print_only_their_type() {
    let printed = [
        format!("{:?}", RecordTag::from_bytes([0xab; 16])),
        format!("{:?}", SealedBeacon::from_bytes([0xab; 64])),
        format!("{:?}", MeetupHint::from_wire([0xab; 16])),
        format!("{:?}", BoardCursor::from_bytes(vec![0xab; 4])),
        format!("{:?}", KnockSignature::from_bytes([0xab; 64])),
        format!("{:?}", PushToken::from_bytes(vec![0xab; 4])),
        format!("{:?}", SealedPushBlob::from_bytes([0xab; 320])),
    ];
    assert_eq!(
        printed,
        [
            "RecordTag(redacted)",
            "SealedBeacon(redacted)",
            "Some(MeetupHint(redacted))",
            "BoardCursor(redacted)",
            "KnockSignature(..)",
            "PushToken(redacted)",
            "SealedPushBlob(..)",
        ]
    );
}

#[test]
fn byte_strings_print_only_their_length() {
    let printed = [
        format!("{:?}", KeyPackageBytes::from_bytes(vec![0xab; 3])),
        format!("{:?}", MlsMessageBytes::from_bytes(vec![0xab; 3])),
        format!("{:?}", WelcomeBytes::from_bytes(vec![0xab; 3])),
        format!("{:?}", SealedSnapshot::from_bytes(vec![0xab; 3])),
        format!("{:?}", SealedGrant::from_bytes(vec![0xab; 3])),
        format!("{:?}", SealedAsk::from_bytes(vec![0xab; 3])),
    ];
    assert_eq!(
        printed,
        [
            "KeyPackageBytes(3 bytes)",
            "MlsMessageBytes(3 bytes)",
            "WelcomeBytes(3 bytes)",
            "SealedSnapshot(3 bytes)",
            "SealedGrant(3 bytes)",
            "SealedAsk(3 bytes)",
        ]
    );
}

#[test]
fn text_and_frames_hide_their_content() -> Result<(), ProtoError> {
    assert_eq!(
        format!("{:?}", ChatBody::new(String::from("secret"))?),
        "ChatBody(6 bytes)"
    );
    assert_eq!(
        format!("{:?}", MeetPointLabel::new(String::from("Pub"))?),
        "MeetPointLabel(3 bytes)"
    );
    let frame = SealedLocationFrame {
        generation: LocationGeneration::new(2),
        nonce: [0xab; 12],
        ciphertext: vec![0xab; 5],
    };
    assert_eq!(
        format!("{frame:?}"),
        "SealedLocationFrame { generation: LocationGeneration(2), .. }"
    );
    Ok(())
}

#[test]
fn wire_secrets_stay_out_of_debug_and_compare_by_content() {
    use pingers_proto::{AuthResponse, LeaseId, LeaseReplacement, SecretBytes, SecretText};
    use pingers_time::UnixSeconds;

    let response = AuthResponse {
        bearer: SecretText::new(String::from("bearer-0123456789")),
        server_time: UnixSeconds::new(1),
    };
    let printed = format!("{response:?}");
    assert!(!printed.contains("bearer-0123456789"), "{printed}");
    let replacement = |byte| LeaseReplacement {
        lease_id: LeaseId::from_bytes([1; 16]),
        revoke_key: SecretBytes::new([byte; 32]),
    };
    assert!(!format!("{:?}", replacement(0xab)).contains("171"));
    // `==` on the containers still sees the secret, compared in constant time.
    assert_eq!(replacement(1), replacement(1));
    assert_ne!(replacement(1), replacement(2));
}

#[test]
fn structs_holding_secrets_compare_every_field() -> Result<(), ProtoError> {
    use pingers_proto::{
        AuthResponse, InvitePayload, InviterFingerprint, MailboxId, RegisterRequest,
        SecretBytes, SecretText, ServerId,
    };
    use pingers_time::UnixSeconds;

    let register = RegisterRequest {
        signature_key: [1; 32],
        proof: [2; 64],
        registration_code: Some(SecretText::new(String::from("code"))),
    };
    let mut other = register.clone();
    assert_eq!(register, other);
    other.signature_key = [9; 32];
    assert_ne!(register, other);
    let mut other = register.clone();
    other.proof = [9; 64];
    assert_ne!(register, other);
    let mut other = register.clone();
    other.registration_code = Some(SecretText::new(String::from("cods")));
    assert_ne!(register, other);
    other.registration_code = None;
    assert_ne!(register, other);

    let auth = AuthResponse {
        bearer: SecretText::new(String::from("bearer")),
        server_time: UnixSeconds::new(5),
    };
    let mut other = auth.clone();
    assert_eq!(auth, other);
    other.server_time = UnixSeconds::new(6);
    assert_ne!(auth, other);
    let mut other = auth.clone();
    other.bearer = SecretText::new(String::from("bearex"));
    assert_ne!(auth, other);

    let invite = InvitePayload {
        server: ServerId::new(String::from("https://a.example"))?,
        invite_mailbox: MailboxId::from_bytes([1; 16]),
        invite_secret: SecretBytes::new([2; 32]),
        inviter_fingerprint: InviterFingerprint::from_bytes([3; 8]),
        expires: UnixSeconds::new(4),
    };
    assert_eq!(invite, invite.clone());
    let variants = [
        InvitePayload {
            server: ServerId::new(String::from("https://b.example"))?,
            ..invite.clone()
        },
        InvitePayload {
            invite_mailbox: MailboxId::from_bytes([9; 16]),
            ..invite.clone()
        },
        InvitePayload {
            invite_secret: SecretBytes::new([9; 32]),
            ..invite.clone()
        },
        InvitePayload {
            inviter_fingerprint: InviterFingerprint::from_bytes([9; 8]),
            ..invite.clone()
        },
        InvitePayload {
            expires: UnixSeconds::new(9),
            ..invite.clone()
        },
    ];
    for variant in &variants {
        assert_ne!(&invite, variant);
    }
    Ok(())
}
