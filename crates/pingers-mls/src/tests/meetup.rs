// SPDX-License-Identifier: AGPL-3.0-only
//! Meetup groups: external PSK, roster check, grant keys, leaving, end.

use pingers_crypto::GrantPublicKey;
use pingers_proto::{Epoch, LeafIndex, WelcomeBytes};
use rand_chacha::ChaCha20Rng;

use super::{
    Device, TestError, TestResult, application, commit_summary, grant_key, group_id, meetup_psk,
    padded, psk_id, rng, roster_of,
};
use crate::{
    CipherSuitePolicy, Group, MeetupCreation, MeetupPskBinding, MemberGrantKey, MemoryStorage,
    MlsError, MlsStorage as _, ProcessedMessage, Roster, SelfRemoval,
};

type MeetupGroup<'device> = Group<'device, MemoryStorage, crate::Meetup>;

/// A creates the Meetup with `knocker`; returns A's group (commit merged) and the Welcome.
fn create<'device>(
    a: &'device Device,
    knocker: &Device,
    party_roster: &Roster,
    rng: &mut ChaCha20Rng,
) -> Result<(MeetupGroup<'device>, WelcomeBytes), TestError> {
    create_with_id(
        a,
        Knock {
            knocker,
            party_roster,
        },
        0xA0,
        rng,
    )
}

/// Who knocks, and the Party roster the creator checks them against.
#[derive(Clone, Copy)]
struct Knock<'knock> {
    knocker: &'knock Device,
    party_roster: &'knock Roster,
}

fn create_with_id<'device>(
    a: &'device Device,
    knock: Knock<'_>,
    id: u8,
    rng: &mut ChaCha20Rng,
) -> Result<(MeetupGroup<'device>, WelcomeBytes), TestError> {
    let Knock {
        knocker,
        party_roster,
    } = knock;
    let first_member = knocker.meetup_key_package(&grant_key(2), rng)?;
    let psk = meetup_psk(7);
    let created = a.context().create_meetup_group(
        &MeetupCreation {
            group_id: group_id(id),
            psk: MeetupPskBinding {
                id: psk_id(4)?,
                value: &psk,
            },
            party_roster,
            first_member: &first_member,
            own_grant_key: &grant_key(1),
            suite: CipherSuitePolicy::Classical,
        },
        rng,
    )?;
    let mut group = created.group;
    let welcome = created
        .commit
        .welcome
        .ok_or(TestError::Missing("a Welcome"))?;
    let summary = group.merge_pending_commit()?;
    assert_eq!(summary.added, vec![LeafIndex::new(1)]);
    Ok((group, welcome))
}

fn join<'device>(
    device: &'device Device,
    welcome: &WelcomeBytes,
    party_roster: &Roster,
) -> Result<MeetupGroup<'device>, MlsError> {
    let psk = meetup_psk(7);
    let id = psk_id(4).map_err(|_| MlsError::Internal)?;
    device
        .context()
        .join_meetup_group(welcome, &MeetupPskBinding { id, value: &psk }, party_roster)
}

#[test]
fn meetup_is_created_with_the_psk_and_joined() -> TestResult {
    let mut rng = rng(30);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (mut group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party)?;
    assert_eq!(group_b.epoch(), Epoch::new(1));
    assert_eq!(group_b.group_id(), group_id(0xA0));
    assert_eq!(group_b.roster(), group_a.roster());

    let frame = group_a.seal_application(&padded(b"location")?, &mut rng)?;
    let (sender, plaintext) = application(group_b.process(&frame)?)?;
    assert_eq!(sender, LeafIndex::new(0));
    assert_eq!(plaintext, padded(b"location")?.as_bytes());

    let expected = vec![
        MemberGrantKey {
            leaf: LeafIndex::new(0),
            grant_key: grant_key(1),
        },
        MemberGrantKey {
            leaf: LeafIndex::new(1),
            grant_key: grant_key(2),
        },
    ];
    assert_eq!(group_a.grant_keys()?, expected);
    assert_eq!(group_b.grant_keys()?, expected);
    // The PSK is not left in either device's storage.
    assert!(!a.storage.contains_key_fragment(b"openmls/v1/Psk/"));
    assert!(!b.storage.contains_key_fragment(b"openmls/v1/Psk/"));
    Ok(())
}

#[test]
fn knocker_outside_the_party_roster_is_refused_at_creation() {
    let mut rng = rng(31);
    let (a, b, outsider) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let party = roster_of(&[&a, &b]);
    let result = create(&a, &outsider, &party, &mut rng);
    assert!(matches!(
        result,
        Err(TestError::Mls(MlsError::NotInPartyRoster))
    ));
    assert!(matches!(
        a.context().meetup_group(&group_id(0xA0)),
        Err(MlsError::NotAMember)
    ));
}

#[test]
fn party_key_package_without_grant_key_is_refused() -> TestResult {
    let mut rng = rng(32);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let first_member = b.party_key_package(&mut rng)?;
    let psk = meetup_psk(7);
    let result = a.context().create_meetup_group(
        &MeetupCreation {
            group_id: group_id(0xA0),
            psk: MeetupPskBinding {
                id: psk_id(4)?,
                value: &psk,
            },
            party_roster: &party,
            first_member: &first_member,
            own_grant_key: &grant_key(1),
            suite: CipherSuitePolicy::Classical,
        },
        &mut rng,
    );
    assert!(matches!(result.map(|_| ()), Err(MlsError::InvalidMessage)));
    Ok(())
}

#[test]
fn substituted_key_package_is_refused_when_adding() -> TestResult {
    let mut rng = rng(33);
    let (a, b, outsider) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let party = roster_of(&[&a, &b]);
    let (mut group_a, _welcome) = create(&a, &b, &party, &mut rng)?;
    let substituted = outsider.meetup_key_package(&grant_key(3), &mut rng)?;
    assert!(matches!(
        group_a.add_members(&[substituted], &party, &mut rng),
        Err(MlsError::NotInPartyRoster)
    ));
    assert_eq!(group_a.epoch(), Epoch::new(1));
    // Nothing is pending: A can still seal.
    group_a.seal_application(&padded(b"still here")?, &mut rng)?;
    Ok(())
}

#[test]
fn members_refuse_a_commit_adding_someone_outside_their_roster() -> TestResult {
    let mut rng = rng(34);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let party_without_c = roster_of(&[&a, &b]);
    let party_with_c = roster_of(&[&a, &b, &c]);
    let (mut group_a, welcome) = create(&a, &b, &party_without_c, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party_without_c)?;
    // A was given (say, by a malicious server) a roster that includes C; B's does not.
    let add_c = group_a.add_members(
        &[c.meetup_key_package(&grant_key(3), &mut rng)?],
        &party_with_c,
        &mut rng,
    )?;
    assert!(matches!(
        group_b.process(&add_c.commit),
        Err(MlsError::NotInPartyRoster)
    ));
    assert_eq!(group_b.epoch(), Epoch::new(1));
    Ok(())
}

#[test]
fn each_meetup_checks_against_its_own_roster() -> TestResult {
    let mut rng = rng(47);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let party_without_c = roster_of(&[&a, &b]);
    let party_with_c = roster_of(&[&a, &b, &c]);
    // B is in two Meetups: one whose Party roster includes C, one whose roster does not.
    let (mut first_a, first_welcome) = create_with_id(
        &a,
        Knock {
            knocker: &b,
            party_roster: &party_without_c,
        },
        0xA1,
        &mut rng,
    )?;
    let mut first_b = join(&b, &first_welcome, &party_without_c)?;
    let (_second_a, second_welcome) = create_with_id(
        &a,
        Knock {
            knocker: &b,
            party_roster: &party_with_c,
        },
        0xA2,
        &mut rng,
    )?;
    let _second_b = join(&b, &second_welcome, &party_with_c)?;
    let add_c = first_a.add_members(
        &[c.meetup_key_package(&grant_key(3), &mut rng)?],
        &party_with_c,
        &mut rng,
    )?;
    assert!(matches!(
        first_b.process(&add_c.commit),
        Err(MlsError::NotInPartyRoster)
    ));
    Ok(())
}

#[test]
fn newcomer_in_the_updated_roster_is_accepted_by_members() -> TestResult {
    let mut rng = rng(35);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let party_without_c = roster_of(&[&a, &b]);
    let party_with_c = roster_of(&[&a, &b, &c]);
    let (mut group_a, welcome) = create(&a, &b, &party_without_c, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party_without_c)?;
    // C joined the Party after the Meetup started; B learns the new roster.
    group_b.update_party_roster(&party_with_c)?;
    let add_c = group_a.add_members(
        &[c.meetup_key_package(&grant_key(3), &mut rng)?],
        &party_with_c,
        &mut rng,
    )?;
    group_a.merge_pending_commit()?;
    let summary = commit_summary(group_b.process(&add_c.commit)?)?;
    assert_eq!(summary.added, vec![LeafIndex::new(2)]);
    assert_eq!(summary.epoch, Epoch::new(2));
    let welcome_c = add_c.welcome.ok_or(TestError::Missing("a Welcome"))?;
    let group_c = join(&c, &welcome_c, &party_with_c)?;
    assert_eq!(group_c.grant_keys()?.len(), 3);
    // The stored roster is reloaded with the group.
    let mut reloaded_b = b.context().meetup_group(&group_id(0xA0))?;
    let update = group_a.self_update(&mut rng)?;
    group_a.merge_pending_commit()?;
    commit_summary(reloaded_b.process(&update.commit)?)?;
    Ok(())
}

#[test]
fn welcome_with_another_psk_id_is_refused() -> TestResult {
    let mut rng = rng(36);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (_group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let psk = meetup_psk(7);
    let result = b.context().join_meetup_group(
        &welcome,
        &MeetupPskBinding {
            id: psk_id(5)?,
            value: &psk,
        },
        &party,
    );
    assert!(matches!(result.map(|_| ()), Err(MlsError::PskMismatch)));
    assert!(matches!(
        b.context().meetup_group(&group_id(0xA0)),
        Err(MlsError::NotAMember)
    ));
    Ok(())
}

#[test]
fn welcome_with_another_psk_value_is_refused() -> TestResult {
    let mut rng = rng(37);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (_group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let wrong = meetup_psk(8);
    let result = b.context().join_meetup_group(
        &welcome,
        &MeetupPskBinding {
            id: psk_id(4)?,
            value: &wrong,
        },
        &party,
    );
    assert!(matches!(result.map(|_| ()), Err(MlsError::PskMismatch)));
    assert!(!b.storage.contains_key_fragment(b"openmls/v1/Psk/"));
    Ok(())
}

#[test]
fn party_welcome_is_not_a_meetup_welcome() -> TestResult {
    let mut rng = rng(38);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let mut party_group =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let add = party_group.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    let welcome = add.welcome.ok_or(TestError::Missing("a Welcome"))?;
    assert!(matches!(
        join(&b, &welcome, &party).map(|_| ()),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn meetup_welcome_is_not_a_party_welcome() -> TestResult {
    let mut rng = rng(39);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (_group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    assert!(matches!(
        b.context().join_party_group(&welcome).map(|_| ()),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn joiner_refuses_a_meetup_with_a_member_outside_its_roster() -> TestResult {
    let mut rng = rng(40);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    // A is not in B's view of the Party.
    let party_for_a = roster_of(&[&a, &b]);
    let party_for_b = roster_of(&[&b]);
    let (_group_a, welcome) = create(&a, &b, &party_for_a, &mut rng)?;
    assert!(matches!(
        join(&b, &welcome, &party_for_b).map(|_| ()),
        Err(MlsError::NotInPartyRoster)
    ));
    assert!(matches!(
        b.context().meetup_group(&group_id(0xA0)),
        Err(MlsError::NotAMember)
    ));
    Ok(())
}

#[test]
fn grant_keys_survive_updates() -> TestResult {
    let mut rng = rng(41);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (mut group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party)?;
    let update = group_b.self_update(&mut rng)?;
    group_b.merge_pending_commit()?;
    commit_summary(group_a.process(&update.commit)?)?;
    let keys: Vec<GrantPublicKey> = group_a
        .grant_keys()?
        .into_iter()
        .map(|member| member.grant_key)
        .collect();
    assert_eq!(keys, vec![grant_key(1), grant_key(2)]);
    Ok(())
}

#[test]
fn leaver_self_removes_and_the_meetup_continues() -> TestResult {
    let mut rng = rng(42);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let party = roster_of(&[&a, &b, &c]);
    let (mut group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party)?;
    let add_c = group_a.add_members(
        &[c.meetup_key_package(&grant_key(3), &mut rng)?],
        &party,
        &mut rng,
    )?;
    group_a.merge_pending_commit()?;
    group_b.process(&add_c.commit)?;
    let mut group_c = join(
        &c,
        &add_c.welcome.ok_or(TestError::Missing("a Welcome"))?,
        &party,
    )?;

    let leave = group_b.propose_leave(&mut rng)?;
    b.context().delete_group(&group_id(0xA0))?;
    assert!(b.storage.is_empty());
    for group in [&mut group_a, &mut group_c] {
        assert!(matches!(
            group.process(&leave)?,
            ProcessedMessage::Proposal { .. }
        ));
    }
    let commit = group_c.commit_pending_proposals(&mut rng)?;
    group_c.merge_pending_commit()?;
    let summary = commit_summary(group_a.process(&commit.commit)?)?;
    assert_eq!(summary.removed, vec![LeafIndex::new(1)]);
    let frame = group_a.seal_application(&padded(b"two left")?, &mut rng)?;
    assert_eq!(
        application(group_c.process(&frame)?)?.1,
        padded(b"two left")?.as_bytes()
    );
    Ok(())
}

#[test]
fn silent_member_is_removed_and_learns_it() -> TestResult {
    let mut rng = rng(43);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (mut group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party)?;
    let removal = group_a.remove_member(LeafIndex::new(1), &mut rng)?;
    group_a.merge_pending_commit()?;
    let summary = commit_summary(group_b.process(&removal.commit)?)?;
    assert_eq!(summary.self_removal, SelfRemoval::Removed);
    Ok(())
}

#[test]
fn ending_the_meetup_deletes_everything() -> TestResult {
    let mut rng = rng(44);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let group_b = join(&b, &welcome, &party)?;
    drop((group_a, group_b));
    a.context().delete_group(&group_id(0xA0))?;
    b.context().delete_group(&group_id(0xA0))?;
    assert!(a.storage.is_empty());
    assert!(b.storage.is_empty());
    assert!(matches!(
        a.context().meetup_group(&group_id(0xA0)),
        Err(MlsError::NotAMember)
    ));
    Ok(())
}

#[test]
fn store_can_wipe_one_group_by_its_prefix() -> TestResult {
    let mut rng = rng(48);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let first_knock = Knock {
        knocker: &b,
        party_roster: &party,
    };
    let (first, _) = create_with_id(&a, first_knock, 0xA1, &mut rng)?;
    let (second, _) = create_with_id(&a, first_knock, 0xA2, &mut rng)?;
    drop((first, second));
    // What pingers-store does when a Meetup ends, without going through MlsContext.
    a.storage
        .delete_prefix(&crate::group_storage_prefix(&group_id(0xA1))?)
        .map_err(MlsError::from)?;
    assert!(matches!(
        a.context().meetup_group(&group_id(0xA1)),
        Err(MlsError::NotAMember)
    ));
    // The other Meetup is untouched, roster included.
    let mut second = a.context().meetup_group(&group_id(0xA2))?;
    second.seal_application(&padded(b"still here")?, &mut rng)?;
    let roster = crate::policy::load_party_roster(&a.storage, &group_id(0xA2))?;
    assert_eq!(roster, party);
    Ok(())
}

/// Seals `count` frames from `sender`; `receiver` gets the newest first, then the one
/// `count - 2` generations older (which must open) and the one `count - 1` older (which
/// must not).
fn reordering_window<Kind: crate::group::GroupKind>(
    sender: &mut Group<'_, MemoryStorage, Kind>,
    receiver: &mut Group<'_, MemoryStorage, Kind>,
    count: usize,
    rng: &mut ChaCha20Rng,
) -> TestResult {
    let frame = padded(b"frame")?;
    let frames = (0..count)
        .map(|_| sender.seal_application(&frame, rng))
        .collect::<Result<Vec<_>, MlsError>>()?;
    let (newest, older) = frames.split_last().ok_or(TestError::Missing("frames"))?;
    receiver.process(newest)?;
    let within = older.get(1).ok_or(TestError::Missing("frames"))?;
    application(receiver.process(within)?)?;
    let beyond = older.first().ok_or(TestError::Missing("frames"))?;
    assert!(matches!(
        receiver.process(beyond),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn meetup_tolerates_fifty_generations_of_reordering() -> TestResult {
    let mut rng = rng(45);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    let (mut group_a, welcome) = create(&a, &b, &party, &mut rng)?;
    let mut group_b = join(&b, &welcome, &party)?;
    // Both directions: the creator's configuration and the joiner's.
    reordering_window(&mut group_a, &mut group_b, 51, &mut rng)?;
    reordering_window(&mut group_b, &mut group_a, 51, &mut rng)
}

#[test]
fn party_keeps_the_default_five_generations() -> TestResult {
    let mut rng = rng(46);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut party_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let add = party_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    party_a.merge_pending_commit()?;
    let mut party_b = b.context().join_party_group(
        add.welcome
            .as_ref()
            .ok_or(TestError::Missing("a Welcome"))?,
    )?;
    reordering_window(&mut party_a, &mut party_b, 6, &mut rng)?;
    reordering_window(&mut party_b, &mut party_a, 6, &mut rng)
}
