# Deploying Pingers on monokulo-nodo

This is the runbook for running the four Pingers services on a monokulo-nodo appliance: what the
packages contain, every key and secret, how the sites are exposed, how the signed package feed
is built and published, how to test locally, and the bring-up steps.

**In short:** install the one app `pingers` from the Pingers feed, answer two questions (your
domain and the Firebase key file) and give the device passphrase. Every key and code makes
itself on the appliance, the services are wired to each other, and the app's **Setup** page
(Apps, pingers, Setup, behind the device passphrase, any time) lists what is left to do by hand
with your own values filled in: section 6.

**Host names.** Every host name below is a placeholder under `example.org`. The real ones are
deployment values: the appliance derives them from the domain the owner gives at install (each
can be changed in pingers-server's settings), and the repository's CI variables
(`PINGERS_SERVER_HOST`, `PINGERS_OHTTP_RELAY`, `PINGERS_PUSH_RELAY`, `PINGERS_MAP_HOST`) carry
them into app builds; never Pingers source.

| Placeholder | Service |
|---|---|
| `pingers.example.org` | pingers-server (API; the OHTTP gateway sits behind the relay) |
| `ohttp.example.org` | pingers-ohttp-relay |
| `push.example.org` | pingers-push-relay |
| `maps.example.org` | pingers-map-host |

`cargo test -p xtask --test nodo_packaging` checks the packaging against nodo's own parsers
(manifests with the bundle and owner page, settings, rendering) and parses the rendered server,
push relay, map host and setup page configs, and their dev configs, with their own parsers.

## 1. The apps

nodo runs one service per package, so there are four nodo apps for the four services, and a
fifth, `pingers`, which the owner installs: it is a nodo **bundle** that installs the other four
with it, and it serves the owner's **Setup** page (a nodo owner page). One xbps recipe,
`packaging/nodo/srcpkgs/pingers-server/template`, builds all five from one `cargo build`: the
main package `pingers-server` and the subpackages `pingers-push-relay`, `pingers-ohttp-relay`,
`pingers-map-host` and `pingers` (symlinks beside it, as in void-packages).

The bundle and the owner page need monokulo-nodo's bundles and owner pages (nodo PR
"bundles-and-owner-pages"): `[bundle]` in `pingers`'s manifest names the members, installed in
this order, each before whatever references it (push relay, map host, server, relay), the two
grants the owner approves with the install, the two settings it asks for, and the member whose
data may go on the data drive (the map host). `[owner_page]` names the page on its loopback
port.

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
| 9479 | pingers, the setup page (HTTP/1.1, only `GET /owner`) | nodo's helper, for the owner; not a web site |

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
only the web upstreams, so it cannot reach 9471 or 9479. pingers (the setup page) reaches 9471
(the gateway's two key configs, by referencing `gateway-key-config-url`), 9472 (the push
relay's published and next keys, `server-url`) and 9474 (the map catalog, `gateway-url`), and
has `outbound = "internet"` for its checks of the four public names from the appliance; it
holds no private key. pingers-push-relay is the only service with `outbound = "internet"`.
nodo refuses dependency cycles, and the references run one way: `pingers-ohttp-relay →
pingers-server → pingers-push-relay`, `pingers-server → pingers-map-host`, and `pingers` → all
four, so the bundle installs push relay, map host, server, relay, then `pingers`. The owner's
domain is a setting of the push relay, the root of the references, so the server and the setup
page derive their addresses from it without a cycle.

### 1.1 Settings and secrets

"Sealed" means a nodo secret setting: encrypted under the device key, rendered only into memory
or the app's encrypted volume after the owner unlocks the device, and included in backups.

**pingers-server** (`server.toml.in`; encrypted 1 GiB volume for the databases, so the rendered
config with the gateway keys and the registration code stays inside the volume)

| Setting | Type | Default | Fills |
|---|---|---|---|
| `server_id` | url, required, public (`server-id`) | `https://pingers.$(pingers-push-relay.domain)` | `server_id`. The server's identity: changing it makes a new server |
| `operator` | string, required | `$(pingers-push-relay.operator)` | `server_operator` |
| `ohttp_relay_url` | url, required, public (`ohttp-relay-url`) | `https://ohttp.$(pingers-push-relay.domain)/ohttp` | `ohttp_relay_urls` |
| `map_host` | url, required, public (`map-host`) | `https://maps.$(pingers-push-relay.domain)` | `map_host`. Map downloads through the relay are forwarded to this device's map host on loopback (`map_host_gateway`, from pingers-map-host's `gateway-url`) |
| `logging` | enum `off`/`counts` | `off` | `logging` |
| `registration_code` | **secret**, generated (16 bytes, hex); if set by hand at least 128 bits of entropy: 32 to 128 hex digits, or 22 to 128 of `A-Z a-z 0-9 - _` (pingers-server refuses a weaker code) | | `registration_codes`: only a device presenting it can make an account (design B.1, section 1.3) |
| `ohttp_key_a`, `ohttp_key_b` | **secret**, generated (32 bytes, hex) | | `[[ohttp.keys]] secret` |
| `ohttp_key_a_id`, `ohttp_key_b_id` | int 1 to 255 | 1, 2 | `[[ohttp.keys]] key_id` |
| `ohttp_served_key_id` | int 1 to 255 | 1 | `[ohttp] served_key_id` |
| `push_relay_public_url` | url, required, public (`push-relay-public-url`) | `https://push.$(pingers-push-relay.domain)` | `[[push_relays]] public_url` |
| `gateway_url`, `gateway_key_config_url` | url, public endpoints | `http://127.0.0.1:9471/...` | read by pingers-ohttp-relay; pingers reads the key configs at `gateway-key-config-url` and its `/next` |

`push_relay_key_config_hash` is retired: `[[push_relays]] key_config_from_relay = true` makes
the server ask the push relay for its published key on loopback (`GET /v1/config`, at start and
every 5 minutes) and list that key's hash in `/v1/params`, so nothing is copied and a rotation
needs no change here. The server also reads `pingers-push-relay.server_credential` (a sealed
secret of the push relay) through the grant the owner approves with the bundle, and the push
relay's public `server-url` and `operator`.

**pingers-push-relay** (`push-relay.toml.in`; no volume, sealed secrets keep the rendered config
in memory)

| Setting | Type | Default | Fills |
|---|---|---|---|
| `domain` | string, required, public, **asked at install** | | nothing in this file: the domain the four sites are named under, such as `example.org`; pingers-server's addresses and both operators default from it |
| `operator` | string, required, public | `$(pingers-push-relay.domain)` | `operator` |
| `logging` | enum | `off` | `logging` |
| `hash_key` | **secret**, generated | | `hash_key` |
| `key_a`, `key_b` | **secret**, generated | | `[[keys]] private_key` |
| `key_a_id`, `key_b_id` | int 1 to 255 | 1, 2 | `[[keys]] key_id` |
| `key_a_activated_at`, `key_b_activated_at` | int, Unix seconds | 0, 4102444800 | `[[keys]] activated_at` (B is held in reserve) |
| `key_a_retired_at`, `key_b_retired_at` | int, 0 = not scheduled | 0 | `[[keys]] retired_at` |
| `server_credential` | **secret**, generated | | `[[servers]] credential`; also read by pingers-server |
| `fcm_service_account` | **secret**, required, **asked at install**: pasted by the owner | | `[fcm] service_account_json`; the relay takes the Firebase project from its `project_id` (`fcm_project_id` is retired) |
| `server_url` | url, public endpoint | `http://127.0.0.1:9472` | read by pingers-server and pingers |

**pingers-ohttp-relay** (`relay.toml.in`; no storage, no secrets): `response_timeout_seconds`
(int 5 to 300, default 30). The gateway addresses come from pingers-server.

**pingers-map-host** (`map-host.toml.in`; unencrypted data on the data drive): `max_disk`
(default `40GB`), `hosted` (default `oceania/australia`, space-separated),
`logging`, and `gateway_url` (default `http://127.0.0.1:9474`, published to pingers-server as
`gateway-url`: where it forwards map downloads made through the relay; leave it).

**pingers** (`setup.toml.in`; no storage; its one secret keeps the rendered config in memory):
`registration_code` (**secret**, defaults to `$(pingers-server.registration_code)`, read
through the grant approved with the bundle), `github_repository` (default
`OceanEilonwy/pingers`: where the setup page says the app builds' variables go) and
`release_tag` (default `v1.0.0-rc.1`: the tag it suggests). Everything else it shows comes
from the four services: their public addresses, the gateway's served and next key configs
(`GET /v1/ohttp/config` and `/v1/ohttp/config/next` on the gateway listener), the push relay's
published and next keys (`GET /v1/config`, `/v1/config/next`) and the map catalog, all on
loopback. The binary is `crates/pingers-setup`, named `pingers`.

### 1.2 The Setup page

Apps, pingers, **Setup** in nodo-tui asks for the device passphrase and shows the page; it can
be opened again at any time and is fetched afresh each time (`r` refreshes). It is the ordered
checklist of section 6, each step with its values from this appliance and, where the appliance
can tell, done or not: nodo fills in each site's DNS record, DNS check, certificate and
reachability, the services' health, the domain provider and the newest backup; pingers adds
its own checks through the public names (a TLS handshake that offers only X25519MLKEM768
against Mozilla's roots, HTTP/2 for the server and the relay, the expected answers, `404` for
`POST /ohttp` and `POST /v1/push`, and that the relay and the push relay hand out this
appliance's keys). Values have a Copy button (the terminal's clipboard, OSC 52) and stay
visible as plain text. The page shows the registration code; only the owner, with the device
passphrase, opens it.

### 1.3 Registration code

Without a registration code a Pingers server lets anyone who finds it make an account. The
package generates one on install (`registration_code`, sealed), so it never runs open. How it
reaches phones:

- **The first phone** (the owner's) gets it from the **first-phone QR code** on the Setup page
  (section 1.2). The code is the link `https://<server_id>/j#s1.<payload>`, in the same family
  as invite links: the payload (pingers-proto's `SetupPayload`) carries the server, the code and
  both pin sets the appliance computed from its own keys, served key first. The owner scans it
  with the phone's camera; App Links (or the `/j` landing page) open the app on a confirm step,
  "Set up Pingers on this phone", and the app registers with the code on the server the link
  names, with that server's own account (the app keeps one per server). The app first checks
  the link: if it knows that server already (the one the build was made for, or one recorded
  earlier) the link's served pins must be among the pins it keeps; a server it does not know yet
  (another server, or the build's own in a build without pins) is recorded with the link's
  pins, as an invite's server is, once the keys it serves match them, and keeps them. Release
  builds accept it. A debug build made with `PINGERS_DEV_REGISTRATION_CODE=<code>` still works for
  tests.
- **Everyone else** gets it in the invite link: a phone that registered with a code puts it in
  every invite it makes, and the friend's phone presents it when it opens the link. So the code
  travels as far as invite links do; it is a bearer capability like the link itself.
- **A refused code** (wrong, or reset since) is not tried again until another one is offered
  (a new invite or setup link, or another build), so a phone never retries a 403 in a loop.

To stop new accounts from old invite links, Reset `registration_code` in nodo-tui (Apps,
pingers-server, Settings); the Setup page then shows a new first-phone code for phones that
should keep inviting. Phones that already
have an account are not affected: accounts live in `quota_db`
(`/var/lib/apps/pingers-server/quota.sqlite`, on the encrypted volume), and a registered phone
proves its key without any code. A restart keeps every account and every bearer: a bearer is
still accepted until the hour it was issued for ends, a deleted account stays deleted, and the
database holds only each account's public key, when it registered, and a digest of each bearer
(never a usable bearer). The code only gates new accounts, which the server also limits to
`signups_per_hour` (default 60) whatever the code.

### 1.4 Keys and rotation

No key is made off the device: every 32-byte private key is a generated secret (any 32 random
bytes are a valid X25519 key). The FCM service account is the only secret a person handles, and
only the owner, pasting it into nodo-tui on the device. No tool reads or copies it.

Both HPKE key sets (the OHTTP gateway's and the push relay's) have two slots, and app builds pin
the hashes of both, so the app never meets an unpinned key. The map host has one OHTTP key, made
on its first start (`ohttp_key_file`); its pin, `PINGERS_MAP_HOST_KEY_PINS`, is on the Setup page
too, and changing that key needs a new app build with the new pin (relayed map downloads stop
until then; direct ones do not):

1. At install, slots A and B are generated. A is served (`ohttp_served_key_id = 1`; the push
   relay's B activates in 2100). Put both pins in CI: the Setup page shows the two lines,
   `PINGERS_GATEWAY_KEY_PINS` and `PINGERS_PUSH_RELAY_KEY_PINS`, read from the running services
   (section 6, step 7). A pin is the unpadded base64url SHA-256 of a key config, 43 characters;
   the gateway serves its other slot's config at `GET /v1/ohttp/config/next` on its own
   listener, and the push relay publishes its next key at `GET /v1/config/next`.
2. Yearly, serve B: set `ohttp_served_key_id = 2`; for the push relay set `key_b_activated_at` to
   now and `key_a_retired_at` to at least 30 days later. Every configured key keeps opening
   requests meanwhile. pingers-server lists the push relay's new key by itself within 5
   minutes (`key_config_from_relay`); nothing to copy.
3. Once no supported app build pins A only, `Reset` slot A (nodo generates a new one); for the
   push relay also set `key_a_activated_at` to the next rotation (a year ahead) and
   `key_a_retired_at` to 0, so the new A is published as the next key. Then copy the two pin
   lines from the Setup page into CI again (B and the new A, the served key first) and ship the
   next app release.

On a suspected compromise, do steps 2 and 3 at once.

### 1.5 Invite links and App Links

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
and N2, which monokulo-nodo merged in #12, and bundles and owner pages (branch
`bundles-and-owner-pages`, 226610d, whose PR is not merged yet): an older install review refuses
the `pingers` app, and the workflow stops with "update NODO_REV". `NODO_REV` and the
`nodo-manifest` and `nodo-settings` pins in `Cargo.toml` (the packaging test) all name 226610d,
the branch head. Until that branch is on GitHub, the feed workflow cannot check it out and
`cargo` cannot fetch the pins, so `cargo xtask ci` on GitHub fails. A merge commit keeps
226610d reachable; after a squash merge all three move to the squashed commit. (Before bundles
all were 4ad5661, the merge of #12.)

**Building locally** (Docker, a monokulo-nodo checkout with N1, N2, bundles and owner pages, at `$NODO`):

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
   `target/dev-stack/maps` first. Then the setup page,
   `cargo run --release -p pingers-setup -- packaging/nodo/dev/setup.toml`;
   `curl -s http://127.0.0.1:9479/owner` prints the page as JSON, with the first-phone link of
   this stack (its public checks have nothing to reach and say so).
3. `caddy run --config packaging/nodo/dev/caddy.json` puts Caddy in front exactly as nodo-web
   does (N1): h2c and `flush_interval: -1` for the server (9443) and relay (9444), HTTP/1.1
   for the push relay (9445) and map host (9446), `/ohttp`, `/metrics` and `/v1/push` hidden.
4. Build debug APKs with `PINGERS_SERVER_HOST=https://10.0.2.2:9443`,
   `PINGERS_OHTTP_RELAY=https://10.0.2.2:9444/ohttp`, `PINGERS_PUSH_RELAY=https://10.0.2.2:9445`,
   `PINGERS_MAP_HOST=https://10.0.2.2:9446` and `PINGERS_EXTRA_CA=target/dev-ca/ca.pem` (debug
   builds only). The first emulator registers through the setup page's first-phone link:
   `adb -s emulator-5554 shell am start -a android.intent.action.VIEW -d '<link>'` with the
   link from the page (or build it with `PINGERS_DEV_REGISTRATION_CODE=dev-registration-code-0123456789abcdef`,
   the dev server.toml's code); the second joins by invite and needs none.
5. Two emulators (one foss with UnifiedPush through a local ntfy, one gms with Play services
   when FCM is wanted: `cargo xtask local-stack --fcm-service-account <key.json> --fcm-project
   <id>` points the local push relay at a service account key kept outside the repository, read
   in place and never copied; there is no default): create a Party, join by invite, define a Spot, walk both in with
   `adb -s emulator-5554 emu geo fix <lon> <lat>`, and expect the knock, the Meetup with a live
   channel both ways, and chat.

`cargo xtask local-stack` does steps 1 to 3 in one command with fresh keys outside the
repository, and writes step 4's variables to `<dir>/app.env` (see `xtask/src/stack.rs`).

**Parties on two servers.** `cargo xtask local-stack --second-server` also runs a second
pingers-server ("server B", operator "Second server (local)") with its own keys, databases and
its own pingers-ohttp-relay, behind the same TLS front and dev CA: the emulator reaches B at
`https://10.0.2.2:9447` and its relay at `https://10.0.2.2:9448/ohttp` (B listens on
127.0.0.1:9475, its gateway on :9476, its relay on :9477). B's `/v1/params` lists its own relay,
and the same push relay and map host as server A; the push relay allows B with a credential of
its own. B requires a registration code of its own, so "code required" and "code refused" can be
tried against it; A is unchanged. B's origin, relay, gateway and push relay pins, CA and code are
in `<dir>/second-server.env`, under `app.env`'s variable names. `--fresh` regenerates B too. To
check B by hand: `curl --cacert <dir>/ca/ca.pem https://127.0.0.1:9447/v1/params` answers, and
`curl -s --cacert <dir>/ca/ca.pem https://127.0.0.1:9448/v1/ohttp/config | openssl dgst -sha256
-binary | basenc --base64url | tr -d =` prints B's served gateway pin, the first of
`PINGERS_GATEWAY_KEY_PINS` in `second-server.env`.

**Tier 2: the real packages in nodo's x86-kvm VM** (`scripts/dev-vm.sh up` in a nodo checkout
with bundles and owner pages): build x86_64 packages, sign a test feed with a throwaway key, copy it into the
VM as a directory feed, `AddFeed`, then install `pingers` (the bundle installs the four services,
asking for a test domain and a throwaway service account key). Open Apps, pingers, Setup and
check the page. Check the firewall (`_pingers-server` reaches nothing but 9472 and 9474;
`_pingers-ohttp-relay` reaches only 9471; `_pingers` reaches 9471, 9472 and 9474 on loopback), reboot (services wait for the unlock), and restore a backup
to a fresh VM and check the gateway key hash is unchanged.

**Tier 3**: the aarch64 packages under emulation (`DEV_BOARD=qemu-virt`), then the device.

## 6. Bring-up

The appliance does the rest itself: every key, the hash key, the server credential and the
registration code generate themselves at install, the server learns the push relay's key and
credential without a copy, and the addresses follow from the domain. What is left for the owner
is this list, which the Setup page (Apps, pingers, Setup; section 1.2) shows in the same order
with this appliance's values filled in and each step marked done where the appliance can see it.
`example.org` below stands for your domain.

**Install.**

1. **Feed.** In nodo-tui, Get apps, Sources, add
   `https://github.com/OceanEilonwy/pingers/releases/download/feed`, and accept the key only if
   the fingerprint is `9d:c4:00:6e:53:d8:99:c4:f9:5d:9c:96:b0:e9:1a:07`.
2. **Install `pingers`.** It shows the five apps it installs with their permissions, the two
   grants (pingers-server reads the push relay's credential; pingers reads the server's
   registration code), and keeps the map packs on the data drive when there is one. It asks for:
   - **Your domain**, such as `example.org`. The sites become `pingers.example.org`,
     `ohttp.example.org`, `push.example.org` and `maps.example.org`, and both operators say
     `example.org` (the app shows a same-operator warning, which is correct here). Each can be
     changed later in pingers-server's and pingers-push-relay's settings.
   - **The Firebase service account**: the **owner** pastes the JSON key file of a service
     account with only the Firebase Cloud Messaging API Admin role (project `pingers-a6b31`).
     It is sealed at once; the relay takes the project from the key file.
   - The **device passphrase**, which sealed secrets and the server's encrypted volume need.

**The Setup page's checklist.**

1. **Connect your domain.** At Porkbun: Account, API Access, create an API key (an API key and a
   secret key), then the domain's Details, turn on API Access. On the appliance: Web, Domain
   providers, Add, Porkbun, paste both. nodo then publishes the four A records and keeps them
   pointed at your public address (dynamic DNS), and gets the certificates through DNS-01, so
   port 80 need not be forwarded. *Done when* a connected provider holds the domain.
2. **Put the four sites online.** For pingers-server, pingers-ohttp-relay, pingers-push-relay
   and pingers-map-host: Apps, the app, Hosting, Internet, pick `example.org` and keep the
   suggested address part (`pingers`, `ohttp`, `push`, `maps`), check, Put it online. On the
   confirm screen turn the rate limit **off for `ohttp.example.org`** (phones behind carrier NAT
   share an address) and keep it on for the others, unless the router terminates TLS (section
   2.3: then off for all four). The records, which nodo publishes (create them by hand only with
   a provider nodo cannot drive):

   | Type | Name | Value |
   |---|---|---|
   | A | `pingers` | your public address |
   | A | `ohttp` | your public address |
   | A | `push` | your public address |
   | A | `maps` | your public address |

   The page shows each record with the public address nodo found filled in. *Done when* each
   site is public, its name resolves to the public address and its certificate is issued.
3. **Forward port 443 to this appliance.** On the router, forward TCP 443 to the appliance's LAN
   address (the page shows it). If the router runs its own reverse proxy in front: set it to
   pass TLS through (TCP or SNI forwarding) for the four names, the recommended setup (section
   2.2); if it can only terminate TLS, section 2.3's three conditions apply (X25519MLKEM768,
   streaming with HTTP/2 to the appliance and no body buffering, idle timeout over 60 seconds,
   and the rate limit off for all four). *Done when* nodo's reachability check passes and the
   appliance's own TLS check of each name, through the public address, gets a valid certificate
   with X25519MLKEM768 (and HTTP/2 for the server and the relay). Routers that do not loop
   connections from inside back to the public address make that check say it could not be
   tried; then check from a phone on mobile data (section 2.1's commands).
4. **Check the services answer.** *Done when* the five services are running and healthy, the
   gateway and the push relay answer on the appliance, `https://pingers.example.org/v1/params`
   and `https://maps.example.org/catalog.json` answer 200, `POST https://pingers.example.org/ohttp`
   and `POST https://push.example.org/v1/push` answer 404, and the relay and the push relay hand
   out this appliance's keys. These replace the hand-run curl checks.
5. **Firebase wake-ups.** Pasted at install. To replace the key: Apps, pingers-push-relay,
   Settings, Firebase service account; then delete the old key in the Google Cloud console.
   *Done when* the push relay serves FCM.
6. **Upload the map packs** (section 4): cut them with `pingers-tiles` from the newest daily
   build, then `rsync -a --partial --chown=_pingers-map-host:_pingers-map-host ./packs/
   root@<appliance>:<the map host's data directory>/`; the page shows the command with the
   appliance's address and the directory filled in. Never `--delete`. *Done when* the map host
   serves at least one region.
7. **Copy the hosts and pins into GitHub.** At github.com/OceanEilonwy/pingers: Settings,
   Secrets and variables, Actions, Variables, New repository variable, for each:

   | Variable | Value |
   |---|---|
   | `PINGERS_SERVER_HOST` | `https://pingers.example.org` |
   | `PINGERS_OHTTP_RELAY` | `https://ohttp.example.org/ohttp` |
   | `PINGERS_PUSH_RELAY` | `https://push.example.org` |
   | `PINGERS_MAP_HOST` | `https://maps.example.org` |
   | `PINGERS_GATEWAY_KEY_PINS` | the gateway's two pins, comma-separated, served key first (from the page) |
   | `PINGERS_PUSH_RELAY_KEY_PINS` | the push relay's two pins, the same way (from the page) |
   | `PINGERS_MAP_HOST_KEY_PINS` | the map host gateway's pin (one key; from the page, which reads it from the map host's `/ohttp-keys`) |

   The page also shows the seven as `gh variable set <NAME> --repo OceanEilonwy/pingers
   --body=<value>` lines (`--body=`, since a pin can start with `-`).

   Secrets, which the appliance does not hold (android/docs/release.md says where each comes
   from): create the `release` environment (Settings, Environments) and in it the secrets
   `PINGERS_KEYSTORE_BASE64`, `PINGERS_KEYSTORE_PASSWORD`, `PINGERS_KEY_ALIAS` and
   `GOOGLE_SERVICES_JSON`; set `GOOGLE_SERVICES_JSON` as a repository secret too, or `ci.yml`
   skips the gms build. The package feed (the `feed` environment's `PINGERS_FEED_SIGNING_KEY` and
   the `feed` release, "The package feed" above) was set up before the install. The appliance
   cannot see GitHub, so tick this step off yourself. A test in pingers-setup checks that the
   page names every variable and secret `release.yml` and `ci.yml` read. From outside, `cargo
   xtask pins --relay https://ohttp.example.org --push https://push.example.org` cross-checks
   the served gateway pin and both push relay pins against the running services.
8. **Make a release.** On the commit to release: `git tag -a v1.0.0-rc.1 -m "Pingers
   v1.0.0-rc.1"` and `git push origin v1.0.0-rc.1` (the page uses pingers' `release_tag`
   setting). `release.yml` builds both APKs with the variables and publishes a pre-release;
   install it with Obtainium, pre-releases on (docs/install.md).
9. **Set up your first phone.** Scan the page's first-phone QR code with the phone's camera;
   Pingers opens on "Set up Pingers on this phone" and registers (section 1.3). The page also
   shows the link and the registration code as text. Then the tier-1 scenario with a second
   phone, on mobile data, once with FCM and once with UnifiedPush, including a ten-minute
   Meetup (streaming through the router).
10. **Make a backup.** System, Backups, to a USB drive, with the device passphrase so it includes
    the device key; store it offsite. *Done when* a backup with the device key is newer than the
    install.

After a power loss the server, the push relay and the setup page stay down until the owner
unlocks the appliance: no key is ever outside encryption.
