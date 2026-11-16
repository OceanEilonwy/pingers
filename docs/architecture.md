# Pingers: build architecture

Status: proposal, 2026-11-13. Companion to `pingers-crypto-design.md` (revision 6 plus later decisions). Modelled on the monokulo-fob workspace: small crates with one job each, protocol logic free of I/O, one stateful core, pure presentation crates, and bindings that only convert types.

## 1. What has to be built

| # | Technical element | Where it runs |
|---|---|---|
| 1 | Wire formats: records, leases, knocks, mailboxes, group messages, meetup frames, padding size classes, API types | Shared |
| 2 | Geometry: circle and polygon fences, point-in-polygon, Spot presence state machine with hysteresis, exclusion zones, distance and bearing, area | Client |
| 3 | Cryptographic primitives with domain separation: HKDF labels, keyed tags, AEAD, HPKE seal/open, key schedule from the MLS exporter | Shared |
| 4 | MLS: Party groups and per-meetup groups, ciphersuite policy (classical default, X-Wing opt-in), roster checks, external PSK binding, grant-key leaf extension | Client (server only relays ciphertext) |
| 5 | Anonymous authorisation: RLN membership proofs (prover, verifier, tree, nullifiers) and Privacy Pass (client, issuer) as fallback | Shared |
| 6 | Oblivious HTTP: encapsulation for requests and chunked streams (both sides), key configs, key pinning | Shared |
| 7 | Co-presence protocol: blind-board leases with fixed slots and dummies, trial matching (client rule: only Spots you are inside), mailboxes, knocks, liveness checks, meetup admission | Client and server halves |
| 8 | Meetup protocol: live channel frames, chat threads, meet point, exact-location consent (location keys, grants, asks, rotation, dummy frames), compass targets | Client |
| 9 | Transport: pluggable router by privacy class, OHTTP client, chunked live channel, relay selection | Client |
| 10 | Encrypted local storage: SQLCipher schema and migrations, MLS storage provider, no location history | Client |
| 11 | Client state and orchestration (split by job, section 3): background pipeline (fixes, OS geofence events, wakes, pushes), Focus pause, settings, OS geofence selection (up to 97 on Android), events to the shell | Client |
| 12 | Presentation: per-screen view models, every user-facing string, the "What your server sees" text derived from the live configuration and fallbacks | Client |
| 13 | Native bindings: UniFFI for Kotlin | Client |
| 14 | Server: OHTTP gateway (relay-only), board store, mailboxes, MLS delivery log, sealed-push subscriptions, RLN tree, Privacy Pass issuer, live-channel fan-out | Server |
| 15 | Push relay: open sealed tokens, send APNs/FCM/UnifiedPush, rate limits, no logs | Separate service |
| 16 | Reference OHTTP relay: full-duplex streaming forwarder for third-party operators | Separate service |
| 17 | Android app: Compose UI, permissions, location and geofencing, FCM and UnifiedPush, foreground service, Keystore, interruption filter, MapLibre with offline region packs, compass sensor | Android |
| 19 | Test and dev tooling: headless client, in-process server harness, fake platform, test vectors, load generator | Dev |

## 2. Workspace layout

```
pingers/
  Cargo.toml                 workspace; resolver 3; edition 2024; android profile as in fob
  crates/
    pingers-time/            L0  Clock and Timer traits, UnixSeconds, Bucket (see style guide §6)
    pingers-proto/           L0  wire types and encodings
    pingers-geo/             L0  fences, presence state machine, bearing
    pingers-crypto/          L0  primitives and key schedule
    pingers-mls/             L1  openmls wrapper: Party and meetup groups
    pingers-anon/            L1  RLN proofs and Privacy Pass
    pingers-ohttp/           L1  OHTTP and chunked OHTTP encapsulation
    pingers-presence/        L1  board, leases, mailboxes, knocks (sans-IO)
    pingers-meetup/          L1  meetup session and location consent (sans-IO)
    pingers-gate/            L1  "may this device share right now?" (Focus, pause, zones)
    pingers-store/           L2  SQLCipher storage
    pingers-transport/       L2  router and OHTTP client
    pingers-map-packs/       L2  region map packs: PMTiles reading, place index, manifests, budgets
    pingers-parties/         L3  Party lifecycle: create, invite, join, roster, Spots
    pingers-sync/            L3  server sync: group logs, mailboxes, request queue
    pingers-wake/            L3  the background pipeline: one wake, start to finish
    pingers-meetup-session/  L3  a live meetup: channel, members, lifecycle
    pingers-map-downloads/   L3  region map packs: catalogs, downloads direct or via the relay, install
    pingers-client/          L3  composition root: wires the above, no domain logic
    pingers-screens/         L4  view models
    pingers-words/           L4  all user-facing text
    pingers-ffi/             L5  UniFFI bindings
    pingers-board-store/     L3  server: board records, expiry, paging, segment hashes
    pingers-mailbox/         L3  server: mailboxes and the wake rule
    pingers-delivery/        L3  server: MLS delivery log, subscriptions, live fan-out
    pingers-quota/           L3  server: RLN tree, nullifiers, Privacy Pass issuer, limits
    pingers-server/          L5  server binary: routes to the crates above, nothing else
    pingers-push-relay/      L5  push relay binary
    pingers-ohttp-relay/     L5  reference relay (library and binary)
    pingers-map-host/        L5  map host binary: serves whole region packs
    pingers-testkit/         dev fake platform, harness, vectors
    pingers-cli/             dev headless client and load generator
    pingers-tiles/           dev operator tool: cuts region packs from the Protomaps build
  android/                   Kotlin + Compose app
  vectors/                   JSON test vectors shared with other implementations
```

Layer rule: a crate depends only on crates in lower layers. Nothing below L2 does I/O or depends on tokio.

Licence (decided 2026-11-13): every crate, binary and the Android app is `AGPL-3.0-only` (`license = "AGPL-3.0-only"` in `[workspace.package]`, inherited by each crate; SPDX header in every source file). Dependencies must be AGPL-compatible; `cargo deny` enforces the allow-list.

## 3. The crates

### L0: foundations (pure, no I/O, no async)

**pingers-proto**: every type that crosses the network or the FFI boundary in serialised form.
- Board record (96 B), lease (4 slots), knock, mailbox registration with sealed push, group message envelope, meetup frame, API request and response structs.
- Canonical encoding (CBOR via `minicbor`, or a hand-rolled fixed layout for the fixed-size records), version tags, padding to size classes (256 B, 1 KiB, 4 KiB).
- No crypto, no logic beyond encode, decode and validate lengths.
- Deps: `minicbor`, `serde` (API JSON only), `thiserror`.

**pingers-geo**: geometry and the per-Spot presence state machine.
- WGS84 to local ENU projection; circle and polygon containment; polygon area and validity (no self-intersection, at least 3 points); distance and bearing.
- `SpotPresence` state machine (Outside, Candidate, Inside, Suppressed) with the entry rule (two fixes 30 s apart) and exit hysteresis (max(50 m, r/4)).
- Exclusion-zone checks including the 5-minute trail buffer.
- OS geofence selection: nearest N Spots plus a horizon region.
- Deps: none beyond `libm` if needed. Property tests for containment and hysteresis.

**pingers-crypto**: the only place primitives are called directly.
- Domain-separated labels as constants (`pingers/v1/...`); HKDF, HMAC tags truncated to 16 B, AES-GCM, HPKE base mode (X25519; X-Wing behind a feature).
- Key schedule: Party exporter to `K_spot`, `K_tag`, bucket tags; sealed push; location grants; beacon sealing.
- Secret types wrap `zeroize::Zeroizing`; no `Debug` on secrets.
- Deps: `hkdf`, `hmac`, `sha2`, `aes-gcm`, `hpke-rs` (or `hpke`), `rand_core`, `zeroize`, `subtle`.
- Ships the test vectors in `vectors/`.

### L1: protocols (sans-IO state machines, injected clock and randomness)

**pingers-mls**: a narrow wrapper over OpenMLS.
- Create, join and leave Party groups; create meetup groups with the external PSK; add and remove with the roster check (every added leaf key must be in the Party roster).
- Ciphersuite policy; `pingers_grant_key` leaf extension; exporter access for pingers-crypto.
- Storage through a trait that pingers-store implements, so this crate has no database.
- Deps: `openmls`, `openmls_rust_crypto` (or a libcrux provider for X-Wing), pingers-crypto, pingers-proto.

**pingers-anon**: anonymous authorisation, both sides.
- RLN: witness and proof generation (client), verification, membership tree updates and nullifier checks (server). Slashing off by default.
- Privacy Pass: token issuance (server) and redemption (client), type 0x0001 VOPRF.
- Deps: `zerokit`'s `rln` (pinned ceremony parameters), `voprf`, pingers-proto.
- Feature flags: `prover`, `verifier`, `issuer`, so each binary pulls only its side.

**pingers-ohttp**: encapsulation only, for both client and gateway.
- RFC 9458 requests and responses; chunked OHTTP (draft-08) for the live channel, with AES-GCM pinned and the zero-length-chunk check the upstream crate is missing.
- Key config parsing; key pinning and mismatch detection.
- Deps: `ohttp` (patched or vendored `stream` module), `bhttp`, pingers-crypto.

**pingers-presence**: the co-presence protocol.
- Client half: build leases (real slots plus random dummies), trial-match board pages (only Spots that are Inside), open beacons, build and verify knocks, mailbox lifecycle, liveness decision, meetup admission decisions.
- Server half (feature `server`): record validation, segment hashing, expiry rules, mailbox posting rules.
- All inputs and outputs are values; the caller does the network.
- Deps: pingers-crypto, pingers-proto, pingers-geo (for the Inside check input only).

**pingers-meetup**: what happens inside a meetup.
- Location consent: location keys and generations, grants, asks with expiry, rotation on revoke, scope and leave; fixed-cadence dummy frames.
- Chat threads (one level), meet point (last writer wins by Lamport clock, `AtMember` only from that member), keep-alives, silent-member removal timer.
- Compass computation from own fix and heading plus the target's latest decrypted position.
- Deps: pingers-mls, pingers-crypto, pingers-proto, pingers-geo.

**pingers-gate**: the single answer to "may this device share its presence right now?".
- Inputs: Focus/DND state, manual pause, quiet hours, exclusion zones with the 5-minute trail buffer, location sharing on or off.
- Output: `Share` or `Suppress(reason)`. Suppress means only dummy uploads, no trial matching, decline every knock (B.10).
- Pure and tiny; every path through it is a test case. Keeping it separate means no other crate can grow its own pause logic.
- Deps: pingers-geo.

### L2: I/O adapters

**pingers-store**: encrypted local storage.
- SQLCipher through `rusqlite` with `bundled-sqlcipher`; migrations; typed repositories per table; implements the pingers-mls storage trait.
- Cross-process lock for the app and its extensions; crash-safe transactions.
- No location history table, by construction.
- Deps: `rusqlite`, pingers-mls, pingers-proto, `zeroize`.

**pingers-transport**: getting bytes to the server.
- `Transport` trait and the router by privacy class (Anonymous, Identified, Stream); anonymous and stream traffic never goes direct.
- OHTTP client over HTTP/2 (`hyper` + `rustls` with X25519MLKEM768); chunked live channel with reconnect; split-channel fallback; relay selection and health.
- `PublicClient`: plain HTTPS `GET`s to a public static host the user chose to reach directly (the map host with "Download maps through privacy relay" off); never the server.
- Tor and mixnets would be new `Transport` implementations here, nowhere else.
- Deps: pingers-ohttp, `hyper`, `hyper-util`, `rustls`, `tokio`.

**pingers-map-packs**: region map packs (design B.12, "Map host"), shared by the map host and the operator tool.
- Region lists with hierarchical ids (`continent/country/state`), so large countries split into sub-regions; a host's allow-list and per-region zoom caps.
- A PMTiles v3 and vector-tile reader, enough to build the place-name search index from the pack's own labels; the manifest and catalog formats the app reads.
- `pmtiles extract` (and `--dry-run` size estimates) as a child process; the disk budget check.
- Sync file I/O only; no network of its own (the `pmtiles` tool fetches from the build). Content checksums come from pingers-crypto. Deps: pingers-crypto, `flate2`, `serde_json`, `toml`.

### L3: stateful logic

There is no "core" crate on either side. Each stateful job has its own crate with a name that says what belongs in it. A crate whose name stops describing its contents gets split.

Client side:

**pingers-parties**: everything about Parties as long-lived things.
- Create a Party; invites (mailbox plus URL-fragment secret) and their status; join with safety-code verification; roster changes and removal; leave.
- Spots: create, edit (circle or polygon), delete; the per-Party settings that the design allows (server, relays, post-quantum).
- Deps: pingers-mls, pingers-store, pingers-proto, pingers-geo.

**pingers-sync**: talking to the server, reliably, for everyone else.
- Fetch and apply group log messages; post commits (one per epoch, retry on conflict); mailbox registration and reads; subscriptions with sealed push.
- Queue for anonymous requests that could not be sent in this wake; drop rules (a missed lease looks like a dummy); token supply (pre-fetched Privacy Pass, RLN proofs made only in the main app).
- Knows nothing about what the messages mean; it hands them to their owner.
- Deps: pingers-transport, pingers-store, pingers-anon, pingers-proto.

**pingers-wake**: one background wake, start to finish.
- Entry points: a fix, an OS geofence transition, a scheduled wake, a push, a Focus change.
- Fixed sequence: ask pingers-gate; update Spot presence (pingers-geo); build and upload the lease, sync the board and trial-match (pingers-presence); knock or answer a knock (liveness check); hand a successful match to pingers-meetup-session; choose the next OS geofences and the next wake.
- Owns the wake budget (the seconds a background wake allows) and stops cleanly when it runs out.
- Deps: pingers-gate, pingers-geo, pingers-presence, pingers-sync, pingers-parties (read-only view of Spots).

**pingers-meetup-session**: a meetup while it is live.
- Creates or joins the meetup group; holds the chunked OHTTP live channel; sends location or dummy frames on the cadence; applies consent decisions from pingers-meetup; removes silent members; ends the meetup and deletes its keys and content.
- Deps: pingers-meetup, pingers-mls, pingers-transport, pingers-store, pingers-presence (only for `MeetupKey`: `PSK_meet` with its id, so the Spot keys never leave pingers-presence).

**pingers-map-downloads**: region map packs for the app (design B.12, "Map host"), so the shell makes no network calls.
- Fetches `catalog.json` from each host in the chain (user override, the server's map host, the publisher's default) and offers each region from the first host listing it.
- Downloads a region's index and pack in byte-range pieces, resuming a partial file named after its checksum; checks each against the manifest (pingers-crypto's content checksum) and writes the manifest last; lists and removes installed regions.
- Two routes: direct HTTPS (pingers-transport's `PublicClient`), or through the relay: each request sealed to the map host's own OHTTP gateway (pingers-ohttp, padded), sent as an anonymous request to the server, which forwards it unopened; the gateway key is pinned on first use through a store the caller provides; request counts are padded to size classes.
- Deps: pingers-transport, pingers-map-packs, pingers-ohttp, pingers-crypto, pingers-proto, pingers-time, `tokio` (files).

**pingers-client**: the composition root, the only crate pingers-ffi and pingers-screens see.
- Builds the crates above from a `Platform` (secure store, OS geofences, fixes, DND, wake scheduling) and an `Events` sink, and exposes one async API.
- **Rule: no domain logic.** Each method delegates to one crate. If a change here needs a domain `if`, it belongs in a named crate instead. Enforced by review and a size budget (target under 500 lines).
- Publishes a read-only state snapshot for pingers-screens.

Server side (each crate owns its storage trait, with a SQLite implementation behind a `sqlite` feature, so self-hosting stays one file):

**pingers-board-store**: board records in memory; append, expire after the lease horizon, paging with MACed cursors, segment hashes for equivocation checks. Deps: pingers-presence (`server`), pingers-proto.

**pingers-mailbox**: anonymous mailboxes, posting rules, expiry, and the wake rule (forward the sealed push to the relay, never open it). Deps: pingers-proto.

**pingers-delivery**: the MLS delivery service: group logs with one commit per epoch, sealed-push subscriptions, Welcome routing via mailboxes, live-channel fan-out registry, group expiry. Deps: pingers-proto.

**pingers-quota**: who may post: accounts and their bearers (registration, sign-up limit), RLN membership tree and roots, nullifier set, Privacy Pass issuance and the spent set, per-bucket limits. Deps: pingers-anon (`verifier`, `issuer`), pingers-crypto.

### L4: presentation (pure)

**pingers-screens**: what each screen shows, computed from core state and the clock, as in fob-screens. Home, Party, Spot editor (including the point-by-point polygon editing model), Exclusion zones, Settings, Meetup, Chat, Meet point, Compass, Location request, Invite, Advanced, What your server sees.

**pingers-words**: every user-facing string, from typed scenarios the other crates describe, as in fob-words. This includes generating the "What your server sees" lists from the actual configuration: which relays, which fallbacks are active, and whether operators coincide. Localisation lives here.

### L5: bindings and binaries

**pingers-ffi**: UniFFI only. It converts types, maps errors, turns async calls into Kotlin `suspend`, and exposes `Platform` and `Events` as callback interfaces. No logic. Feature `wipe-freed` with a zeroing allocator as in fob-ffi.

**pingers-server**: the self-hosted server binary. It routes requests to pingers-board-store, pingers-mailbox, pingers-delivery and pingers-quota and holds no logic of its own (same rule as pingers-client). `axum` over `hyper`, HTTP/2; the gateway accepts `POST /ohttp` only from configured relays (mutual TLS or allow-list); identified endpoints direct; config file; metrics without identifiers. It forwards sealed map requests (`/v1/maps/ohttp`) to its map host's gateway and nowhere else.

**pingers-push-relay**: the publisher-run push relay. Stateless: HPKE open, check the mailbox binding, send to APNs (`a2`), FCM HTTP v1, or a UnifiedPush endpoint; per-token and per-server rate limits; key config endpoint; logging off by default.

**pingers-ohttp-relay**: a small reference relay (forward only, full-duplex streaming, no logs) so relay operators have something to run. Optional if an existing relay is used. Its library (`serve`) is what the binary runs and what testkit's harness and pingers-transport's integration tests run, so tests exercise the relay operators deploy.

**pingers-map-host**: the map host, a separate public static service like the push relay (design B.12, "Map host"). Publisher-hosted by default; operators can run their own and Pingers servers can advertise one (`map_host` in `/v1/params`). Serves region manifests, PMTiles packs and search indexes from a directory: HTTP/2, `Range`, strong ETags from the SHA-256, immutable caching, no cookies, no client addresses, logging off by default (counts and disk use when on). Operators choose what they host with an allow-list of regions and a `max_disk` budget; an optional cache mode cuts allow-listed regions from the upstream Protomaps build on first request and evicts least-recently-served packs. Its own OHTTP gateway (`POST /ohttp`, `GET /ohttp-keys`) answers map requests that phones seal to it and the Pingers server forwards unopened, padded to size classes, so relayed downloads hide the region from the server and the phone's address from the host. Deps: pingers-map-packs, pingers-ohttp, pingers-crypto, `axum`, `tokio`, `tokio-util`.

### Dev

**pingers-testkit**: a fake `Platform` (scripted fixes, DND, wakes), an in-process server with relays, deterministic clock and RNG, and the vector loader. Used by every crate's integration tests.

**pingers-cli**: a headless client over pingers-client for manual testing, end-to-end scenarios (G.3 to G.7 in the design), and load generation for the board-size and server-cost experiments.

**pingers-tiles**: the operator tool for map packs: plans `pmtiles extract` commands with per-region size estimates against a host's budget, cuts packs, and writes each region's search index and manifest (`crates/pingers-tiles/README.md`).

## 4. Native apps

**Android (`android/`)**: Kotlin, Jetpack Compose, one Gradle module for the app plus the generated UniFFI bindings (the `.so` built per ABI with `cargo-ndk` and the `android` profile).

The shell implements `Platform` and draws what pingers-screens publishes. It owns:
- permissions and onboarding
- Fused Location and the Geofencing API
- FCM, or a UnifiedPush distributor
- a foreground service of type `location` for meetups and the optional "Keep Pingers running"
- Keystore wrapping of the SQLCipher key
- the `NotificationManager` interruption filter
- MapLibre Native rendering of the whole-region packs the core downloads (pingers-map-downloads): the region picker over the core's catalog, progress, and on-device place search over each installed pack's index (`android/docs/maps.md`)
- the rotation-vector sensor for the compass, and the downgraded-priority counter for FCM

## 5. Rules that keep the core clean

- **Sans-IO below L2.** Protocol crates take bytes, time and randomness as inputs and return bytes and decisions. That makes them deterministic and fuzzable.
- **One responsibility per crate.** If a crate needs a second paragraph to describe it, split it. If two crates always change together, merge them.
- **No catch-all crates.** No crate is named `core`, `common`, `utils` or `shared`. A helper used by two crates lives in the lower-layer crate that owns its concept. The two composition roots (pingers-client, pingers-server) only wire and delegate.
- **No platform code in Rust.** Location, push, sensors and secure storage stay in the shells behind `Platform`.
- **Secrets are typed.** `Zeroizing` wrappers, no `Debug` or `Display` on key material, explicit lifetimes for meetup keys (deleted when the meetup ends).
- **Errors are per crate** (`thiserror`). Only pingers-ffi flattens them for the apps.
- **Testing:** test vectors for every derived value and wire format; property tests for geometry and state machines; fuzzing for every decoder and for chunked OHTTP; end-to-end scenarios through testkit.
- **Feature flags only for real splits** (client versus server halves, X-Wing), never to switch behaviour silently.

## 6. Build order

1. **Spikes** (release blockers 1 to 3 and 11 in H.1): Apple location-push entitlement request; a location push extension under 24 MB; a chunked OHTTP relay round trip; RLN proving time on phones.
2. **Foundations:** pingers-proto, pingers-geo, pingers-crypto with vectors.
3. **Protocols:** pingers-mls, pingers-ohttp, pingers-anon, then pingers-presence and pingers-meetup, each with sans-IO tests.
4. **Server side:** pingers-board-store, pingers-mailbox, pingers-delivery, pingers-quota, pingers-server, pingers-push-relay, pingers-ohttp-relay; pingers-testkit and pingers-cli running the full G.3 to G.7 scenarios headless.
5. **Client:** pingers-gate, pingers-store, pingers-transport, pingers-parties, pingers-sync, pingers-wake, pingers-meetup-session, pingers-client, pingers-screens, pingers-words.
6. **Android:** pingers-ffi plus the Compose shell, screen by screen against the canvas.
