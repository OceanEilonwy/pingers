// SPDX-License-Identifier: AGPL-3.0-only
//! `account_registration` and `account_bearer`: the account's standing with its server.

use pingers_proto::SecretText;
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, params};

use crate::codec::{read_time, time};
use crate::server::Scope;
use crate::{ServerSession, StoreError};

impl ServerSession<'_> {
    /// The account's registration and bearer on this server (`account_registration`,
    /// `account_bearer`).
    #[must_use]
    pub fn account(&self) -> Accounts<'_> {
        Accounts {
            session: self.scope(),
        }
    }
}

/// Whether the account is registered with its server (design B.1), and with which code.
#[derive(Clone, Debug)]
pub enum AccountRegistration {
    /// `POST /v1/accounts` is still to be done, presenting `code` if there is one.
    Pending {
        /// The operator's registration code to present.
        code: Option<SecretText>,
    },
    /// The server took the registration.
    Registered {
        /// The code it took, which this device's invite links pass on.
        code: Option<SecretText>,
    },
    /// The server refused the registration (403): it is not tried again with the same code.
    Refused {
        /// The code it refused, or `None` if none was presented.
        code: Option<SecretText>,
    },
}

/// The account's bearer from `POST /v1/auth`. Not `Debug`: it is a credential.
pub struct AccountBearer {
    /// The bearer value, sent as `Authorization: Bearer <value>`.
    pub bearer: SecretText,
    /// When the server stops accepting it.
    pub expires_at: UnixSeconds,
}

/// Repository for the account tables.
pub struct Accounts<'session> {
    session: Scope<'session>,
}

const PENDING: i64 = 0;
const REGISTERED: i64 = 1;
const REFUSED: i64 = 2;

impl Accounts<'_> {
    /// The registration; [`AccountRegistration::Pending`] without a code if none is recorded.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if the stored state is unknown.
    pub fn registration(&self) -> Result<AccountRegistration, StoreError> {
        self.session.with(|connection| {
            let row = connection
                .query_row(
                    "SELECT state, code FROM account_registration WHERE server_key = ?1",
                    [self.session.key()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()?;
            let Some((state, code)) = row else {
                return Ok(AccountRegistration::Pending { code: None });
            };
            let code = code.map(SecretText::new);
            match state {
                PENDING => Ok(AccountRegistration::Pending { code }),
                REGISTERED => Ok(AccountRegistration::Registered { code }),
                REFUSED => Ok(AccountRegistration::Refused { code }),
                _ => Err(StoreError::Corrupt {
                    field: "account_registration.state",
                }),
            }
        })
    }

    /// Records the registration, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails (an empty code is refused by the schema).
    pub fn set_registration(&self, registration: &AccountRegistration) -> Result<(), StoreError> {
        let (state, code) = match registration {
            AccountRegistration::Pending { code } => (PENDING, code),
            AccountRegistration::Registered { code } => (REGISTERED, code),
            AccountRegistration::Refused { code } => (REFUSED, code),
        };
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO account_registration (server_key, state, code) \
                 VALUES (?1, ?2, ?3)",
                params![
                    self.session.key(),
                    state,
                    code.as_ref().map(SecretText::expose)
                ],
            )?;
            Ok(())
        })
    }

    /// The stored bearer, if any (expired or not: the caller compares with its clock).
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn bearer(&self) -> Result<Option<AccountBearer>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT bearer, expires_at FROM account_bearer WHERE server_key = ?1",
                    [self.session.key()],
                    |row| {
                        Ok((|| {
                            Ok::<_, StoreError>(AccountBearer {
                                bearer: SecretText::new(row.get(0)?),
                                expires_at: read_time(row, 1, "account_bearer.expires_at")?,
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }

    /// Stores `bearer`, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] for an expiry past the column's range;
    /// [`StoreError::Database`] if the write fails.
    pub fn set_bearer(&self, bearer: &AccountBearer) -> Result<(), StoreError> {
        let expires_at = time(bearer.expires_at, "account_bearer.expires_at")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO account_bearer (server_key, bearer, expires_at) \
                 VALUES (?1, ?2, ?3)",
                params![self.session.key(), bearer.bearer.expose(), expires_at],
            )?;
            Ok(())
        })
    }

    /// Forgets the bearer (the server no longer accepts it).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn forget_bearer(&self) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM account_bearer WHERE server_key = ?1",
                [self.session.key()],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError};

    fn code(text: &str) -> SecretText {
        SecretText::new(text.to_owned())
    }

    fn exposed(registration: &AccountRegistration) -> (&'static str, Option<String>) {
        match registration {
            AccountRegistration::Pending { code } => {
                ("pending", code.as_ref().map(|c| c.expose().to_owned()))
            }
            AccountRegistration::Registered { code } => {
                ("registered", code.as_ref().map(|c| c.expose().to_owned()))
            }
            AccountRegistration::Refused { code } => {
                ("refused", code.as_ref().map(|c| c.expose().to_owned()))
            }
        }
    }

    #[test]
    fn registration_starts_pending_and_keeps_its_state_and_code() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let accounts = server.account();
        assert_eq!(exposed(&accounts.registration()?), ("pending", None));
        for (registration, expected) in [
            (
                AccountRegistration::Pending {
                    code: Some(code("invite")),
                },
                ("pending", Some("invite".to_owned())),
            ),
            (
                AccountRegistration::Refused {
                    code: Some(code("old")),
                },
                ("refused", Some("old".to_owned())),
            ),
            (
                AccountRegistration::Refused { code: None },
                ("refused", None),
            ),
            (
                AccountRegistration::Registered {
                    code: Some(code("good")),
                },
                ("registered", Some("good".to_owned())),
            ),
        ] {
            accounts.set_registration(&registration)?;
            assert_eq!(exposed(&accounts.registration()?), expected);
        }
        assert_eq!(fixture.rows("account_registration")?, 1);
        Ok(())
    }

    #[test]
    fn an_unknown_registration_state_is_corrupt() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        fixture.session.with(|connection| {
            connection.execute_batch(
                "PRAGMA ignore_check_constraints = ON; \
                 INSERT INTO account_registration (server_key, state) VALUES (1, 7);",
            )?;
            Ok(())
        })?;
        assert!(matches!(
            fixture.server()?.account().registration(),
            Err(StoreError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn the_bearer_is_kept_replaced_and_forgotten() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let accounts = server.account();
        assert!(accounts.bearer()?.is_none());
        accounts.set_bearer(&AccountBearer {
            bearer: SecretText::new("first".to_owned()),
            expires_at: UnixSeconds::new(100),
        })?;
        accounts.set_bearer(&AccountBearer {
            bearer: SecretText::new("second".to_owned()),
            expires_at: UnixSeconds::new(200),
        })?;
        let stored = accounts.bearer()?.ok_or(StoreError::NotFound {
            table: "account_bearer",
        })?;
        assert_eq!(stored.bearer.expose(), "second");
        assert_eq!(stored.expires_at, UnixSeconds::new(200));
        accounts.forget_bearer()?;
        assert!(accounts.bearer()?.is_none());
        Ok(())
    }
}
