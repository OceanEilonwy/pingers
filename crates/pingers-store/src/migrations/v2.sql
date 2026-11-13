-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 2: the presence snapshot keeps when Inside was last confirmed, so the
-- 60-minute Inside timeout survives a restart (pingers-geo `PresenceSnapshot::confirmed_at`,
-- wave 1 review). NULL outside Inside, and for rows saved before this version, which
-- pingers-geo restores as before.

ALTER TABLE presence ADD COLUMN confirmed_at INTEGER CHECK (confirmed_at >= 0);
