# Wave 1 review

Status: 2026-11-13, branch `w2-review`, reviewing `main` at 708181f.

Scope: every wave 1 crate against `docs/style-guide.md`, `docs/design.md` and the CONTRACT
Decisions, plus `deny.toml`, `vendor/ohttp` and the tests. Line numbers are those on `main`.

Each finding is marked **fixed in this branch** (with the commit) or **for the owner** (naming
the crate). "Needs a decision" means the fix changes a contract, a design value or another
crate, so a human or the owning agent should choose.

## Summary

| Severity | Found | Fixed here | For the owner |
|---|---|---|---|
| High | 6 | 4 | 2 |
| Medium | 27 | 10 | 17 |
| Low | 39 | 10 | 29 |
| **Total** | **72** | **24** | **48** |

The five that matter most:

1. **pingers-mls (high, fixed):** a Meetup Welcome with a 33-byte group id was accepted and
   aliased another group's id, so a Party insider could make a victim delete a live Meetup.
2. **pingers-store (high, fixed):** ending a Meetup left its MLS epoch secrets on disk unless
   every caller also remembered `delete_group`.
3. **vendor/ohttp (high, fixed):** upstream `trace!` lines logged HKDF output (the response AEAD
   key and nonce) and generated private keys; any trace-level logger would have recorded them.
4. **pingers-crypto (high, for the owner):** `seal_join_request` does not pad, so a sealed join
   request never fits a mailbox size class and leaks the request size.
5. **xtask (high, partly fixed):** `cargo xtask ci` skipped `cargo deny` when it was not
   installed (fixed) and still runs neither `cargo mutants` nor the fuzz targets (for a human).

Checked and found sound: the chunked-OHTTP patch (nonce `max(Nn, Nk)`, final-chunk AAD,
sequence overflow, empty non-final chunks, truncation), no system RNG left in `vendor/ohttp`,
the crypto derivations and sizes against design B.4.1, B.4.2, B.4.9 and B.6.1 byte for byte,
`delete_prefix` and group-prefix isolation in the store, and the claim that pingers-anon never
reaches rln's sled-backed `PmTree`.

---

## deny.toml and vendor/ohttp

- **High: `vendor/ohttp/src/rh/hkdf.rs:63-105`, `rh/hpke.rs:713,733`, `src/lib.rs:64`.**
  `expand_data` traced `hex::encode(&okm)`, which is the response AEAD key and nonce;
  `extract` traced the salt and input; key generation traced private keys through `Debug`. The
  patch removed `unsafe-print-secrets` but kept these lines. **Fixed in this branch** (3b0c1b8):
  every `trace!` and the `log` dependency removed; README updated.
- **Medium: `vendor/ohttp/src/rh/mod.rs:12`.** `SymKey(Vec<u8>)` holds exported secrets,
  PRKs and AEAD keys for the life of a chunked exchange and was not zeroed. **Fixed in this
  branch** (3b0c1b8): `Drop` zeroizes it.
- **Low: `deny.toml:12` (RUSTSEC-2026-0387).** The reason "Pingers uses no P-256 MLS suite"
  was not true: `KeyPackageIn::validate` verified untrusted KeyPackage signatures with whatever
  suite they named, P-256 included. **Fixed in this branch** (3a73cfa, 6732bc5): pingers-mls
  refuses other suites before validating, and the reason now says so.
- **Low: `vendor/ohttp/src/chunked.rs:121-125`.** The ChaCha20-Poly1305 32-byte response nonce
  fix (half of upstream issue #102) is never exercised: Pingers pins AES-128-GCM and upstream's
  unit tests were dropped, though the README says pingers-ohttp's tests cover the patched code.
  Add a ChaCha20-Poly1305 chunked round trip before upstreaming. **For the owner**
  (pingers-ohttp).
- The rln advisories (sled, zstd-safe, fxhash, instant, tracing-subscriber, derivative) are
  justified: pingers-anon builds only `ArkGroth16Backend` from the embedded zkey and never a
  `PmTree`, `RLN::new` or a default tree.

## pingers-time

- **Medium: `src/lib.rs:18,26`.** `ManualClock` compiled only with feature `test-util`, which
  nothing enables for the crate's own tests, so `cargo test -p pingers-time` and `cargo mutants
  -p pingers-time` never ran its six tests. **Fixed in this branch** (e9b2783):
  `cfg(any(test, feature = "test-util"))`.
- **Low: `src/unix.rs:50`, `src/bucket.rs:89,104`, `src/timer.rs:27-50`.** `checked_sub` and
  `previous` were tested only on the `None` path; `MonotonicTime` had no tests. **Fixed in this
  branch** (809a068).
- **Low: `src/manual.rs:115-128`.** `Sleep`'s doc says dropping it cancels the sleep, but
  `ManualSleep` has no `Drop`, so a dropped sleep's waker stays registered until the next
  `advance`, and `select!` loops pile them up in tests. Remove the registration on drop, or
  document lazy cancellation. **For the owner** (pingers-time; small, but `ManualClock` is used
  by every wave 2 test, so not changed under them).
- **Low: `docs/style-guide.md` §6.** Says tests use "the manual clock from pingers-testkit";
  it lives in pingers-time behind `test-util`. **For a human** (one-line doc edit).

## pingers-geo

- **Medium: `src/fence.rs:120`, `src/plane.rs:318-345`.** `Polygon::new` had no vertex limit
  while its checks are O(n²) and O(n³); polygons come from other Party members and
  pingers-store rebuilds them on every load (up to 65,535 vertices). **Fixed in this branch**
  (f87cb48): `MAXIMUM_POLYGON_VERTICES = 100` and `GeoError::TooManyVertices`. **Needs a
  decision** on the value; pingers-proto and the UI should enforce the same number.
- **Medium: `src/presence.rs:216-227` (`restore`).** After a restart the Inside timeout runs
  from `last_fix_at`, which every fix sets, including one beyond the exit buffer that the live
  machine does not count. A user walking away when the app restarts stays Inside (posting real
  records) for up to 60 more minutes, and the exit evidence is forgotten; the CONTRACT calls
  this conservative, but it is less private. Store `confirmed_at` in `PresenceSnapshot`, or
  run the timeout from `since`. **For a human** (changes the design C.2 `presence` table).
- **Low: `src/geofence.rs:82-88`.** A zero distance to the farthest chosen Spot gave a
  zero-radius horizon, which `Circle::new(..).ok()` silently dropped: no horizon, so the OS never
  asked to reselect. **Fixed in this branch** (bff0c04): at least `MINIMUM_SPOT_RADIUS`.
- **Low: `src/fence.rs:136-137`.** The cannot-happen enclosing-circle case returned
  `TooFewVertices`. **Fixed in this branch** (f87cb48): `GeoError::Internal` (style guide §5).
- **Low: `src/fence.rs:738-750`.** Property `containment_agrees_with_distance_outside` could not
  fail: `contains` is defined as `distance_outside == 0`. **Fixed in this branch** (f87cb48):
  deleted.
- **Low: `src/presence.rs:1020,1276`.** `restore_round_trips_the_snapshot` and the property
  `restoring_a_snapshot_keeps_state_and_snapshot` duplicate each other and test only that the
  snapshot is stored, not behaviour after restore. Replace one with the restore-timeout test
  above. **For the owner** (pingers-geo, with the restore decision).
- **Low: `src/presence.rs:1188-1210`.** The two-fixes-30-s-apart property measures from the
  first good fix ever, so after a Candidate timeout it passes trivially. Track the pair start in
  the model. **For the owner** (pingers-geo).

## pingers-gate

- **Low: `CONTRACT.md` ("Implementer decides") vs `src/lib.rs:182`.** The contract says adding
  a `SuppressReason` is breaking on purpose, but the enum is `#[non_exhaustive]`. Pick one.
  **For a human.**
- **Low: `src/lib.rs:224-262,356-368`.** Four single-reason tests catch nothing that
  `reasons_are_reported_in_priority_order` does not. **Fixed in this branch** (5d7dc72).

## pingers-ohttp

- **Medium: `src/message.rs:62,115`.** `BinaryRequest` and `BinaryResponse` derived `Debug`,
  printing paths (`/v1/groups/{id}/live`) and bodies. **Fixed in this branch** (aa43a22): method
  or status, header names and body length only, with a test.
- **Low: `src/error.rs`, every `map_err(|_| ..)`.** No variant keeps a `#[source]` (style §4).
  If deliberate for privacy, record it as a CONTRACT decision. **For the owner** (pingers-ohttp).
- **Low: `src/chunked.rs:69-75`.** `seal_error` maps every `ohttp::Error::Hpke(_)` to
  `TooManyChunks`; on the open side a sequence overflow becomes `OpenFailed`, not the
  `TooManyChunks` the CONTRACT promises. Match `HpkeError::MessageLimitReached`. **For the
  owner** (pingers-ohttp).
- **Low: `src/chunked.rs:250,363,438`.** `finish() -> Result<Option<OpenedChunk>>` is never
  `None`, and `request_decoder(&self)` takes from a `Cell`, so a second call returns a dead
  decoder. **For the owner** (pingers-ohttp; contract change).
- **Low: `src/chunked.rs:313-330`.** `accept` returns `Truncated` and the caller re-feeds the
  growing preamble, O(n²) for a head sent a byte at a time. **For the owner** (pingers-ohttp).
- **Low: `src/config.rs:418-437` with `pingers-crypto/src/mailbox.rs:94-104`.** Key ids are bare
  `u8` in both crates; the gateway secret is a raw `Zeroizing<[u8; 32]>` while crypto has
  `secret_key!` types; and OHTTP uses rust-hpke through the vendor crate while crypto uses
  hpke-rs, against "only pingers-crypto calls primitives". Add a shared `KeyId` and a
  `GatewayPrivateKey`, and record the two-HPKE exception. **For a human.**

## pingers-ohttp-relay

- **Medium: `src/relay.rs:150-151,201-204`.** No body size cap, no idle timeout between
  frames on streaming bodies, no connection or stream cap; `response_timeout` covers response
  headers only. Design B.12 asks to confirm the relay's limits. **For the owner** (decide the
  limits).
- **Medium: `src/relay.rs:87-89`.** All clients share one pooled h2c connection to the gateway;
  hyper-util's legacy client may not open a second when `MAX_CONCURRENT_STREAMS` is reached, so
  a few hundred idle chunked exchanges could starve everyone. Unverified; add a test with many
  open streams. **For the owner.**
- **Medium: `src/config.rs:85-92`.** Design B.12 says the gateway accepts relays by mutual TLS
  or an address allow-list; the relay speaks plaintext h2c only, so mTLS is impossible. Add TLS
  with a client certificate, or amend the design to "allow-list only". **For a human.**
- **Low: `src/relay.rs:190-192`.** `accept()` errors `continue` with no backoff, so EMFILE spins
  a core. **For the owner.**
- **Low: `src/relay.rs:150-151`, `src/tests.rs:105-110`.** Uses `tokio::time::timeout`, not the
  injected `Timer` (style §6, §10); the gateway-timeout path is untested. **For the owner.**
- **Low: `src/config.rs:52-82`.** `ConfigError` is hand-written, and `Syntax(String)`
  stringifies the toml error (style §4). **For the owner.**

## pingers-store

- **High: `src/repo/meetup.rs:142-165`, `src/lib.rs:39-40,53-56`.** `Meetups::end` left the
  group's `mls_kv` rows (epoch secrets, key pairs), relying on each caller to also call
  `delete_group`, against style §8 and design B.5, A.4; the crate doc claimed every key row was
  deleted. **Fixed in this branch** (806dfe3): `end` deletes the rows under
  `group_storage_prefix` in the same batch, with a test.
- **Medium: `src/lib.rs:83-90`.** The six mutant survivors were excused because `Polygon::new`
  and `RlnSecret` were stubs; both work now. **Fixed in this branch** (ece09db): polygon round
  trip, `RlnIdentities` save/load and rejected-secret tests; excuse removed.
- **Medium: `src/types.rs:237` vs `pingers-proto/src/api.rs:289`.** `RootEpoch` is `u64` here
  and `u32` in proto (which matches design D), and the store already depends on proto. Use
  proto's type. **For a human** (crosses into pingers-proto, which wave 2 is changing).
- **Low: `src/repo/meetup.rs:289,296-299`.** Reading coordinates as `Option<i32>` let rusqlite
  put an out-of-range stored value into `StoreError::Database` (style §9). **Fixed in this
  branch** (c73f6f6): read `i64`, narrow to `Corrupt { field }`, with a test.
- **Low: `src/repo/mls.rs:75-87`, `src/lib.rs:51-52`.** Busy and locked databases mapped to
  `StorageError::Failed`, though the MLS contract says `Unavailable` means retry; the crate doc
  said every database error became `Unavailable`. **Fixed in this branch** (911e37b).
- **Low: `src/database.rs:97`, `src/key.rs:165-174`.** rusqlite copies the hex `PRAGMA key`
  into a `CString` that is not zeroed. Avoiding it needs unsafe code; accept and comment.
  **For a human.**
- **Low: `docs/architecture.md:138`.** pingers-store's dependency list omits pingers-anon,
  -crypto, -geo and -time. **For a human** (doc edit).

## pingers-mls

- **High: `src/group.rs:301-327,405-443,529-535`.** `Group::group_id` cuts or pads the OpenMLS
  id to 32 bytes and neither join checked its length. A Party insider with one of our Meetup
  KeyPackages could send a Welcome with id `victim_id || 0x00` and no grant key; the failed
  grant check then deleted the victim Meetup's state. **Fixed in this branch** (3a73cfa): both
  joins refuse ids that are not `GroupId::LENGTH`, with a test that fails without the check.
- **Medium: `src/adapter.rs:427-435`.** KeyPackage bundles (private init and leaf keys) are
  global rows that only a consuming Welcome deletes, so an unanswered knock leaves them on disk
  forever. Add `delete_key_package`, an expiry sweep, or a knock-scoped prefix. **For a human.**
- **Medium: `CONTRACT.md`, mutation-testing Decision.** It says
  `delete_encryption_epoch_key_pairs` runs only from `MlsGroup::delete`, but OpenMLS calls it
  on every merge (`staged_commit.rs:799`). So nothing checks that epoch n-1's private keys are
  gone after a commit, which is forward secrecy for a stolen device (design A.4). Add that test
  and correct the Decision. **For the owner** (pingers-mls).
- **Medium: `src/adapter.rs:139-141,175-199`.** Epoch secrets, key pairs, KeyPackage bundles
  and PSKs are serialised into plain `Vec`s, contradicting `storage.rs:68` and style §8. Return
  `Zeroizing<Vec<u8>>`. **For the owner** (pingers-mls).
- **Low: `src/group.rs:1095-1103`.** Untrusted KeyPackages were validated with the suite they
  named, reaching P-256 code. **Fixed in this branch** (3a73cfa): suite checked first.
- **Low: `src/policy.rs:364-368`, `src/group.rs:1016`.** `GrantPublicKey::from_bytes` accepts
  any length and the roster check only tests that the extension exists. Add a checked
  constructor in pingers-crypto (32 or 1,216 bytes). **For the owner** (pingers-crypto, then
  pingers-mls).
- **Low: `src/group.rs:261,312,597`, `NotImplemented` at 70-73.** OpenMLS errors are dropped
  with `|_|` and the CONTRACT justifies that only for `StorageError`; `NotImplemented` is never
  returned. **For the owner** (pingers-mls).
- **Low: `src/tests/party.rs:204-208,243-247,254`.** The secrecy tests assert `WrongEpoch`,
  which the wrapper returns before decrypting, so cryptographic exclusion is untested; line 254
  is a bare `is_err()`. **For the owner** (pingers-mls).
- **Low: `CONTRACT.md:124`.** Eleven `delete_*` adapter survivors are called justified because
  nothing calls them; untested deletion code is better covered by a `StorageProvider`
  conformance test or removed. **For the owner** (pingers-mls).

## pingers-anon

- **Medium: `src/token.rs:137-161`.** `PrivacyPassToken` derived `Clone` and `PartialEq`, wrapped
  a plain `Vec`, and `authorization()` returned a plain `String`. **Fixed in this branch**
  (2996c0f): `Zeroizing`, no `Clone` or `==`, `Zeroizing<String>`, and no longer falsely
  fallible.
- **Medium: `src/identity.rs:23`, `src/verify.rs:62`.** `IdCommitment`, `Nullifier` and the other
  field elements derived `Debug`, printing bytes that pingers-proto's `RlnAuth` deliberately
  redacts; `NullifierRecord` printed the share `y`. **Fixed in this branch** (19c82f3).
- **Medium: `src/verify.rs:62-69`.** The server keeps `y` for every nullifier, though design B.4.8
  says the launch default `rln_slashing = off` ignores shares; a database or backup then holds
  half of what recovers `sk` after a duplicate. Drop `y` unless slashing is on. **For a human**
  (with pingers-quota).
- **Medium: `src/identity.rs:13-15`, `field.rs:12`, `pp.rs:15,35`.** Domain labels
  (`pingers/v1/rln`, `pingers/v1/rln/x`) live here and the crate calls `sha2` itself; its
  CONTRACT grants itself an exception the style guide (§8) does not allow. Its request digest
  length-prefixes method and path with `u64be` where crypto's `grp-request` uses `be32`. Move the
  preimages to crypto or amend §8, and pick one prefix width. **For a human.**
- **Low: `src/tree.rs:41`, `src/verify.rs:74`.** `LeafUpdate` and `NullifierOutcome` were not
  `#[non_exhaustive]`. **Fixed in this branch** (19c82f3).
- **Low: `tree.rs:109`, `prove.rs:46`, `identity.rs`, `issuer.rs:42`.** Error docs drift:
  `Parameters` and `NotImplemented` are never built; `MembershipTree::empty` and
  `RlnSecret::generate` cannot fail but return `Result`; a bad P-384 issuer scalar is reported as
  a BN254 `NotAFieldElement`. **For the owner** (pingers-anon).
- **Low: `verify.rs:68`, `issuer.rs:67,112`, `identity.rs:205`.** Raw `[u8; 32]`, `Vec<u8>` and
  `&str` in public signatures (style §2): add `TokenNonce`, `TokenKeyId`, `IssuerPublicKey`.
  **For the owner** (pingers-anon; pingers-store already has a `TokenKeyId`).
- **Low: `src/token.rs:16`, `src/pp.rs:33`.** The 300-token batch limit is defined twice. **For
  the owner** (pingers-anon).
- **Low: design.md B.4.8.** Still says `x = H(request body)` and an unprefixed
  `external_nullifier`; the code uses method, path and body with length prefixes. Update the
  design. **For a human.**
- **Low: tests.** No test swaps the proof's root for the other accepted root, and none proves
  after the device's own leaf is zeroed. **For the owner** (pingers-anon).

## pingers-testkit

- **Low: `src/harness.rs:12`.** `TODO (issue)` with no issue link (style §11). **For the owner.**
- **Low: crate-wide.** Five error enums (`ClockError`, `FixtureError`, `HarnessError`,
  `SecureStoreError`, `VectorError`) against one per crate; the RLN `World` fixture lives in
  `pingers-anon/tests/rln.rs` though store and quota will need it; anon's `tests/vectors.rs:22`
  reimplements `VectorFile::vector`. **For a human** (merge or justify the enums; move the
  fixture).

## pingers-cli

- **Medium: `src/scenario/runner.rs:83-89,227,299,331,355`.** `ScenarioDriver` and the runner
  return `anyhow::Result` below `main`, and `Outcome::Failed(String)` stringifies errors (style
  §4 allows anyhow at the top level only). Add a `CliError`. **For the owner** (pingers-cli;
  left alone here because the scenario runner is the wave 2 test harness).
- **Low: `src/load.rs:13`.** Its own `BUCKET_SECONDS = 900`. **Fixed in this branch** (f8d9f22):
  `Bucket::LENGTH`.

## xtask

- **High: `xtask/src/main.rs:428-478`.** `ci` runs neither `cargo mutants` nor the fuzz targets
  (`crates/pingers-proto/fuzz`, `crates/pingers-ohttp/fuzz`) that style §13 lists, and there is
  no other CI. It also passed without `cargo deny` when cargo-deny was missing. **Fixed in this
  branch** (9bf12f0) for deny, which is now required. **For a human:** add `cargo mutants
  --in-diff` and time-boxed fuzz runs, probably as separate xtask commands, since they need a
  base ref and a nightly toolchain.
- **Medium: `xtask/src/main.rs:25,263-275`.** `layers` ran `cargo tree` without
  `--all-features`, so anon's `prover`, `verifier` and `issuer` sides were never checked, and
  `sled` was not an I/O crate. **Fixed in this branch** (9bf12f0): all features, `sled` listed,
  with a written exception for pingers-anon.

## pingers-crypto (review only)

- **High: `src/invite.rs:29` (`seal_join_request`).** Output is `nonce || ct` with no padding.
  Mailbox posts must be exactly 1, 4, 8 or 32 KiB (`pingers-presence/src/server.rs:65`), so a
  sealed join request is either rejected or, padded outside the AEAD, leaks the key package and
  name size. Take a `SizeClass` and pad inside, as `seal_knock` does, with a vector. **For the
  owner** (pingers-crypto).
- **Low: `src/location.rs:401,496`.** HPKE base mode does not authenticate `context.sender` or
  `requester`, so any member can seal a grant claiming another leaf. Document that callers must
  check the MLS sender matches, and test it in pingers-meetup. **For the owners** (pingers-crypto
  docs, pingers-meetup test).
- **Low: `src/presence.rs:108`, `src/board.rs:29-45`.** `tag_matches` reruns the HMAC key
  schedule per record (4 compressions, not the 2 design B.4.3 budgets); `board.rs` duplicates
  `primitives::sha256`. **For the owner.**
- **Low: `src/negative_tests.rs`.** `relay_public_key_carries_its_key_id` checks a getter;
  `two_snapshots…differ` and `two_blobs…differ` only show the RNG was consumed, which the vector
  tests' `exhausted_exactly()` already covers. **For the owner.**

## pingers-proto (review only)

- **Medium: `src/api.rs:139,184`.** `revoke_key` and `bearer` travel as `Redacted<T>`, which is
  `Copy`, `PartialEq` and `Default` and never zeroizes, so the server can compare a bearer token
  with `==`. Add a wire-secret type and a constant-time `revoke_key_matches` in crypto. **For the
  owners** (pingers-proto, pingers-crypto).
- **Low: `src/board.rs:96-98`, `Lease::to_bytes`, `MeetupHint::for_group`.** `unwrap_or(zeros)`
  on conversions that cannot fail would silently emit zeros after a size change; destructure
  arrays instead. **For the owner.**

## pingers-push-relay (review only)

- **Medium: `src/service.rs:191`.** Only `now > not_after` is checked; `max_ttl` (30 days) is
  published but not enforced, so a far-future `not_after` replays until the key retires (design
  B.4.9). **For the owner.**
- **Medium: `src/keys.rs:64-66`, `src/config.rs:272-277`.** The public key is configured
  separately "because crypto cannot derive it", but `PushRelayPrivateKey::public_key` exists; a
  mismatched pair makes every push 422. **For the owner.**
- **Medium: `src/service.rs:51-55,122`.** The per-token key is SipHash with
  `RandomState::new()`: uninjected OS randomness (style §6), replacing design C.1a's
  `SHA-256(push_token)`. Add `pingers_crypto::push_token_fingerprint`. **For the owners**
  (pingers-crypto, pingers-push-relay).
- **Medium: `src/delivery.rs:561-578`.** The SSRF filter misses `::a.b.c.d`, NAT64
  `64:ff9b::/96`, 6to4 `2002::/16` with private IPv4 inside, IPv6 multicast, `0.0.0.0/8`,
  `192.0.0.0/24`, `198.18.0.0/15` and `224.0.0.0/3`. Better: check every resolved address at
  connect time. **For the owner.**
- **Medium: `src/request.rs:1-13`, `src/http.rs:61-98`, `src/delivery.rs:72-75,364`.** Three
  wire formats (`PushReq`, `RelayConfig` with extra fields, the device push payload) are defined
  here as JSON, against style §12; the payload's `v` is `1` for UnifiedPush and `"1"` for FCM.
  Move them to pingers-proto. **For the owners** (pingers-push-relay, pingers-proto).
- **Medium: `src/credentials.rs:18,64`.** Server credentials are stored raw (design C.1a says
  `credential_hash`), and `ct_eq` on slices of different lengths leaks the length. **For the
  owner.**
- **Low: `src/opener.rs:461-465`.** An `#[expect(wildcard_enum_match_arm)]` reason cites a
  `NotImplemented` variant that is gone. **For the owner.**
- **Low: `src/request.rs:313`, `src/delivery.rs:604`, `src/http.rs:79,83`.**
  `unwrap_or_default()` sends an empty body, key id 0 or an empty key instead of an error, and
  `http.rs` slices the encoded config by index. **For the owner.**
- **Low: `src/error.rs`, `src/request.rs:222`, `src/opener.rs:420`.** Four error types, one not
  thiserror, and all modules `pub`. **For the owner.**

## Cross-crate inconsistencies

- **Medium: pingers-push-relay `src/keys.rs:176` vs pingers-ohttp `src/config.rs:22`.** Two RFC
  9458 key-config encoders, both named `KeyConfig`; devices pin the SHA-256 of these bytes, so
  they must not drift. Reuse one. **For a human.**
- **Medium: leaf positions, roots and tokens modelled several times.** `LeafPosition` (anon),
  `MembershipIndex` (proto) and `TreePosition` (store); `MembershipRoot` is a field element in
  anon but `{epoch, root}` in proto; `PrivacyPassToken` (anon) vs `UnspentToken` (store); the key
  id is a raw `[u8; 32]` in anon and `TokenKeyId` in store. Give each concept one owner. **For a
  human.**
- **Medium: `RootEpoch`** is `u32` in pingers-proto and `u64` in pingers-store (above).
- **Low: pingers-crypto `src/location.rs:151` vs pingers-proto `src/meetup.rs:141-148`.** The
  80-byte sealed-location size lives in crypto while proto's `ciphertext` is a `Vec<u8>` of any
  length; crypto reimplements proto's `be16 || payload || zeros` padding twice. **For the
  owners.**
- **Low: 30-day lifetime** defined in push-relay `keys.rs:19`, `config.rs:119` and presence
  `server.rs:13`; `KEY_OVERLAP` is fixed while `max_ttl` is configurable. **For the owners.**
- **Low: lease lifetime** stated as `LEASE_BUCKETS = 4` (presence `lease.rs:13`) and
  `LEASE_DURATION = 1h` (presence `mailbox.rs:75`). Derive one from the other. **For the owner**
  (pingers-presence).
- **Low: pingers-proto `CONTRACT.md:65` vs design.md:939.** The CONTRACT drops APNs; design D,
  B.7 and C.1a still specify it. Add a dated note to the design. **For a human.**
- **Low: pingers-push-relay `src/http.rs:6` vs design D.** The relay answers 400, 401, 501, 502
  and 504, which design D does not list, so the server's action on each is undefined. **For a
  human.**

The other CONTRACT Decisions agree with design.md and with each other: the MLS PSK id and
exporter contexts match crypto's, and the presence tie-break, the meetup Lamport ties, the
900-second `Bucket` and the gate rules are consistent.
