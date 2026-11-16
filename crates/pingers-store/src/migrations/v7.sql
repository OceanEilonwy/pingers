-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 7: pingers-parties' invites, waiting join requests, joins in progress and how
-- this device came into a Party, which it kept as key-value rows under `pingers-parties/v1/`
-- until join flow A settled (pingers-parties request 2).

-- An invite this device made, by its mailbox. The secret is the link's; it goes with the invite,
-- and the invite goes with its Party. status: 0 open, 1 used (admitted_reply names the joiner
-- let in), 2 cancelled. read_position: the last post read from the mailbox, NULL before any.
CREATE TABLE party_invite (
    mailbox_id     BLOB PRIMARY KEY CHECK (length(mailbox_id) = 16),
    party_id       BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    secret         BLOB NOT NULL CHECK (length(secret) = 32),
    shared_at      INTEGER NOT NULL CHECK (shared_at >= 0),
    expires_at     INTEGER NOT NULL CHECK (expires_at >= 0),
    status         INTEGER NOT NULL CHECK (status IN (0, 1, 2)),
    admitted_reply BLOB CHECK (admitted_reply IS NULL OR length(admitted_reply) = 16),
    read_position  INTEGER CHECK (read_position IS NULL OR read_position >= 0),
    CHECK ((status = 1) = (admitted_reply IS NOT NULL))
) STRICT;

CREATE INDEX party_invite_by_party ON party_invite (party_id);

-- A join request waiting for the inviter, by the joiner's reply mailbox; `request` is the
-- pingers-proto JoinRequest as it arrived. It goes with the invite it came through.
CREATE TABLE join_approval (
    party_id       BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    reply_mailbox  BLOB NOT NULL CHECK (length(reply_mailbox) = 16),
    invite_mailbox BLOB NOT NULL REFERENCES party_invite (mailbox_id) ON DELETE CASCADE,
    asked_at       INTEGER NOT NULL CHECK (asked_at >= 0),
    request        BLOB NOT NULL,
    PRIMARY KEY (party_id, reply_mailbox)
) STRICT;

CREATE INDEX join_approval_by_invite ON join_approval (invite_mailbox);

-- A join this device asked for, by its reply mailbox, until the Welcome is accepted or the
-- request withdrawn. There is no Party row yet. `payload` is the link's pingers-proto
-- InvitePayload (it holds the invite secret); the key packages are the ones the request carried.
CREATE TABLE party_join (
    reply_mailbox      BLOB PRIMARY KEY CHECK (length(reply_mailbox) = 16),
    payload            BLOB NOT NULL,
    display_name       TEXT NOT NULL CHECK (length(display_name) > 0),
    sub_id             BLOB CHECK (sub_id IS NULL OR length(sub_id) = 16),
    key_package        BLOB NOT NULL,
    x_wing_key_package BLOB,
    asked_at           INTEGER NOT NULL CHECK (asked_at >= 0)
) STRICT;

-- Who let this device into a Party it joined, and the join code both phones showed.
CREATE TABLE joined_via (
    party_id     BLOB PRIMARY KEY REFERENCES party (party_id) ON DELETE CASCADE,
    inviter_leaf INTEGER NOT NULL CHECK (inviter_leaf >= 0),
    join_code    INTEGER NOT NULL CHECK (join_code >= 0 AND join_code < 1000000000000)
) STRICT;
