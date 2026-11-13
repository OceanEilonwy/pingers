# Releasing the Android app

Release builds are signed with the Pingers **upload key**, and every deployment host comes from
the environment. None of it is in the repository: not the keystore, not its password, not a
host. Two workflows use them (`.github/workflows/`):

- `ci.yml`, on every push and pull request: `cargo xtask ci`, then for each flavour the debug
  APK, lint and unit tests. No signing key and no host; gms only when `GOOGLE_SERVICES_JSON` is
  set, otherwise it is skipped with a notice.
- `release.yml`, by hand or on a tag `v*`: the signed release APK and AAB of both flavours,
  uploaded as the run's artifacts. Its job runs in the `release` environment.

## Building a release here

`cargo xtask release-android` is the build `release.yml` runs, from the same variables:

```sh
export PINGERS_KEYSTORE_PATH=~/.config/pingers/keys/android/upload-keystore.p12
export PINGERS_KEYSTORE_PASSWORD="$(cat ~/.config/pingers/keys/android/upload-keystore.password)"
export PINGERS_KEY_ALIAS=pingers-upload
export PINGERS_SERVER_HOST=https://...   # and the other hosts below; unset, they are .invalid
cargo xtask release-android               # foss, plus gms if PINGERS_GOOGLE_SERVICES_JSON is set
cargo xtask release-android --flavour gms --flavour foss
```

It refuses to start without the three signing variables, warns about each host left unset, runs
`./gradlew assemble<Flavour>Release bundle<Flavour>Release` (without the configuration cache, so
the signing config is not stored in `android/.gradle`), and then checks every APK with
`apksigner verify --print-certs` and every AAB with `keytool -printcert -jarfile`: each must be
signed by the keystore's certificate and nothing else. The outputs are in
`android/app/build/outputs/apk/<flavour>/release/` and `.../bundle/<flavour>Release/`.

Gradle itself also refuses: any release task without `PINGERS_KEYSTORE_PATH`,
`PINGERS_KEYSTORE_PASSWORD` and `PINGERS_KEY_ALIAS` stops at `checkReleaseSigning`, which names
what is missing. A release build is never signed with the debug key. When running a signed
release through Gradle directly, pass `--no-configuration-cache` as the xtask does: otherwise
Gradle stores the signing config (encrypted, but password included) under `android/.gradle`.

## Secrets and variables

Local values live under `~/.config/pingers/keys/` (directory 0700, files 0600), described in
its `README.md`. The repository has no remote yet, so none of these is set on GitHub; once it
has one, run the commands from the repository's checkout. `--env release` puts a secret in the
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

The hosts are variables because they ship inside the APK anyway. `release.yml` reads
`vars.NAME || secrets.NAME`, so a host can be a secret instead (`gh secret set NAME --env
release`) while it should not appear in logs. Each host can also be passed to Gradle as a
property (`-PpingersServerHost=`, `-PpingersOhttpRelay=`, `-PpingersPushRelay=`,
`-PpingersMapHost=`).

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
