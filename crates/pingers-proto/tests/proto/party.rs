// SPDX-License-Identifier: AGPL-3.0-only
//! Party state operations and snapshots: round trips, corruption, redacted `Debug`, the
//! float rules, and the known-answer vectors in `vectors/proto/party-*.json`.

use pingers_proto::{
    DisplayName, FencePoint, Lamport, MemberKey, MemberProfile, PartyId, PartyOperation,
    PartySnapshot, PartyTitle, ProfileUpdate, ProtoError, SpotFence, SpotId, SpotLife, SpotState,
    SpotTitle, SubscriptionId, WireDecode, WireEncode,
};
use pingers_testkit::{Vector, VectorError, load_vectors};
use proptest::array::uniform;
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::Index;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Vector(#[from] VectorError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error("vector `{0}` field `{1}` is missing or malformed")]
    Field(String, &'static str),
}

// --- strategies ---

fn name_text() -> impl Strategy<Value = String> {
    vec(any::<char>(), 1..60).prop_map(|characters| characters.into_iter().collect())
}

fn point() -> impl Strategy<Value = FencePoint> {
    (-90.0_f64..=90.0, -180.0_f64..=180.0).prop_filter_map("finite", |(latitude, longitude)| {
        FencePoint::new(latitude, longitude).ok()
    })
}

fn fence() -> impl Strategy<Value = SpotFence> {
    prop_oneof![
        (point(), 0.0_f64..1.0e7).prop_map(|(center, radius_meters)| SpotFence::Circle {
            center,
            radius_meters,
        }),
        vec(point(), 0..12).prop_map(SpotFence::Polygon),
    ]
}

fn spot() -> impl Strategy<Value = SpotState> {
    (
        uniform::<_, 16>(any::<u8>()),
        name_text().prop_filter_map("1 to 200 bytes", |name| SpotTitle::new(name).ok()),
        fence(),
        uniform::<_, 32>(any::<u8>()),
        any::<u64>(),
        any::<bool>(),
    )
        .prop_map(|(id, name, fence, key, lamport, deleted)| SpotState {
            id: SpotId::from_bytes(id),
            name,
            fence,
            created_by: MemberKey::from_bytes(key),
            lamport: Lamport::new(lamport),
            life: if deleted {
                SpotLife::Deleted
            } else {
                SpotLife::Active
            },
        })
}

fn profile() -> impl Strategy<Value = ProfileUpdate> {
    (
        name_text().prop_filter_map("1 to 200 bytes", |name| DisplayName::new(name).ok()),
        option::of(uniform::<_, 16>(any::<u8>())),
    )
        .prop_map(|(name, subscription)| ProfileUpdate {
            name,
            subscription: subscription.map(SubscriptionId::from_bytes),
        })
}

fn operation() -> impl Strategy<Value = PartyOperation> {
    prop_oneof![
        profile().prop_map(PartyOperation::Profile),
        spot().prop_map(PartyOperation::Spot),
    ]
}

fn snapshot() -> impl Strategy<Value = PartySnapshot> {
    (
        uniform::<_, 16>(any::<u8>()),
        name_text().prop_filter_map("1 to 200 bytes", |name| PartyTitle::new(name).ok()),
        vec(
            (uniform::<_, 32>(any::<u8>()), profile()).prop_map(|(key, profile)| MemberProfile {
                key: MemberKey::from_bytes(key),
                profile,
            }),
            0..4,
        ),
        vec(spot(), 0..3),
    )
        .prop_map(|(party, name, members, spots)| PartySnapshot {
            party: PartyId::from_bytes(party),
            name,
            members,
            spots,
        })
}

// --- properties ---

fn round_trip<T>(value: &T, at: Index, byte: u8) -> Result<(), TestCaseError>
where
    T: WireEncode + WireDecode + PartialEq + core::fmt::Debug,
{
    let fail = |error: ProtoError| TestCaseError::fail(error.to_string());
    let bytes = value.encode().map_err(fail)?;
    prop_assert_eq!(&T::decode(&bytes).map_err(fail)?, value);
    // One replaced byte either fails or decodes to a value encoding to exactly those bytes.
    let mut corrupted = bytes;
    let position = at.index(corrupted.len());
    if let Some(target) = corrupted.get_mut(position) {
        *target = byte;
    }
    if let Ok(decoded) = T::decode(&corrupted) {
        prop_assert_eq!(decoded.encode().map_err(fail)?, corrupted);
    }
    Ok(())
}

proptest! {
    #[test]
    fn operations_round_trip_and_stay_canonical(
        value in operation(),
        at in any::<Index>(),
        byte in any::<u8>(),
    ) {
        round_trip(&value, at, byte)?;
    }

    #[test]
    fn snapshots_round_trip_and_stay_canonical(
        value in snapshot(),
        at in any::<Index>(),
        byte in any::<u8>(),
    ) {
        round_trip(&value, at, byte)?;
    }
}

#[test]
fn coordinates_survive_bit_for_bit() -> Result<(), TestError> {
    // A value that a four-byte float could not hold, and the smallest subnormal.
    let center = FencePoint::new(51.501_234_567_890_12, f64::from_bits(1))?;
    let operation = PartyOperation::Spot(SpotState {
        id: SpotId::from_bytes([1; 16]),
        name: SpotTitle::new("Pub".into())?,
        fence: SpotFence::Circle {
            center,
            radius_meters: 100.000_000_000_1,
        },
        created_by: MemberKey::from_bytes([2; 32]),
        lamport: Lamport::new(1),
        life: SpotLife::Active,
    });
    assert_eq!(PartyOperation::decode(&operation.encode()?)?, operation);
    Ok(())
}

#[test]
fn non_finite_numbers_are_refused() {
    assert!(matches!(
        FencePoint::new(f64::NAN, 0.0),
        Err(ProtoError::OutOfRange { field: "latitude" })
    ));
    assert!(matches!(
        FencePoint::new(0.0, f64::INFINITY),
        Err(ProtoError::OutOfRange { field: "longitude" })
    ));
}

#[test]
fn debug_prints_no_names_keys_or_places() -> Result<(), TestError> {
    let printed = format!(
        "{:?} {:?} {:?} {:?}",
        PartyTitle::new("Friends".into())?,
        SpotTitle::new("Home".into())?,
        MemberKey::from_bytes([0xab; 32]),
        FencePoint::new(51.5, -0.1)?,
    );
    assert_eq!(
        printed,
        "PartyTitle(redacted) SpotTitle(redacted) MemberKey(redacted) FencePoint(redacted)"
    );
    Ok(())
}

#[test]
fn names_are_one_to_two_hundred_bytes() {
    assert!(PartyTitle::new(String::new()).is_err());
    assert!(SpotTitle::new("a".repeat(200)).is_ok());
    assert!(SpotTitle::new("a".repeat(201)).is_err());
}

// --- vectors ---

fn field(vector: &Vector, name: &'static str) -> TestError {
    TestError::Field(vector.name.clone(), name)
}

fn text(vector: &Vector, name: &'static str) -> Result<String, TestError> {
    vector
        .inputs
        .get(name)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| field(vector, name))
}

fn float(vector: &Vector, name: &'static str) -> Result<f64, TestError> {
    vector
        .inputs
        .get(name)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| field(vector, name))
}

fn array<const N: usize>(vector: &Vector, name: &'static str) -> Result<[u8; N], TestError> {
    vector
        .input_bytes(name)?
        .try_into()
        .map_err(|_length| field(vector, name))
}

fn optional_subscription(
    vector: &Vector,
    name: &'static str,
) -> Result<Option<SubscriptionId>, TestError> {
    if vector.input_bytes(name)?.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SubscriptionId::from_bytes(array(vector, name)?)))
    }
}

fn spot_from(vector: &Vector, fence: SpotFence) -> Result<SpotState, TestError> {
    Ok(SpotState {
        id: SpotId::from_bytes(array(vector, "spot")?),
        name: SpotTitle::new(text(vector, "name")?)?,
        fence,
        created_by: MemberKey::from_bytes(array(vector, "created_by")?),
        lamport: Lamport::new(vector.input_u64("lamport")?),
        life: match vector.input_u64("deleted")? {
            0 => SpotLife::Active,
            _ => SpotLife::Deleted,
        },
    })
}

fn pub_spot() -> Result<SpotState, TestError> {
    Ok(SpotState {
        id: SpotId::from_bytes(core::array::from_fn(|index| {
            u8::try_from(index).unwrap_or_default()
        })),
        name: SpotTitle::new("Pub".into())?,
        fence: SpotFence::Circle {
            center: FencePoint::new(51.5, -0.1)?,
            radius_meters: 150.0,
        },
        created_by: MemberKey::from_bytes(core::array::from_fn(|index| {
            u8::try_from(index).unwrap_or_default().saturating_add(0x20)
        })),
        lamport: Lamport::new(3),
        life: SpotLife::Active,
    })
}

fn operation_from(vector: &Vector) -> Result<PartyOperation, TestError> {
    Ok(match text(vector, "kind")?.as_str() {
        "profile" => PartyOperation::Profile(ProfileUpdate {
            name: DisplayName::new(text(vector, "name")?)?,
            subscription: optional_subscription(vector, "subscription")?,
        }),
        "circle" => PartyOperation::Spot(spot_from(
            vector,
            SpotFence::Circle {
                center: FencePoint::new(float(vector, "latitude")?, float(vector, "longitude")?)?,
                radius_meters: float(vector, "radius")?,
            },
        )?),
        "polygon" => PartyOperation::Spot(spot_from(
            vector,
            SpotFence::Polygon(vec![
                FencePoint::new(1.0, 2.0)?,
                FencePoint::new(1.5, 2.0)?,
                FencePoint::new(1.5, 2.5)?,
            ]),
        )?),
        _ => return Err(field(vector, "kind")),
    })
}

fn snapshot_from(vector: &Vector) -> Result<PartySnapshot, TestError> {
    Ok(PartySnapshot {
        party: PartyId::from_bytes(array(vector, "party")?),
        name: PartyTitle::new(text(vector, "name")?)?,
        members: vec![MemberProfile {
            key: MemberKey::from_bytes(array(vector, "member_key")?),
            profile: ProfileUpdate {
                name: DisplayName::new(text(vector, "member_name")?)?,
                subscription: optional_subscription(vector, "member_subscription")?,
            },
        }],
        spots: vec![pub_spot()?],
    })
}

fn check<T: WireEncode + WireDecode + PartialEq + core::fmt::Debug>(
    file: &str,
    build: fn(&Vector) -> Result<T, TestError>,
) -> Result<(), TestError> {
    let loaded = load_vectors(&format!("proto/{file}"))?;
    assert_eq!(loaded.name, file);
    assert!(!loaded.vectors.is_empty());
    for vector in &loaded.vectors {
        if let Some(expected) = vector.expected.get("error") {
            let error = T::decode(&vector.input_bytes("encoding")?).err();
            let variant = error.map(|error| {
                let debug = format!("{error:?}");
                debug
                    .split(|character: char| !character.is_alphanumeric())
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            });
            assert_eq!(variant.as_deref(), expected.as_str(), "{}", vector.name);
            continue;
        }
        let bytes = vector.expected_bytes("encoding")?;
        let value = build(vector)?;
        assert_eq!(value.encode()?, bytes, "{}: encode", vector.name);
        assert_eq!(T::decode(&bytes)?, value, "{}: decode", vector.name);
    }
    Ok(())
}

#[test]
fn party_operation_vectors() -> Result<(), TestError> {
    check("party-operation-encoding", operation_from)
}

#[test]
fn party_snapshot_vectors() -> Result<(), TestError> {
    check("party-snapshot-encoding", snapshot_from)
}

#[test]
fn a_fence_point_gives_back_its_coordinates() -> Result<(), TestError> {
    let point = FencePoint::new(51.5, -0.1)?;
    assert!((point.latitude() - 51.5).abs() < f64::EPSILON);
    assert!((point.longitude() + 0.1).abs() < f64::EPSILON);
    Ok(())
}
