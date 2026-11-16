# Deploying Pingers on monokulo-nodo

This is the runbook for running the four Pingers services on a monokulo-nodo appliance: what the
packages contain, every key and secret, how the sites are exposed, how the signed package feed
is built and published, how to test locally, and the bring-up steps.

**Host names.** Every host name below is a placeholder under `example.org`. The real ones are
deployment values: they live in the appliance's nodo settings and in the repository's CI
variables (`PINGERS_SERVER_HOST`, `PINGERS_OHTTP_RELAY`, `PINGERS_PUSH_RELAY`,
`PINGERS_MAP_HOST`), never in Pingers source.

| Placeholder | Service |
|---|---|
| `pingers.example.org` | pingers-server (API; the OHTTP gateway sits behind the relay) |
| `ohttp.example.org` | pingers-ohttp-relay |
| `push.example.org` | pingers-push-relay |
| `maps.example.org` | pingers-map-host |

`cargo test -p xtask --test nodo_packaging` checks the packaging against nodo's own parsers
(manifests, settings, rendering) and parses the rendered server, push relay and map host configs,
and their dev configs, with those services' own parsers.

## 1. The four apps

nodo runs one service per package, so there are four nodo apps. One xbps recipe,
`packaging/nodo/srcpkgs/pingers-server/template`, builds all four from one `cargo build`: the
main package `pingers-server` and the subpackages `pingers-push-relay`, `pingers-ohttp-relay`
and `pingers-map-host` (symlinks beside it, as in void-packages).

Each app installs under `/opt/nodo/apps/<id>/`:

```
bin/<id>              launcher: exec ../libexec/<id> /etc/apps/<id>/<name>.toml
libexec/<id>          the Rust binary
manifest.toml         what nodo generates the sandbox, firewall and Caddy site from
settings.lua          the settings nodo-tui shows
<name>.toml.in        the config template nodo renders from the settings
LICENSE               AGPL-3.0
```

nodo's exec arguments may not name `/etc/apps/<id>/`, where the rendered config lives, so each
app has a launcher. It removes the `NODO_*` variables nodo also passes (they include the sealed
secrets) and keeps the binary's name, so nodo's generated `pkill -x <id>` stops whichever is
running.

All listeners are on loopback:

| Port | Listener | Reached by |
|---|---|---|
| 9470 | pingers-server API (h2c) | Caddy, for `pingers.example.org` |
| 9471 | pingers-server OHTTP gateway (h2c; only `POST /ohttp`, `GET /v1/ohttp/config`) | pingers-ohttp-relay only |
| 9472 | pingers-push-relay (HTTP/1.1) | Caddy, for `push.example.org`; pingers-server |
| 9473 | pingers-ohttp-relay (h2c) | Caddy, for `ohttp.example.org` |
| 9474 | pingers-map-host (HTTP/1.1; `POST /ohttp` is its own OHTTP gateway) | Caddy, for `maps.example.org`; pingers-server (map downloads through the relay) |

```
phone ──TLS 1.3, X25519MLKEM768──▶ [router :443] ──▶ nodo Caddy (ACME certificates)
            pingers.…  → 127.0.0.1:9470  h2c   pingers-server API
            ohttp.…    → 127.0.0.1:9473  h2c   pingers-ohttp-relay ──h2c──▶ 127.0.0.1:9471 gateway
            push.…     → 127.0.0.1:9472  h1    pingers-push-relay (GET /v1/config; /v1/push hidden)
            maps.…     → 127.0.0.1:9474  h1    pingers-map-host
pingers-server ──loopback──▶ 127.0.0.1:9472 /v1/push
pingers-server ──loopback──▶ 127.0.0.1:9474 /ohttp, /ohttp-keys   (map downloads through the relay, still sealed)
pingers-push-relay ──TLS──▶ FCM, Google OAuth, UnifiedPush servers
```

Firewall, from the manifests and the references between settings: pingers-server has no
outbound access at all and reaches only 9472, by referencing the push relay's `server-url`, and
9474, by referencing the map host's `gateway-url`.
pingers-ohttp-relay reaches only 9471, by referencing the server's `gateway-url`. Caddy reaches
only the web upstreams, so it cannot reach 9471. Only pingers-push-relay has
`outbound = "internet"`. nodo refuses dependency cycles, and the references run one way:
`pingers-ohttp-relay → pingers-server → pingers-push-relay` and `pingers-server →
pingers-map-host`, so the map host is installed before the server.

### 1.1 Settings and secrets

"Sealed" means a nodo secret setting: encrypted under the device key, rendered only into memory
or the app's encrypted volume after the owner unlocks the device, and included in backups.

**pingers-server** (`server.toml.in`; encrypted 1 GiB volume for the databases, so the rendered
config with the gateway keys and the registration code stays inside the volume)

| Setting | Type | Default | Fills |
|---|---|---|---|
| `server_id` | url, required | | `server_id`, such as `https://pingers.example.org`. The server's identity: changing it makes a new server |
| `operator` | string, required | `$(pingers-push-relay.operator)` | `server_operator` |
| `ohttp_relay_url` | url, required | | `ohttp_relay_urls`, such as `https://ohttp.example.org/ohttp` |
| `map_host` | url, required | | `map_host`, such as `https://maps.example.org`. Map downloads through the relay are forwarded to this device's map host on loopback (`map_host_gateway`, from pingers-map-host's `gateway-url`) |
| `logging` | enum `off`/`counts` | `off` | `logging` |
| `registration_code` | **secret**, generated (16 bytes, hex); 16 to 128 of `A-Z a-z 0-9 - _` if set by hand | | `registration_codes`: only a device presenting it can make an account (design B.1, section 1.3) |
| `ohttp_key_a`, `ohttp_key_b` | **secret**, generated (32 bytes, hex) | | `[[ohttp.keys]] secret` |
| `ohttp_key_a_id`, `ohttp_key_b_id` | int 1 to 255 | 1, 2 | `[[ohttp.keys]] key_id` |
| `ohttp_served_key_id` | int 1 to 255 | 1 | `[ohttp] served_key_id` |
| `push_relay_public_url` | url, required | | `[[push_relays]] public_url`, such as `https://push.example.org` |
| `push_relay_key_config_hash` | string, a pin (43 base64url characters), required | | `[[push_relays]] key_config_hash` |
| `gateway_url`, `gateway_key_config_url` | url, public endpoints | `http://127.0.0.1:9471/...` | read by pingers-ohttp-relay |

It also reads `pingers-push-relay.server_credential` (a sealed secret of the push relay) once the
owner approves the grant, and the push relay's public `server-url` and `operator`.

**pingers-push-relay** (`push-relay.toml.in`; no volume, sealed secrets keep the rendered config
in memory)

| Setting | Type | Default | Fills |
|---|---|---|---|
| `operator` | string, required, public | | `operator` |
| `logging` | enum | `off` | `logging` |
| `hash_key` | **secret**, generated | | `hash_key` |
| `key_a`, `key_b` | **secret**, generated | | `[[keys]] private_key` |
| `key_a_id`, `key_b_id` | int 1 to 255 | 1, 2 | `[[keys]] key_id` |
| `key_a_activated_at`, `key_b_activated_at` | int, Unix seconds | 0, 4102444800 | `[[keys]] activated_at` (B is held in reserve) |
| `key_a_retired_at`, `key_b_retired_at` | int, 0 = not scheduled | 0 | `[[keys]] retired_at` |
| `server_credential` | **secret**, generated | | `[[servers]] credential`; also read by pingers-server |
| `fcm_project_id` | string, required | | `[fcm] project_id` |
| `fcm_service_account` | **secret**, required, pasted by the owner | | `[fcm] service_account_json` |
| `server_url` | url, public endpoint | `http://127.0.0.1:9472` | read by pingers-server |

**pingers-ohttp-relay** (`relay.toml.in`; no storage, no secrets): `response_timeout_seconds`
(int 5 to 300, default 30). The gateway addresses come from pingers-server.

**pingers-map-host** (`map-host.toml.in`; unencrypted data on the data drive): `max_disk`
(default `40GB`), `hosted` (default `oceania/australia`, space-separated),
`logging`, and `gateway_url` (default `http://127.0.0.1:9474`, published to pingers-server as
`gateway-url`: where it forwards map downloads made through the relay; leave it).

### 1.3 Registration code

Without a registration code a Pingers server lets anyone who finds it make an account. The
package generates one on install (`registration_code`, sealed), so it never runs open. How it
reaches phones:

- **The first phone** (the owner's) presents it at its first start. Until the app has a screen
  for it (it needs a design first), that is a debug build made with
  `PINGERS_DEV_REGISTRATION_CODE=<code>` (or `-PpingersDevRegistrationCode=<code>`); release
  builds ignore the variable. Read the code over the owner's SSH from the rendered config,
  `registration_codes` in `/run/nodo/volumes/pingers-server/config/server.toml`.
- **Everyone else** gets it in the invite link: a phone that registered with a code puts it in
  every invite it makes, and the friend's phone presents it when it opens the link. So the code
  travels as far as invite links do; it is a bearer capability like the link itself.
- **A refused code** (wrong, or reset since) is not tried again until another one is offered
  (a new invite link, or another build), so a phone never retries a 403 in a loop.

To stop new accounts from old invite links, Reset `registration_code` in nodo-tui, and give the
new code to phones that should keep inviting (today: a new debug build). Phones that already
have an account are not affected: accounts live in `quota_db`
(`/var/lib/apps/pingers-server/quota.sqlite`, on the encrypted volume), and a registered phone
proves its key without any code. A restart keeps every account and every bearer: a bearer is
still accepted until the hour it was issued for ends, a deleted account stays deleted, and the
database holds only each account's public key, when it registered, and a digest of each bearer
(never a usable bearer). The code only gates new accounts, which the server also limits to
`signups_per_hour` (default 60) whatever the code.

### 1.2 Keys and rotation

No key is made off the device: every 32-byte private key is a generated secret (any 32 random
bytes are a valid X25519 key). The FCM service account is the only secret a person handles, and
only the owner, pasting it into nodo-tui on the device. No tool reads or copies it.

Both HPKE key sets (the OHTTP gateway's and the push relay's) have two slots, and app builds pin
the hashes of both, so the app never meets an unpinned key:

1. At install, slots A and B are generated. A is served (`ohttp_served_key_id = 1`; the push
   relay's B activates in 2100). Put both pins in CI: `PINGERS_GATEWAY_KEY_PINS` and
   `PINGERS_PUSH_RELAY_KEY_PINS` (section 6, step 9). A pin is the unpadded base64url SHA-256 of
   a key config, 43 characters; the push relay also publishes its next key at
   `GET /v1/config/next`.
2. Yearly, serve B: set `ohttp_served_key_id = 2`; for the push relay set `key_b_activated_at` to
   now and `key_a_retired_at` to at least 30 days later. Every configured key keeps opening
   requests meanwhile. Update `push_relay_key_config_hash` in pingers-server.
3. Once no supported app build pins A only, `Reset` slot A (nodo generates a new one) and ship
   the next app release with the pins of B and the new A.

On a suspected compromise, do steps 2 and 3 at once.

### 1.4 Invite links and App Links

An invite link is `https://<server_id>/j#v1.<invite>`. The invite is in the fragment, which
browsers never send, so the server only ever sees `GET /j`. It answers with a static landing
page (strict CSP, its own script and stylesheet only, cacheable) whose script reads the fragment
on the phone and offers "Open in Pingers": `pingers://join#<invite>`, or on Android an intent
URL naming the app, so no other app that claims `pingers://` receives the invite. It tries the
apps of `[app_links]` in order (without it, the official Play then F-Droid builds), each
fallback being the page again for the next app, and the last `docs/install.md`. The server
keeps no record of the request, as of any other.

With `[app_links]` the server also serves `/.well-known/assetlinks.json`, so Android opens
`https://<server_id>/j` links straight in an official build (Play `org.pingers.app`, F-Droid
`org.pingers.app.foss`) without the page. The setting `app_signing_certificate` is the signing
certificate's SHA-256 fingerprint; its default is the official builds'. App Links verify only
on port 443. Links of other servers open through the landing page (on Android 10 and 11, also
through the "Open with" chooser).

## 2. Exposure, and the router's reverse proxy

For each of the four sites, in nodo-tui (Web): connect the domain and DNS provider (Porkbun,
DNS-01, so port 80 need not be forwarded), set the full public host, turn **public** on, and on
the Confirm screen choose the rate limit (below). No `.local` or onion exposure.

The router accepts 443 and runs its own reverse proxy in front of the appliance. Whether it
terminates TLS or passes it through is not known, and the package works either way; three
things change, so check which it is.

### 2.1 Checking whether the router terminates TLS

From **outside** the LAN (a phone hotspot), and then from **inside** the LAN straight at the
appliance's LAN address, fetch the certificate for the same name and compare:

```sh
# Outside: through the router.
openssl s_client -connect pingers.example.org:443 -servername pingers.example.org \
  -alpn h2 </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256 -issuer -dates
# Inside: the appliance itself (192.0.2.10 stands for its LAN address).
openssl s_client -connect 192.0.2.10:443 -servername pingers.example.org \
  -alpn h2 </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256 -issuer -dates
```

- **Same fingerprint**: the router passes TLS through (TCP or SNI forwarding). The phone talks
  TLS to Caddy, and everything in this document holds as written.
- **Different fingerprint** (or the router's own issuer): the router **terminates** TLS and
  opens a second connection to the appliance.

Also from outside, with OpenSSL 3.5 or later, check the post-quantum group and HTTP/2:

```sh
openssl s_client -connect ohttp.example.org:443 -servername ohttp.example.org \
  -groups X25519MLKEM768 -alpn h2 </dev/null 2>&1 | grep -E 'Negotiated TLS1.3 group|ALPN|Verification'
```

It must connect and show `X25519MLKEM768` and `h2`. Do this for all four names.

### 2.2 If the router passes TLS through

Nothing to change. Caddy offers X25519MLKEM768 by default, streams h2c both ways for the server
and the relay (`flush_interval: -1`, nodo N1), and sees each client's address for its rate
limit. Turn Caddy's per-address rate limit **off for `ohttp.example.org`** (phones behind
carrier NAT share an address, and the server's anonymous limits are the real control); keep it
on for the other three.

### 2.3 If the router terminates TLS

The package still works, but the router now decides three things:

1. **Post-quantum TLS** on the outer hop is the router's. If the check above does not show
   X25519MLKEM768, the phone's connection is classical; switch the router to TLS passthrough
   (SNI forwarding to the appliance) for these four names. Passthrough is the recommended setup.
2. **Streaming.** Chunked OHTTP (a Meetup's live channel) needs every chunk forwarded at once in
   both directions, for as long as a Meetup lasts. The router must not buffer request or response
   bodies for `ohttp.example.org` and `pingers.example.org`, must proxy HTTP/2 (or at least
   chunked HTTP/1.1) to the appliance, and its idle timeout must exceed 60 seconds (the server
   sends a keep-alive chunk every 20 seconds). Check with two phones: start a Meetup and watch
   the other phone's position move within a second or two, for ten minutes.
3. **Client addresses.** Caddy sees only the router's address. Pingers' relay-only rule does not
   depend on it: the gateway is a separate loopback listener that only the relay's firewall
   identity can reach, and no `trusted_proxies` is configured. But Caddy's per-address rate limit
   would then put every visitor in one budget, so turn it **off for all four sites**.

The router must connect to the appliance over **HTTPS on 443 with the site's name as SNI**
(Caddy redirects plain HTTP), and should verify Caddy's certificate. A terminating router also
sees every request in clear apart from what OHTTP and MLS encrypt; here it is the same operator.

## 3. The package feed

The signed feed is the assets of one fixed GitHub release, `feed`, in `OceanEilonwy/pingers`:

```
https://github.com/OceanEilonwy/pingers/releases/download/feed
  aarch64-repodata, x86_64-repodata        the signed xbps indexes
  <pkg>-<version>.<arch>.xbps (.sig2)      the four packages, both architectures, and signatures
  pingers-feed.pub.pem                     the public key
  pingers-<commit>.tar.gz                  the exact source the packages were built from
```

xbps reads a repository as `<address>/<arch>-repodata` and `<address>/<file>`, and nodo-install
follows GitHub's redirect to its storage host (`nodo-install/src/http.rs`), so the release's
download URL is a flat repository. Releases rather than GitHub Pages because of size: release
assets may be up to 2 GiB each with no total limit, while Pages allows 100 MB per file and 1 GB
per site. Eight packages of tens of megabytes would fit in Pages today, but the vendored source
tarball (about 115 MB, which AGPL-3.0 makes worth publishing beside the binaries) would not, and
Pages would also hold every old version until the site is rebuilt. Releases also keep the feed
out of the repository's history and leave Pages free for anything else.

**Key.** RSA 4096, signer name `pingers feed`, fingerprint (as xbps and nodo show it):

```
9d:c4:00:6e:53:d8:99:c4:f9:5d:9c:96:b0:e9:1a:07
```

The public key is `packaging/nodo/feed-signing-key.pub.pem`. The private key is kept offline by
the owner and as the `PINGERS_FEED_SIGNING_KEY` secret of the `feed` environment.

**Workflow.** `.github/workflows/feed.yml` runs by hand (Actions, feed, Run workflow on `main`)
or on a pushed `feed-*` tag. `dist` runs `cargo xtask nodo-dist`; `build` builds both
architectures in monokulo-nodo's xbps-src builder at the commit `NODO_REV` (x86_64 natively,
aarch64 cross-compiled); `publish`, in the `feed` environment and the only job with
`contents: write`, signs with nodo's `publish-feed.sh`, runs nodo's own install review on every
package against the pinned fingerprint, uploads the new packages, then the indexes, then deletes
assets the new indexes no longer name.

**One-time setup** (the owner or the coordinator):

```sh
# The environment that holds the key, deployable from main and feed-* tags only.
gh api -X PUT repos/OceanEilonwy/pingers/environments/feed --input - <<'EOF'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
EOF
gh api -X POST repos/OceanEilonwy/pingers/environments/feed/deployment-branch-policies \
  -f name=main -f type=branch
gh api -X POST repos/OceanEilonwy/pingers/environments/feed/deployment-branch-policies \
  -f name='feed-*' -f type=tag
# The key, straight from the file.
gh secret set PINGERS_FEED_SIGNING_KEY --repo OceanEilonwy/pingers --env feed \
  < ~/.config/pingers/keys/feed/pingers-feed.pem
# The release whose assets are the feed.
gh release create feed --repo OceanEilonwy/pingers --target main --latest=false \
  --title "Pingers feed for monokulo-nodo" \
  --notes "Signed xbps feed. Address: https://github.com/OceanEilonwy/pingers/releases/download/feed  Key fingerprint: 9d:c4:00:6e:53:d8:99:c4:f9:5d:9c:96:b0:e9:1a:07"
```

`NODO_REV` in `feed.yml` names the monokulo-nodo commit the feed is built with; it must have N1
and N2, which monokulo-nodo merged in #12 (4ad5661, also the commit `cargo test -p xtask` parses
the packaging with).

**Building locally** (Docker, a monokulo-nodo checkout at 4ad5661 or later, which has N1 and N2, at `$NODO`):

```sh
cargo xtask nodo-dist HEAD --nodo "$NODO"     # prints version, sha256; fills target/nodo/srcpkgs
NODO_EXTRA_PACKAGES="$PWD/target/nodo/srcpkgs" "$NODO/scripts/build-packages.sh" x86_64 pingers-server
NODO_EXTRA_PACKAGES="$PWD/target/nodo/srcpkgs" "$NODO/scripts/build-packages.sh" aarch64 pingers-server
# Sign a test feed with a throwaway key, never the real one, and check it as a device would:
FEED_PACKAGES="$PWD/packaging/nodo/feed-packages" FEED_SIGNER="pingers test" \
  FEED_SITE=/tmp/pingers-feed "$NODO/scripts/publish-feed.sh" sign /tmp/throwaway.pem x86_64
FEED_PACKAGES="$PWD/packaging/nodo/feed-packages" FEED_FINGERPRINT=<throwaway fingerprint> \
  FEED_SITE=/tmp/pingers-feed "$NODO/scripts/publish-feed.sh" check x86_64
```

The packages land in `$NODO/out/feed/<arch>/`. `nodo-dist` is reproducible: the same commit
gives the same tarball and SHA-256.

## 4. Map packs

Regions: Australia (`oceania/australia`), with a 40 GB budget on the data drive, which it shares
with the Monero blockchain. Turn on **UseDataDrive** for pingers-map-host **before** installing
it. Split the country into states (`crates/pingers-tiles/README.md`, "Sub-regions"): smaller packs
say less about the user. The region list is operator data and stays outside the repository.

```sh
pingers-tiles plan regions.toml --build https://build.protomaps.com/<YYYYMMDD>.pmtiles \
    --host-config map-host.local.toml --pmtiles ./pmtiles      # refuses if over max_disk
pingers-tiles cut  regions.toml --build https://build.protomaps.com/<YYYYMMDD>.pmtiles \
    --out ./packs --host-config map-host.local.toml --pmtiles ./pmtiles
rsync -a --partial --chown=_pingers-map-host:_pingers-map-host ./packs/ \
    root@<appliance>:<data drive>/pingers-map-host/
```

`map-host.local.toml` mirrors the device's `max_disk` and `hosted`. Packs are immutable and
their manifests change last, so uploading while the host runs is safe; no restart is needed.
Do not add `--delete`: the directory also holds the host's OHTTP key (`.ohttp-key`), and a new
key makes every phone that downloaded through the relay ask whether to trust it.

## 5. Local testing

**Tier 0**: `cargo xtask ci` (it includes the packaging test), the server's `through_relay`
tests, `crates/pingers-e2e`, and the push relay's ntfy script.

**Tier 1: the whole stack on the development machine, behind Caddy, with two emulators.**

1. `cargo xtask dev-ca` writes `target/dev-ca/ca.pem` and a leaf for `localhost`, `127.0.0.1`
   and `10.0.2.2` (the emulator's view of the host). The CA's key is not kept.
2. Start the four services with the dev configs, from the repository root; they have the shape
   of the templates and test keys only:
   `cargo run --release -p pingers-server -- packaging/nodo/dev/server.toml`, and likewise
   `push-relay.toml`, `relay.toml`, `map-host.toml`. Cut the Melbourne CBD test pack into
   `target/dev-stack/maps` first.
3. `caddy run --config packaging/nodo/dev/caddy.json` puts Caddy in front exactly as nodo-web
   does (N1): h2c and `flush_interval: -1` for the server (9443) and relay (9444), HTTP/1.1
   for the push relay (9445) and map host (9446), `/ohttp`, `/metrics` and `/v1/push` hidden.
4. Build debug APKs with `PINGERS_SERVER_HOST=https://10.0.2.2:9443`,
   `PINGERS_OHTTP_RELAY=https://10.0.2.2:9444/ohttp`, `PINGERS_PUSH_RELAY=https://10.0.2.2:9445`,
   `PINGERS_MAP_HOST=https://10.0.2.2:9446` and `PINGERS_EXTRA_CA=target/dev-ca/ca.pem` (debug
   builds only), plus `PINGERS_DEV_REGISTRATION_CODE=dev-registration-code` for the first
   emulator (the dev server.toml's code; the second joins by invite and needs none).
5. Two emulators (one foss with UnifiedPush through a local ntfy, one gms only if a real service
   account is in place locally): create a Party, join by invite, define a Spot, walk both in with
   `adb -s emulator-5554 emu geo fix <lon> <lat>`, and expect the knock, the Meetup with a live
   channel both ways, and chat.

**Tier 2: the real packages in nodo's x86-kvm VM** (`scripts/dev-vm.sh up` in a nodo checkout
at 4ad5661 or later): build x86_64 packages, sign a test feed with a throwaway key, copy it into the
VM as a directory feed, `AddFeed`, then install in dependency order (push relay, map host, server,
relay). Check the firewall (`_pingers-server` reaches nothing but 9472 and 9474; only
`_pingers-ohttp-relay` reaches 9471), reboot (services wait for the unlock), and restore a backup
to a fresh VM and check the gateway key hash is unchanged.

**Tier 3**: the aarch64 packages under emulation (`DEV_BOARD=qemu-virt`), then the device.

## 6. Bring-up

1. **DNS and router.** Point the four names at the home address. Forward TCP 443 to the
   appliance (or to the router's proxy, section 2). Porkbun DNS-01 needs no port 80.
2. **Feed.** In nodo-tui, Feeds, add
   `https://github.com/OceanEilonwy/pingers/releases/download/feed`, and accept the key only if
   the fingerprint is `9d:c4:00:6e:53:d8:99:c4:f9:5d:9c:96:b0:e9:1a:07`.
3. **Push relay.** Install `pingers-push-relay` (the device passphrase is needed, since it has
   sealed secrets). Set `operator` (truthful: the app shows a same-operator warning, which is
   correct here) and `fcm_project_id`. The **owner** pastes the service account JSON, on one line,
   into `fcm_service_account`. Keys, hash key and credential generate themselves.
4. **Map host.** UseDataDrive on, install, check `max_disk` and `hosted`, rsync the packs (section
   4). The server references its gateway, so it comes first. Its OHTTP key (`.ohttp-key` in the
   data directory) makes itself on first start.
5. **Server.** Install `pingers-server` (creates the 1 GiB encrypted volume) and approve its
   grant for `pingers-push-relay.server_credential`. Set `server_id`
   (`https://pingers.example.org`), `ohttp_relay_url` (`https://ohttp.example.org/ohttp`),
   `map_host`, `push_relay_public_url`, and `push_relay_key_config_hash`: the first pin (slot A,
   the active key; pins follow `activated_at`) that
   `/opt/nodo/apps/pingers-push-relay/libexec/pingers-push-relay pins
   /run/nodo/secrets/pingers-push-relay/config/push-relay.toml` prints over the owner's SSH. Leave
   `operator` at its default, the push relay's.
   The registration code generates itself (section 1.3); note it for the first phone.
6. **Relay.** Install `pingers-ohttp-relay`. Nothing to set.
7. **Exposure.** Section 2 for each site; wait until the certificate status is valid for all four.
8. **Smoke tests from outside the LAN:**
   - `curl -sI https://maps.example.org/catalog.json` gives 200, and
     `curl -s https://maps.example.org/ohttp-keys | wc -c` prints 41 (the gateway's key config).
   - `curl -s https://ohttp.example.org/v1/ohttp/config | openssl dgst -sha256 -binary | basenc
     --base64url | tr -d =` equals the served key's pin from `pingers-server pins
     /run/nodo/volumes/pingers-server/config/server.toml`.
   - `curl -s -o /dev/null -w '%{http_code}' -X POST https://pingers.example.org/ohttp` gives
     404: the gateway is not reachable directly.
   - `curl -s -o /dev/null -w '%{http_code}' -X POST https://push.example.org/v1/push` gives 404.
   - The `openssl s_client -groups X25519MLKEM768` check of section 2.1, for each name.
9. **Pins into CI.** Over the owner's SSH, run `pingers-server pins` and `pingers-push-relay pins`
   on the rendered configs (paths in steps 5 and 8); each prints one pin per line. Join each
   pair with a comma into `PINGERS_GATEWAY_KEY_PINS` and `PINGERS_PUSH_RELAY_KEY_PINS`. From
   outside, `cargo xtask pins --relay https://ohttp.example.org --push https://push.example.org`
   prints the same two lines from the running services: the served gateway key (fetched through
   the relay, as the app fetches it) and the push relay's published and next keys. Its gateway
   pin must be one of the two from `pingers-server pins` (the next slot is not served, so it only
   comes from there), and each of its push relay pins must be one `pingers-push-relay pins`
   prints. Then build the release APKs. (`cargo xtask pins <server.toml>
   <push-relay.toml>` prints both lines from copies of the configs, but those hold the private
   keys: on nodo, keep them on the device and use the binaries.)
10. **Two real phones** on mobile data, once with FCM and once with UnifiedPush: the tier-1
    scenario, including a ten-minute Meetup (streaming through the router).
11. **Backup.** A USB backup with the device passphrase (so it includes the device key), stored
    offsite.

After a power loss the server and push relay stay down until the owner unlocks the appliance:
no key is ever outside encryption.
