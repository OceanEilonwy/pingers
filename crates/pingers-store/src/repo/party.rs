// SPDX-License-Identifier: AGPL-3.0-only
//! `identity`, `party`, `member` and `always_allow`.

use pingers_crypto::{AccountPublicKey, AccountSigningKey};
use pingers_proto::{AccountId, GroupId, LeafIndex, PartyId, ServerId};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{narrow, read_array, read_secret, read_time, time};
use crate::repo::expect_one;
use crate::server::{origin_of, server_key};
use crate::{Avatar, MemberName, PartyName, Session, StoreError};

/// This device's account (design B.1, C.2 `identity`): one row.
pub struct Identity {
    /// The account id, `SHA-256(signature public key)[..16]`.
    pub account: AccountId,
    /// The account's Ed25519 key, also its MLS leaf signature key. The same key registers
    /// the account on every server the device has Parties on (each server's standing is in
    /// its own rows, see [`crate::ServerSession`]).
    pub signing_key: AccountSigningKey,
}

/// Repository for the `identity` table.
pub struct Identities<'session> {
    session: &'session Session,
}

impl Session {
    /// The account (`identity` table).
    #[must_use]
    pub fn identity(&self) -> Identities<'_> {
        Identities { session: self }
    }

    /// Parties (`party` table).
    #[must_use]
    pub fn parties(&self) -> Parties<'_> {
        Parties { session: self }
    }

    /// Party members (`member` table).
    #[must_use]
    pub fn members(&self) -> Members<'_> {
        Members { session: self }
    }

    /// The location-request allow-list (`always_allow` table, design B.6.1).
    #[must_use]
    pub fn always_allow(&self) -> AlwaysAllow<'_> {
        AlwaysAllow { session: self }
    }
}

impl Identities<'_> {
    /// Stores the account, replacing any previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn save(&self, identity: &Identity) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO identity (singleton, account_id, sig_sk) VALUES (1, ?1, ?2)",
                params![
                    identity.account.as_bytes(),
                    identity.signing_key.expose_bytes(),
                ],
            )?;
            Ok(())
        })
    }

    /// The account, if one has been created.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn load(&self) -> Result<Option<Identity>, StoreError> {
        self.session.with(|connection| {
            let row = connection
                .query_row("SELECT account_id, sig_sk FROM identity", [], |row| {
                    Ok((
                        read_array(row, 0, "identity.account_id"),
                        read_secret(row, 1, "identity.sig_sk"),
                    ))
                })
                .optional()?;
            row.map(|(account, secret)| {
                Ok(Identity {
                    account: AccountId::from_bytes(account?),
                    signing_key: AccountSigningKey::from_bytes(secret?),
                })
            })
            .transpose()
        })
    }
}

/// How a member's exact position is shared inside a Meetup (design B.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationVisibility {
    /// Nobody holds your location key until you approve their request (the default).
    AskEachTime,
    /// Every current and newly added Meetup member is granted automatically.
    Everyone,
    /// Requests are dropped; no grants are sent.
    Nobody,
}

impl LocationVisibility {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::AskEachTime => 0,
            Self::Everyone => 1,
            Self::Nobody => 2,
        }
    }

    pub(crate) fn from_code(code: i64, field: &'static str) -> Result<Self, StoreError> {
        match code {
            0 => Ok(Self::AskEachTime),
            1 => Ok(Self::Everyone),
            2 => Ok(Self::Nobody),
            _ => Err(StoreError::Corrupt { field }),
        }
    }
}

/// A Party (design C.2 `party`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Party {
    /// The Party's id.
    pub id: PartyId,
    /// Its MLS group.
    pub group: GroupId,
    /// Its name.
    pub name: PartyName,
    /// This device's leaf in the group.
    pub own_leaf: LeafIndex,
    /// When this device created or joined it.
    pub created_at: UnixSeconds,
    /// A per-Party override of the device's location visibility; `None` follows the device.
    pub location_visibility: Option<LocationVisibility>,
    /// The server the Party lives on: its group log, mailboxes and board.
    pub server: ServerId,
}

/// Repository for the `party` table.
pub struct Parties<'session> {
    session: &'session Session,
}

const PARTY_COLUMNS: &str = "party_id, mls_group_id, name, my_leaf, created_at, \
     location_visibility, (SELECT origin FROM server WHERE server.server_key = party.server_key)";

fn party_from_row(row: &Row<'_>) -> Result<Party, StoreError> {
    Ok(Party {
        id: PartyId::from_bytes(read_array(row, 0, "party.party_id")?),
        group: GroupId::from_bytes(read_array(row, 1, "party.mls_group_id")?),
        name: PartyName::new(row.get(2)?).map_err(|_| StoreError::Corrupt {
            field: "party.name",
        })?,
        own_leaf: LeafIndex::new(narrow(row.get(3)?, "party.my_leaf")?),
        created_at: read_time(row, 4, "party.created_at")?,
        location_visibility: row
            .get::<_, Option<i64>>(5)?
            .map(|code| LocationVisibility::from_code(code, "party.location_visibility"))
            .transpose()?,
        server: origin_of(row.get(6)?)?,
    })
}

impl Parties<'_> {
    /// Adds a Party.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if a Party with this id or group already exists.
    pub fn insert(&self, party: &Party) -> Result<(), StoreError> {
        self.session.with(|connection| {
            let server = server_key(connection, &party.server)?;
            connection.execute(
                "INSERT INTO party (party_id, mls_group_id, name, my_leaf, created_at, \
                 location_visibility, server_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    party.id.as_bytes(),
                    party.group.as_bytes(),
                    party.name.as_str(),
                    party.own_leaf.get(),
                    time(party.created_at, "party.created_at")?,
                    party.location_visibility.map(LocationVisibility::code),
                    server,
                ],
            )?;
            Ok(())
        })
    }

    /// The Party with this id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, id: &PartyId) -> Result<Option<Party>, StoreError> {
        self.query_one("party_id", id.as_bytes())
    }

    /// The Party whose MLS group this is.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn by_group(&self, group: &GroupId) -> Result<Option<Party>, StoreError> {
        self.query_one("mls_group_id", group.as_bytes())
    }

    fn query_one(&self, column: &str, value: &[u8]) -> Result<Option<Party>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!("SELECT {PARTY_COLUMNS} FROM party WHERE {column} = ?1"),
                    [value],
                    |row| Ok(party_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Every Party, oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<Party>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {PARTY_COLUMNS} FROM party ORDER BY created_at, party_id"
            ))?;
            let rows = statement.query_map([], |row| Ok(party_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Renames a Party.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such Party.
    pub fn rename(&self, id: &PartyId, name: &PartyName) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE party SET name = ?2 WHERE party_id = ?1",
                    params![id.as_bytes(), name.as_str()],
                )?,
                "party",
            )
        })
    }

    /// Records this device's new leaf after a re-join or tree change.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such Party.
    pub fn set_own_leaf(&self, id: &PartyId, leaf: LeafIndex) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE party SET my_leaf = ?2 WHERE party_id = ?1",
                    params![id.as_bytes(), leaf.get()],
                )?,
                "party",
            )
        })
    }

    /// Sets or clears (`None`) the Party's location-visibility override.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such Party.
    pub fn set_location_visibility(
        &self,
        id: &PartyId,
        visibility: Option<LocationVisibility>,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE party SET location_visibility = ?2 WHERE party_id = ?1",
                    params![id.as_bytes(), visibility.map(LocationVisibility::code)],
                )?,
                "party",
            )
        })
    }

    /// Deletes a Party and, with it, its members, Spots and their presence, its Meetups (with
    /// their keys) and its allow-list. MLS state is pingers-mls's to delete
    /// (`MlsContext::delete_group`).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such Party.
    pub fn delete(&self, id: &PartyId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute("DELETE FROM party WHERE party_id = ?1", [id.as_bytes()])?,
                "party",
            )
        })
    }
}

/// Whether this device has checked a member's safety code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verification {
    /// Not checked.
    Unverified,
    /// Checked in person or over a trusted channel.
    Verified,
}

impl Verification {
    const fn code(self) -> i64 {
        match self {
            Self::Unverified => 0,
            Self::Verified => 1,
        }
    }

    fn from_code(code: i64) -> Result<Self, StoreError> {
        match code {
            0 => Ok(Self::Unverified),
            1 => Ok(Self::Verified),
            _ => Err(StoreError::Corrupt {
                field: "member.verified",
            }),
        }
    }
}

/// A member of a Party (design C.2 `member`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The Party.
    pub party: PartyId,
    /// The member's leaf in the Party group.
    pub leaf: LeafIndex,
    /// The member's account key (their leaf signature key).
    pub signature_key: AccountPublicKey,
    /// Their display name.
    pub name: MemberName,
    /// Their avatar, if they set one.
    pub avatar: Option<Avatar>,
    /// Whether this device verified them.
    pub verification: Verification,
}

/// Repository for the `member` table.
pub struct Members<'session> {
    session: &'session Session,
}

const MEMBER_COLUMNS: &str = "party_id, leaf_index, sig_pk, display_name, avatar, verified";

fn member_from_row(row: &Row<'_>) -> Result<Member, StoreError> {
    Ok(Member {
        party: PartyId::from_bytes(read_array(row, 0, "member.party_id")?),
        leaf: LeafIndex::new(narrow(row.get(1)?, "member.leaf_index")?),
        signature_key: AccountPublicKey::from_bytes(read_array(row, 2, "member.sig_pk")?),
        name: MemberName::new(row.get(3)?).map_err(|_| StoreError::Corrupt {
            field: "member.display_name",
        })?,
        avatar: row.get::<_, Option<Vec<u8>>>(4)?.map(Avatar::from_bytes),
        verification: Verification::from_code(row.get(5)?)?,
    })
}

impl Members<'_> {
    /// Adds a member, or replaces the member at that leaf.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist.
    pub fn put(&self, member: &Member) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                &format!(
                    "INSERT OR REPLACE INTO member ({MEMBER_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                ),
                params![
                    member.party.as_bytes(),
                    member.leaf.get(),
                    member.signature_key.as_bytes(),
                    member.name.as_str(),
                    member.avatar.as_ref().map(Avatar::as_bytes),
                    member.verification.code(),
                ],
            )?;
            Ok(())
        })
    }

    /// The member at `leaf`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, party: &PartyId, leaf: LeafIndex) -> Result<Option<Member>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!(
                        "SELECT {MEMBER_COLUMNS} FROM member \
                         WHERE party_id = ?1 AND leaf_index = ?2"
                    ),
                    params![party.as_bytes(), leaf.get()],
                    |row| Ok(member_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// The Party's members by leaf.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, party: &PartyId) -> Result<Vec<Member>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {MEMBER_COLUMNS} FROM member WHERE party_id = ?1 ORDER BY leaf_index"
            ))?;
            let rows = statement.query_map([party.as_bytes()], |row| Ok(member_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Records whether a member is verified.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such member.
    pub fn set_verification(
        &self,
        party: &PartyId,
        leaf: LeafIndex,
        verification: Verification,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE member SET verified = ?3 WHERE party_id = ?1 AND leaf_index = ?2",
                    params![party.as_bytes(), leaf.get(), verification.code()],
                )?,
                "member",
            )
        })
    }

    /// Removes a member (after a Remove commit).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such member.
    pub fn remove(&self, party: &PartyId, leaf: LeafIndex) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "DELETE FROM member WHERE party_id = ?1 AND leaf_index = ?2",
                    params![party.as_bytes(), leaf.get()],
                )?,
                "member",
            )
        })
    }
}

/// Repository for the `always_allow` table: members whose location requests are approved
/// automatically in any Meetup of their Party (design B.6.1). Never leaves the device.
pub struct AlwaysAllow<'session> {
    session: &'session Session,
}

impl AlwaysAllow<'_> {
    /// Adds a member to the Party's allow-list; adding twice is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist.
    pub fn add(&self, party: &PartyId, member: &AccountPublicKey) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO always_allow (party_id, member_sig_pk) VALUES (?1, ?2)",
                params![party.as_bytes(), member.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Removes a member from the allow-list; removing an absent member is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn remove(&self, party: &PartyId, member: &AccountPublicKey) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM always_allow WHERE party_id = ?1 AND member_sig_pk = ?2",
                params![party.as_bytes(), member.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Whether the member is on the Party's allow-list.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the query fails.
    pub fn contains(&self, party: &PartyId, member: &AccountPublicKey) -> Result<bool, StoreError> {
        self.session.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT 1 FROM always_allow WHERE party_id = ?1 AND member_sig_pk = ?2",
                    params![party.as_bytes(), member.as_bytes()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use zeroize::Zeroizing;

    use super::*;
    use crate::test_support::{Fixture, TestError, party};

    fn member(party: &Party, leaf: u32) -> Result<Member, TestError> {
        Ok(Member {
            party: party.id,
            leaf: LeafIndex::new(leaf),
            signature_key: AccountPublicKey::from_bytes([u8::try_from(leaf).unwrap_or(0); 32]),
            name: MemberName::new(format!("Member {leaf}"))?,
            avatar: Some(Avatar::from_bytes(vec![1, 2, 3])),
            verification: Verification::Unverified,
        })
    }

    #[test]
    fn identity_round_trips() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let identities = fixture.session.identity();
        assert!(identities.load()?.is_none());
        let identity = Identity {
            account: AccountId::from_bytes([3; 16]),
            signing_key: AccountSigningKey::from_bytes(Zeroizing::new([4; 32])),
        };
        identities.save(&identity)?;
        let loaded = identities
            .load()?
            .ok_or(StoreError::NotFound { table: "identity" })?;
        assert_eq!(loaded.account, identity.account);
        assert_eq!(loaded.signing_key.expose_bytes(), &[4; 32]);
        // Saving again replaces the single row.
        identities.save(&Identity {
            account: AccountId::from_bytes([9; 16]),
            ..identity
        })?;
        assert_eq!(fixture.rows("identity")?, 1);
        assert_eq!(
            identities.load()?.map(|loaded| loaded.account),
            Some(AccountId::from_bytes([9; 16]))
        );
        Ok(())
    }

    #[test]
    fn party_round_trips_and_is_found_by_group() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let parties = fixture.session.parties();
        let mut first = party(1)?;
        first.location_visibility = Some(LocationVisibility::Nobody);
        let second = party(2)?;
        parties.insert(&second)?;
        parties.insert(&first)?;
        assert_eq!(parties.get(&first.id)?, Some(first.clone()));
        assert_eq!(parties.by_group(&second.group)?, Some(second.clone()));
        assert_eq!(parties.get(&PartyId::from_bytes([9; 16]))?, None);
        assert_eq!(parties.list()?, vec![first.clone(), second]);
        assert!(parties.insert(&first).is_err());
        Ok(())
    }

    #[test]
    fn party_updates_change_only_their_party() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let parties = fixture.session.parties();
        let first = party(1)?;
        let second = party(2)?;
        parties.insert(&first)?;
        parties.insert(&second)?;
        let renamed = PartyName::new("Renamed".into())?;
        parties.rename(&first.id, &renamed)?;
        parties.set_own_leaf(&first.id, LeafIndex::new(42))?;
        parties.set_location_visibility(&first.id, Some(LocationVisibility::Everyone))?;
        let updated = parties.get(&first.id)?;
        assert_eq!(
            updated,
            Some(Party {
                name: renamed,
                own_leaf: LeafIndex::new(42),
                location_visibility: Some(LocationVisibility::Everyone),
                ..first.clone()
            })
        );
        parties.set_location_visibility(&first.id, None)?;
        assert_eq!(
            parties
                .get(&first.id)?
                .and_then(|party| party.location_visibility),
            None
        );
        assert_eq!(parties.get(&second.id)?, Some(second));
        Ok(())
    }

    #[test]
    fn updating_a_missing_party_is_not_found() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let parties = fixture.session.parties();
        let missing = PartyId::from_bytes([8; 16]);
        let not_found = |result: Result<(), StoreError>| {
            matches!(result, Err(StoreError::NotFound { table: "party" }))
        };
        assert!(not_found(
            parties.rename(&missing, &PartyName::new("x".into())?)
        ));
        assert!(not_found(parties.set_own_leaf(&missing, LeafIndex::new(1))));
        assert!(not_found(parties.set_location_visibility(&missing, None)));
        assert!(not_found(parties.delete(&missing)));
        Ok(())
    }

    #[test]
    fn deleting_a_party_deletes_its_members_and_allow_list() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let doomed = party(1)?;
        let kept = party(2)?;
        session.parties().insert(&doomed)?;
        session.parties().insert(&kept)?;
        session.members().put(&member(&doomed, 0)?)?;
        session.members().put(&member(&kept, 0)?)?;
        session
            .always_allow()
            .add(&doomed.id, &AccountPublicKey::from_bytes([1; 32]))?;
        session.parties().delete(&doomed.id)?;
        assert_eq!(session.parties().list()?, vec![kept.clone()]);
        assert_eq!(fixture.rows("member")?, 1);
        assert_eq!(fixture.rows("always_allow")?, 0);
        Ok(())
    }

    #[test]
    fn members_round_trip_in_leaf_order() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        let later = member(&owner, 5)?;
        let mut earlier = member(&owner, 2)?;
        earlier.avatar = None;
        session.members().put(&later)?;
        session.members().put(&earlier)?;
        assert_eq!(
            session.members().list(&owner.id)?,
            vec![earlier.clone(), later.clone()]
        );
        assert_eq!(
            session.members().get(&owner.id, LeafIndex::new(5))?,
            Some(later.clone())
        );
        assert_eq!(session.members().get(&owner.id, LeafIndex::new(3))?, None);
        // put replaces the member at that leaf.
        let replacement = Member {
            name: MemberName::new("New".into())?,
            ..later
        };
        session.members().put(&replacement)?;
        assert_eq!(
            session.members().get(&owner.id, LeafIndex::new(5))?,
            Some(replacement)
        );
        Ok(())
    }

    #[test]
    fn member_verification_and_removal() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        session.members().put(&member(&owner, 1)?)?;
        session.members().put(&member(&owner, 2)?)?;
        let members = session.members();
        members.set_verification(&owner.id, LeafIndex::new(1), Verification::Verified)?;
        let verified = members.get(&owner.id, LeafIndex::new(1))?;
        assert_eq!(
            verified.map(|member| member.verification),
            Some(Verification::Verified)
        );
        let other = members.get(&owner.id, LeafIndex::new(2))?;
        assert_eq!(
            other.map(|member| member.verification),
            Some(Verification::Unverified)
        );
        members.remove(&owner.id, LeafIndex::new(1))?;
        assert_eq!(members.get(&owner.id, LeafIndex::new(1))?, None);
        assert_eq!(members.list(&owner.id)?.len(), 1);
        assert!(matches!(
            members.remove(&owner.id, LeafIndex::new(1)),
            Err(StoreError::NotFound { table: "member" })
        ));
        assert!(matches!(
            members.set_verification(&owner.id, LeafIndex::new(9), Verification::Verified),
            Err(StoreError::NotFound { table: "member" })
        ));
        Ok(())
    }

    #[test]
    fn a_member_needs_an_existing_party() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        assert!(
            fixture
                .session
                .members()
                .put(&member(&party(1)?, 0)?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn allow_list_is_per_party() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let first = party(1)?;
        let second = party(2)?;
        session.parties().insert(&first)?;
        session.parties().insert(&second)?;
        let chloe = AccountPublicKey::from_bytes([7; 32]);
        let allow = session.always_allow();
        assert!(!allow.contains(&first.id, &chloe)?);
        allow.add(&first.id, &chloe)?;
        allow.add(&first.id, &chloe)?;
        assert!(allow.contains(&first.id, &chloe)?);
        assert!(!allow.contains(&second.id, &chloe)?);
        allow.remove(&first.id, &chloe)?;
        assert!(!allow.contains(&first.id, &chloe)?);
        Ok(())
    }
}
