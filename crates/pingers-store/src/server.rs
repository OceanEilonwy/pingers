// SPDX-License-Identifier: AGPL-3.0-only
//! `server` and `server_relay`: the Pingers servers this device has Parties on, and
//! [`ServerSession`], the only way to reach a server's own rows.
//!
//! A device can be in Parties on different servers at once (decided 2026-11-14). Each server
//! has its own account registration and bearer, Privacy Pass stock, RLN identity, counters and
//! tree, mailboxes and subscriptions, request queue, board upload, push relays and fallbacks.
//! Those tables carry a `server_key` column, and their repositories are methods of
//! [`ServerSession`] rather than [`Session`]: code that holds a plain session cannot read or
//! write one server's credentials by mistake while talking to another.
//!
//! A server row holds what the client needs to reach the server again after a restart: its
//! OHTTP relays, its push relay, the key pins its gateway and push relay must match, and how
//! those pins were first recorded ([`ServerTrust`]). Deleting a server deletes all its rows.

use core::ops::Deref;

use pingers_proto::{KeyConfigHash, PushHandle, ServerId};
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::codec::read_optional_array;
use crate::repo::expect_one;
use crate::{Session, StoreError};

/// How a server's key pins were first recorded. Nothing is pinned on first use: every server
/// a device talks to was trusted in one of these ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerTrust {
    /// The app's built-in server: its pins come from the build and are refreshed from it at
    /// every start.
    BuiltIn,
    /// The pins came in an invite link, from an inviter who already trusted the server.
    Invite,
    /// The user looked at the server's keys and chose to trust them, when creating a Party on
    /// it.
    Confirmed,
}

impl ServerTrust {
    const fn code(self) -> i64 {
        match self {
            Self::BuiltIn => 0,
            Self::Invite => 1,
            Self::Confirmed => 2,
        }
    }

    fn from_code(code: i64) -> Result<Self, StoreError> {
        match code {
            0 => Ok(Self::BuiltIn),
            1 => Ok(Self::Invite),
            2 => Ok(Self::Confirmed),
            _ => Err(StoreError::Corrupt {
                field: "server.trust",
            }),
        }
    }
}

/// A server this device knows (`server` row and its `server_relay` rows).
///
/// The relay and push relay addresses are plain text: they are checked by pingers-transport
/// when a connection is made, and a debug build's local stack may use `http://` relays, which
/// pingers-proto's `RelayUrl` refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerRecord {
    /// The server's origin, also its OHTTP gateway.
    pub server: ServerId,
    /// The OHTTP relays to reach it through, in order of preference.
    pub ohttp_relays: Vec<String>,
    /// The push relay its wake-up pushes go through, if any.
    pub push_relay: Option<String>,
    /// The gateway key config hashes allowed (the key served now and the next one).
    pub gateway_pins: Vec<KeyConfigHash>,
    /// The push relay key config hashes allowed.
    pub push_relay_pins: Vec<KeyConfigHash>,
    /// How the pins were recorded.
    pub trust: ServerTrust,
}

/// What a push handle belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HandleKind {
    /// One of this device's mailboxes.
    Mailbox,
    /// One of this device's group subscriptions.
    Subscription,
}

/// The server and kind of a push handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleOwner {
    /// The server the mailbox or subscription is registered on.
    pub server: ServerId,
    /// Mailbox or subscription.
    pub kind: HandleKind,
}

/// A [`Session`] scoped to one server's rows. Dereferences to the session for device-wide
/// tables; the per-server repositories are its own methods.
pub struct ServerSession<'session> {
    session: &'session Session,
    server: ServerId,
    key: i64,
}

impl Session {
    /// The rows of `server`.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownServer`] if the server has no row ([`Servers::put`] first).
    pub fn server(&self, server: &ServerId) -> Result<ServerSession<'_>, StoreError> {
        let key = self.with(|connection| server_key(connection, server))?;
        Ok(ServerSession {
            session: self,
            server: server.clone(),
            key,
        })
    }

    /// The servers this device knows (`server` table).
    #[must_use]
    pub fn servers(&self) -> Servers<'_> {
        Servers { session: self }
    }

    /// Which server and which kind of registration a push handle belongs to, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if the server's origin does not decode.
    pub fn push_handle_owner(
        &self,
        handle: &PushHandle,
    ) -> Result<Option<HandleOwner>, StoreError> {
        self.with(|connection| {
            let row = connection
                .query_row(
                    "SELECT origin, 0 FROM mailbox JOIN server USING (server_key) \
                     WHERE handle = ?1 \
                     UNION ALL \
                     SELECT origin, 1 FROM subscription JOIN server USING (server_key) \
                     WHERE handle = ?1 LIMIT 1",
                    [handle.as_bytes()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            row.map(|(origin, kind)| {
                Ok(HandleOwner {
                    server: origin_of(origin)?,
                    kind: if kind == 0 {
                        HandleKind::Mailbox
                    } else {
                        HandleKind::Subscription
                    },
                })
            })
            .transpose()
        })
    }
}

impl ServerSession<'_> {
    /// The server these rows belong to.
    #[must_use]
    pub const fn server(&self) -> &ServerId {
        &self.server
    }

    /// The session and the server's key, for the per-server repositories.
    pub(crate) const fn scope(&self) -> Scope<'_> {
        Scope {
            session: self.session,
            key: self.key,
        }
    }

    /// Runs `body` as one atomic unit, as [`Session::transaction`] does, with the same scope.
    ///
    /// # Errors
    ///
    /// As [`Session::transaction`].
    pub fn transaction<T, E: From<StoreError>>(
        &self,
        body: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E> {
        self.session.transaction(|_session| body(self))
    }
}

impl Deref for ServerSession<'_> {
    type Target = Session;

    fn deref(&self) -> &Session {
        self.session
    }
}

/// What a per-server repository holds: the session and the server's row key.
#[derive(Clone, Copy)]
pub(crate) struct Scope<'session> {
    session: &'session Session,
    key: i64,
}

impl Scope<'_> {
    /// The server's row key, bound into every query.
    pub(crate) const fn key(self) -> i64 {
        self.key
    }

    /// Runs `operation` on the connection.
    pub(crate) fn with<T>(
        self,
        operation: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.session.with(operation)
    }

    /// Runs `body` as one atomic unit ([`Session::transaction`]).
    pub(crate) fn transaction<T, E: From<StoreError>>(
        self,
        body: impl FnOnce(Self) -> Result<T, E>,
    ) -> Result<T, E> {
        self.session.transaction(|_session| body(self))
    }
}

/// The row key of `server`.
pub(crate) fn server_key(connection: &Connection, server: &ServerId) -> Result<i64, StoreError> {
    connection
        .query_row(
            "SELECT server_key FROM server WHERE origin = ?1",
            [server.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StoreError::UnknownServer)
}

/// A stored origin as a [`ServerId`].
pub(crate) fn origin_of(origin: String) -> Result<ServerId, StoreError> {
    ServerId::new(origin).map_err(|_| StoreError::Corrupt {
        field: "server.origin",
    })
}

/// Pins as stored: their hashes back to back.
fn pins_blob(pins: &[KeyConfigHash]) -> Vec<u8> {
    pins.iter().flat_map(|pin| *pin.as_bytes()).collect()
}

pub(crate) fn pins_from_blob(
    blob: &[u8],
    field: &'static str,
) -> Result<Vec<KeyConfigHash>, StoreError> {
    let (chunks, rest) = blob.as_chunks::<32>();
    if !rest.is_empty() {
        return Err(StoreError::Corrupt { field });
    }
    Ok(chunks
        .iter()
        .map(|chunk| KeyConfigHash::from_bytes(*chunk))
        .collect())
}

/// Repository for the `server` and `server_relay` tables.
pub struct Servers<'session> {
    session: &'session Session,
}

impl Servers<'_> {
    /// Adds a server, or replaces the configuration of one already known, keeping its rows
    /// (account, tokens, mailboxes, Parties) and the server pin recorded with its account.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn put(&self, record: &ServerRecord) -> Result<(), StoreError> {
        self.session.transaction(|session| {
            session.with(|connection| {
                connection.execute(
                    "INSERT INTO server (origin, push_relay, gateway_pins, push_relay_pins, \
                     trust) VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (origin) DO UPDATE SET push_relay = excluded.push_relay, \
                     gateway_pins = excluded.gateway_pins, \
                     push_relay_pins = excluded.push_relay_pins, trust = excluded.trust",
                    params![
                        record.server.as_str(),
                        record.push_relay,
                        pins_blob(&record.gateway_pins),
                        pins_blob(&record.push_relay_pins),
                        record.trust.code(),
                    ],
                )?;
                let key = server_key(connection, &record.server)?;
                connection.execute("DELETE FROM server_relay WHERE server_key = ?1", [key])?;
                for (position, relay) in record.ohttp_relays.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO server_relay (server_key, position, url) VALUES (?1, ?2, ?3)",
                        params![
                            key,
                            i64::try_from(position).map_err(|_| StoreError::OutOfRange {
                                field: "server_relay.position",
                            })?,
                            relay
                        ],
                    )?;
                }
                Ok(())
            })
        })
    }

    /// The server with this origin, if known.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, server: &ServerId) -> Result<Option<ServerRecord>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|record| record.server == *server))
    }

    /// Every known server, in the order they were first added.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<ServerRecord>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT server_key, origin, push_relay, gateway_pins, push_relay_pins, trust \
                 FROM server ORDER BY server_key",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            let mut records = Vec::new();
            for row in rows {
                let (key, origin, push_relay, gateway_pins, push_relay_pins, trust) = row?;
                let mut relays = connection.prepare(
                    "SELECT url FROM server_relay WHERE server_key = ?1 ORDER BY position",
                )?;
                let ohttp_relays = relays
                    .query_map([key], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                records.push(ServerRecord {
                    server: origin_of(origin)?,
                    ohttp_relays,
                    push_relay,
                    gateway_pins: pins_from_blob(&gateway_pins, "server.gateway_pins")?,
                    push_relay_pins: pins_from_blob(&push_relay_pins, "server.push_relay_pins")?,
                    trust: ServerTrust::from_code(trust)?,
                });
            }
            Ok(records)
        })
    }

    /// The gateway key config hash recorded when the account was made on this server (design
    /// C.2 `server_pin`), if it has been.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownServer`]; [`StoreError::Corrupt`].
    pub fn server_pin(&self, server: &ServerId) -> Result<Option<KeyConfigHash>, StoreError> {
        self.session.with(|connection| {
            let key = server_key(connection, server)?;
            Ok(connection
                .query_row(
                    "SELECT server_pin FROM server WHERE server_key = ?1",
                    [key],
                    |row| Ok(read_optional_array(row, 0, "server.server_pin")),
                )??
                .map(KeyConfigHash::from_bytes))
        })
    }

    /// Records the gateway key config hash the account was made with.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the server is not known.
    pub fn set_server_pin(&self, server: &ServerId, pin: &KeyConfigHash) -> Result<(), StoreError> {
        self.session.with(|connection| {
            let changed = connection.execute(
                "UPDATE server SET server_pin = ?1 WHERE origin = ?2",
                params![pin.as_bytes(), server.as_str()],
            )?;
            expect_one(changed, "server")
        })
    }

    /// Forgets a server and every row that belongs to it: its account standing, tokens, RLN
    /// state, mailboxes, subscriptions, queued requests and Parties.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the server is not known.
    pub fn delete(&self, server: &ServerId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            let changed =
                connection.execute("DELETE FROM server WHERE origin = ?1", [server.as_str()])?;
            expect_one(changed, "server")
        })
    }
}
