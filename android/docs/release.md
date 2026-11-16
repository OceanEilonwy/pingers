# Releasing the Android app

Release builds are signed with the Pingers **upload key**, and every deployment host comes from
the environment. None of it is in the repository: not the keystore, not its password, not a
host. Two workflows use them (`.github/workflows/`):

- `ci.yml`, on every push and pull request: `cargo xtask ci`, then for each flavour the debug
  APK, lint and unit tests. No signing key and no host; gms only when `GOOGLE_SERVICES_JSON` is
  set, otherwise it is skipped with a notice.
- `release.yml`, on a tag `v*`: the signed release APK and AAB of both flavours, published as
  a GitHub Release, which is where users install and update from with Obtainium
  ([`docs/install.md`](../../docs/install.md)). It has no manual trigger, and its `version` and
  build jobs both stop unless the ref is a `v*` tag whose commit is on `main`, before any secret
  is read. Its build job runs in the `release` environment; only its publish job can write to
  the repository. The repository settings that back this up (the environment limited to `v*`
  tags with a required reviewer, and a tag ruleset) are under "Repository settings" below.

## Publishing a release

1. Push a tag `v<major>.<minor>.<patch>`, or `v<major>.<minor>.<patch>-rc.<n>` (also `-alpha.<n>`,
   `-beta.<n>`) for a pre-release, on the commit to release. An annotated tag's message becomes
   the release notes; for a lightweight tag the notes list the commit subjects since the
   previous release tag.
2. `release.yml`'s `version` job refuses a malformed tag, or one whose version does not come
   after every other `v*` tag; then each flavour builds, and the `publish` job creates the
   release with:
   - `pingers-<version>-gms.apk` and `pingers-<version>-foss.apk`, the files Obtainium installs
     (its APK filters are `pingers-.*-gms\.apk$` and `pingers-.*-foss\.apk$`);
   - `pingers-<version>-gms.aab` and `pingers-<version>-foss.aab`, for app stores;
   - `SHA256SUMS`;
   - a body with the notes, which APK to choose, the upload certificate's SHA-256 (below) and
     every file's SHA-256.

   A pre-release tag makes a GitHub pre-release, which Obtainium offers only to users who turned
   on "Include prereleases"; any other tag becomes the latest release.

`versionName` is the tag without its `v`, and `versionCode` comes from the tag alone
(`xtask/src/release_version.rs`): `major * 10_000_000 + minor * 100_000 + patch * 1_000 +
stage`, where stage is `n` for `alpha.n`, `100 + n` for `beta.n`, `200 + n` for `rc.n` and 999 for
a final release (minor, patch and `n` up to 99, major up to 209). So `v1.0.0-rc.1` is 10000201,
`v1.0.0` 10000999 and `v1.0.1` 10001999: every later tag has a larger code, which Android and
Obtainium need to accept the update. `cargo xtask release-version <tag>` prints the numbers.

## Building a release here

`cargo xtask release-android` is the build `release.yml` runs, from the same variables:

```sh
export PINGERS_KEYSTORE_PATH=~/.config/pingers/keys/android/upload-keystore.p12
export PINGERS_KEYSTORE_PASSWORD="$(cat ~/.config/pingers/keys/android/upload-keystore.password)"
export PINGERS_KEY_ALIAS=pingers-upload
export PINGERS_SERVER_HOST=https://...   # and the other hosts below; unset, they are .invalid
cargo xtask release-android                       # HEAD's v* tag; foss, plus gms if
                                                  # PINGERS_GOOGLE_SERVICES_JSON is set
cargo xtask release-android --tag v1.0.0-rc.1 --flavour gms --flavour foss
cargo xtask release-notes --tag v1.0.0-rc.1 --dir target/release-android   # the release body
```

It takes the version from `--tag`, or from the `v*` tag on `HEAD`, and refuses a tag that does
not come after every other `v*` tag. It refuses to start without the three signing variables or the two key pin variables, or
when the keystore's certificate is not the published upload certificate below, and warns about
each host left unset. It runs `./gradlew -PpingersVersionCode=<code>
-PpingersVersionName=<name> assemble<Flavour>Release bundle<Flavour>Release` (without the
configuration cache, so the signing config is not stored in `android/.gradle`), and then checks
the APK with `apksigner verify --print-certs` and the AAB with `keytool -printcert -jarfile`:
each must be signed by the keystore's certificate and nothing else. It then copies them to
`target/release-android/pingers-<version>-<flavour>.apk` and `.aab`, the names the GitHub
Release uses. `release-notes` writes `SHA256SUMS` beside them and prints the body.

Gradle itself also refuses: any release task without `PINGERS_KEYSTORE_PATH`,
`PINGERS_KEYSTORE_PASSWORD` and `PINGERS_KEY_ALIAS` stops at `checkReleaseSigning`, which names
what is missing. A release build is never signed with the debug key. When running a signed
release through Gradle directly, pass `--no-configuration-cache` as the xtask does: otherwise
Gradle stores the signing config (encrypted, but password included) under `android/.gradle`.

## Secrets and variables

Local values live under `~/.config/pingers/keys/` (directory 0700, files 0600), described in
its `README.md`. On GitHub (`OceanEilonwy/pingers`) the `release` environment holds the
signing secrets and the hosts are set; the key pin variables must be set before the first
release tag, since a release fails without them. Run the commands below
from the repository's checkout to change one. `--env release` puts a secret in the
`release` environment, where it can be limited to tags and protected by required reviewers;
create the environment first under Settings > Environments.

| Name | Kind | What it is | Local value | Add it to GitHub |
|---|---|---|---|---|
| `PINGERS_KEYSTORE_BASE64` | secret (`release`) | The upload keystore, PKCS12, RSA 4096, base64. `release.yml` decodes it into a 0600 file in the runner's temp directory and points `PINGERS_KEYSTORE_PATH` at it. | `~/.config/pingers/keys/android/upload-keystore.p12` | `base64 -w0 ~/.config/pingers/keys/android/upload-keystore.p12 \| gh secret set PINGERS_KEYSTORE_BASE64 --env release` |
| `PINGERS_KEYSTORE_PASSWORD` | secret (`release`) | The keystore's password, which is also the key's (PKCS12 has one). | `~/.config/pingers/keys/android/upload-keystore.password` | `gh secret set PINGERS_KEYSTORE_PASSWORD --env release < ~/.config/pingers/keys/android/upload-keystore.password` |
| `PINGERS_KEY_ALIAS` | secret (`release`) | The key's alias in the keystore: `pingers-upload`. Not sensitive; a secret so the signing inputs stay together. | `~/.config/pingers/keys/README.md` | `gh secret set PINGERS_KEY_ALIAS --env release --body pingers-upload` |
| `GOOGLE_SERVICES_JSON` | secret (repository and `release`) | Firebase's `google-services.json` for the gms flavour's FCM, with a client for `org.pingers.app`. `ci.yml` skips gms without it; `release.yml` fails gms without it. Locally, `PINGERS_GOOGLE_SERVICES_JSON` or the Gradle property `pingers.googleServicesJson` points at the file. | Downloaded from the Firebase console (Project settings > Your apps); keep it outside the repository. Not the service-account key in `~/.config/pingers/fcm/`, which is the server's. | `gh secret set GOOGLE_SERVICES_JSON < google-services.json` and `gh secret set GOOGLE_SERVICES_JSON --env release < google-services.json` |
| `PINGERS_SERVER_HOST` | variable | The Pingers server and OHTTP gateway the app talks to (`BuildConfig.SERVER_HOST`, into `Core.open`). Unset: `https://pingers.invalid`. | The deployment's choice; not a secret. | `gh variable set PINGERS_SERVER_HOST --body https://...` |
| `PINGERS_OHTTP_RELAY` | variable | The OHTTP relay (`BuildConfig.OHTTP_RELAY`). Unset: `https://ohttp.pingers.invalid`. | As above. | `gh variable set PINGERS_OHTTP_RELAY --body https://...` |
| `PINGERS_PUSH_RELAY` | variable | The push relay (`BuildConfig.PUSH_RELAY`). Unset: `https://push.pingers.invalid`. | As above. | `gh variable set PINGERS_PUSH_RELAY --body https://...` |
| `PINGERS_MAP_HOST` | variable | The publisher's map host, the last link of the map host chain (`BuildConfig.MAP_HOST`, [maps.md](maps.md)). Unset: `https://maps.pingers.invalid`. | As above. | `gh variable set PINGERS_MAP_HOST --body https://...` |
| `PINGERS_GATEWAY_KEY_PINS` | variable | The OHTTP gateway's key config hashes (`BuildConfig.GATEWAY_KEY_PINS`, into `Core.open`): base64url SHA-256, comma-separated, the key served now and the next one (two-slot rotation). The core never pins on first use; a malformed entry fails the build. Required for a release (`release-android` fails without it); unset, a development build reaches the server only once a setup link or invite has recorded its pins. | `cargo xtask pins --relay <url> --push <url>` (served key, from the live relay) and `pingers-server pins` (both slots); compare them before storing. Public values. | `gh variable set PINGERS_GATEWAY_KEY_PINS --body <hashA>,<hashB>` |
| `PINGERS_PUSH_RELAY_KEY_PINS` | variable | The push relay's key config hashes (`BuildConfig.PUSH_RELAY_KEY_PINS`), in the same form. No push token is sealed to a key outside the set. Required for a release; unset, a development build seals no push token until a setup link or invite brings pins. | `cargo xtask pins --relay <url> --push <url>` (published and next key, from the live relay) and `pingers-push-relay pins`. Public values. | `gh variable set PINGERS_PUSH_RELAY_KEY_PINS --body <hashA>,<hashB>` |
| `PINGERS_EXTRA_CA` | local only, debug builds | A test CA (a PEM file's path, relative to the repository, or the PEM itself) that debug builds trust instead of Mozilla's roots, for a local stack. Release builds never read it. | `target/dev-ca/ca.pem` | Never set in CI. |

The hosts are variables because they ship inside the APK anyway. `release.yml` reads
`vars.NAME || secrets.NAME`, so a host can be a secret instead (`gh secret set NAME --env
release`) while it should not appear in logs. Each host can also be passed to Gradle as a
property (`-PpingersServerHost=`, `-PpingersOhttpRelay=`, `-PpingersPushRelay=`,
`-PpingersMapHost=`).

## Repository settings

The workflow refuses anything but a `v*` tag on `main`, but a workflow file can be changed on a
branch, so the signing secrets also need GitHub to refuse. Set once, by the repository owner:

- **Settings > Environments > `release`**: Deployment branches and tags: "Selected branches and
  tags", with one tag rule `v*` and no branch rule; Required reviewers: the owner (and "Prevent
  self-review" off if the owner is the only maintainer); "Allow administrators to bypass" off.
- **Settings > Rules > Rulesets > New tag ruleset**: target tags matching `v*`, enforcement
  active, rules "Restrict creations", "Restrict updates" and "Restrict deletions", with the
  owner as the only bypass actor. Only the owner can then push, move or delete a release tag.

## The upload key

Created 2026-11-13 with `keytool -genkeypair -storetype PKCS12 -keyalg RSA -keysize 4096
-validity 10950 -alias pingers-upload`, valid until 2056-11-05. Its certificate's SHA-256
fingerprint:

```
AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16
```

With Play App Signing this is the upload key: Google holds the app signing key, and a lost or
leaked upload key can be reset from the Play Console. F-Droid and direct APK downloads are
signed by this key itself, so for them losing it means a new app id: keep an offline backup of
`~/.config/pingers/keys/`.
