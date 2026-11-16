# Pingers for Android

The Android shell: Kotlin, Jetpack Compose and Material 3, min SDK 29. It draws
every screen of the design canvas (`docs/design-canvas`). The app runs on
`CoreRepository`, over the Rust core (`crates/pingers-ffi`, through UniFFI): each
pingers-screens view model arrives as a record and maps one to one onto `UiState`.
Until the crates behind pingers-client merge, `Core.open` answers
`CoreException.Unavailable`, so the screens show their empty states. Debug builds
launched with `--ez fake true` run on `FakeRepository` (the canvas's example) instead,
for screenshots:

```sh
adb shell am start -n org.pingers.app.debug/org.pingers.app.MainActivity --ez fake true --es screen Compass
```

## Build

Needs a JDK 21, Rust (the workspace's `rust-version`), the Android targets,
`cargo-ndk` and NDK 27.0.12077973 (the version in `app/build.gradle.kts`):

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
sdkmanager "ndk;27.0.12077973"

export JAVA_HOME=/opt/android-studio/jbr   # any JDK 21
./gradlew assembleDebug lint
./gradlew installDebug
./gradlew assembleRelease -PpingersSplits   # signed (below); one APK per ABI, plus a universal one
```

**Release builds** are signed with the upload key from `PINGERS_KEYSTORE_PATH`,
`PINGERS_KEYSTORE_PASSWORD` and `PINGERS_KEY_ALIAS`, and fail without them (never a debug
signature); `cargo xtask release-android` builds both flavours and checks their signatures. The
deployment hosts (`PINGERS_SERVER_HOST`, `PINGERS_OHTTP_RELAY`, `PINGERS_PUSH_RELAY`,
`PINGERS_MAP_HOST`) come from the environment too. [docs/release.md](docs/release.md) lists
every CI secret and variable and where its local value lives.

Gradle drives cargo, so the Rust library and the Kotlin bindings can't drift from
the crates:

| Task | What it runs |
|---|---|
| `cargoHost` | `cargo build -p pingers-ffi --features bindgen` for this machine: the library and the `uniffi-bindgen` binary. |
| `uniffiBindings` | `uniffi-bindgen generate --library` on the host library: Kotlin in `build/rust/kotlin` (package `uniffi.pingers`, set in `crates/pingers-ffi/uniffi.toml`). |
| `cargoNdkDebug` | `cargo ndk -t arm64-v8a -t x86_64` with the `dev` profile, for debug builds. |
| `cargoNdkRelease` | The same with the workspace's `android` profile (LTO, one codegen unit, stripped), for release builds. |

Their outputs are added as generated Kotlin sources and `jniLibs`. JNA comes as an
`@aar` (it carries its own native library); `proguard-rules.pro` keeps JNA and the
bindings through R8. Set `CARGO_BUILD_JOBS` to limit cargo's parallelism.

**Flavours and app ids.** `gms` (Play services location, FCM and UnifiedPush) is
`org.pingers.app`; `foss` (no Google code, UnifiedPush only) is `org.pingers.app.foss`.
Debug builds add `.debug`, so all four install side by side.

**FCM.** The gms flavour reads Firebase's `google-services.json` from the Gradle property
`pingers.googleServicesJson` or `PINGERS_GOOGLE_SERVICES_JSON`; it is never in the
repository. The file needs a client for `org.pingers.app`, and the gms debug build
(`org.pingers.app.debug`) uses that client too. That works while the Firebase API key is
not restricted to Android apps by package name. If it is, register a second Android app
`org.pingers.app.debug` in the Firebase project (with the debug signing certificate's
SHA-1 if the key is restricted by certificate as well), download the new
`google-services.json`, and set `firebasePackage` in `app/build.gradle.kts` per build type.

**Adding a call.** Export it from `crates/pingers-ffi` (`#[uniffi::export]`, records with
`uniffi::Record`, errors with `uniffi::Error`, which Kotlin sees as `...Exception`),
rebuild, and call it from Kotlin as `uniffi.pingers.someCall()`. `CoreInfo.kt` is the
one Kotlin caller today.

## Structure

`app/src/main/kotlin/org/pingers/app/`

| Path | What it holds |
|---|---|
| `data/UiState.kt` | One plain data class per screen (`HomeState`, `PartyState`, `MeetupState`, ...). No behaviour. |
| `data/PingersRepository.kt` | The one interface the UI uses: a `StateFlow` per screen and a method per user action. |
| `data/CoreRepository.kt`, `CoreMapping.kt` | The repository over the Rust core, and the record-to-UiState mapping. |
| `data/FakeRepository.kt` | The canvas's example data (Climbing crew at Riverside Park), for previews and screenshots. |
| `data/PolygonEditor.kt` | FakeRepository's Draw area editing model (tap to add, close on the first point, drag, midpoints, select and delete, undo, clear), kept out of the composables. |
| `Navigator.kt` | `Route` (every canvas screen) and a plain back stack, as in monokulo-fob. |
| `MainActivity.kt` | Edge-to-edge host, routes, notification and join-link intents (an invite link opens Open invite; a first-phone setup link, `#s1.`, opens its step, before first run on a fresh install; You're in opens on `Events.let_in` and from its notification). |
| `InviteLinks.kt` | Tells invite links (`#v1.`) from first-phone setup links (`#s1.`) in each form they reach the app: https `/j`, `pingers://join`, and the landing page's `invite` extra. |
| `data/SetupLinkFlow.kt` | The setup link step's states: the core checks the link (sending nothing), "Set up" uses it, "Try again" only when tapped. |
| `ui/screens/Onboarding.kt` | First run, and the first-phone setup link's step in its layout. Their words come from the core. |
| `ui/screens/Join.kt` | Join flow A: Open invite, Asked to join, You're in, and "Ben asked to join" on the inviter's phone. Their words all come from the core. |
| `CoPresenceNotifier.kt` | The co-presence alert as a real notification with "Open meetup" and "Hide me here". |
| `CoreInfo.kt` | The calls into the Rust core so far (`coreVersion`, `currentBucket`), for the Security screen. |
| `Heading.kt` | Compass heading from the rotation-vector sensor. |
| `Words.kt` | Joins strings from `strings.xml` (name lists, directions, areas). |
| `ui/theme`, `ui/components` | Canvas colours, Space Grotesk and Inter type, the canvas's SVG icons, shared rows, cards and buttons. |
| `ui/map/StylisedMap.kt` | `MapBox`: the real map when a pack covers the frame, else the canvas's placeholder (land, water, park, roads). Overlays use map units. |
| `ui/map/PackMap.kt` | MapLibre Native over an installed PMTiles pack; the camera worked out from the screen's `MapFrame`. |
| `maps/` | Region packs: the map host chain, catalogs, resumable verified downloads, installed packs, place search. |
| `ui/screens/MapRegions.kt` | The region picker and the Maps rows for Advanced and What your server sees. |
| `ui/screens/` | One composable per screen. Each gets its state, the repository and the navigator, and holds no logic. |

All copy is in `res/values/strings.xml`, except the join flow's, New party's and Advanced's,
and every screen of the canvas's section 4 (first run, Settings, Your name, Quiet hours,
Zone times, Downloaded maps, Wake-ups, Home's notice and wake-ups lines, Privacy, Party
settings, Safety codes, Leave party, "This server needs a code", the setup link step), which
the core supplies (pingers-words through the FFI views). Those section-4 screens take the FFI
records as they are (`uniffi.pingers.*View`) rather than a `UiState` copy. Fonts are in
`res/font` (both SIL OFL).

**Reaching every screen.** Most screens link as the canvas does. Some screens only open
from outside the app: an invite link (`pingers://join#...`), a push, or a request from
another member. For those, debug builds keep a "Preview events" menu behind a long press
on Home's cog (a tap opens Settings): open an invite link (Open invite), co-presence alert
(posts the real notification too), location request, and meetup ended. Asked to join
follows "Ask to join"; You're in opens when the core raises `Events.let_in` (with a
notification that opens it) and "Ben asked to join" from an opened invite on Invite people.
Debug builds also take `--es screen <Route>` on the launch intent (`OpenInvite`,
`AskedToJoin`, `LetIn`, `JoinRequest` with `--ez fake true`):

```sh
adb shell am start -n org.pingers.app.debug/org.pingers.app.MainActivity --es screen Compass
```

The section-4 routes are `Setup`, `Settings`, `YourName`, `QuietHours`, `ZoneTimes`,
`MapRegions` (Downloaded maps), `Wakeups`, `PartySettings`, `SafetyCodes`, `LeaveParty`
(the sheet over Party settings), `PartyCode` and `SetupLink` (the first-phone setup link's
step), plus `Home` and `Privacy`. With `--ez fake true` they show the canvas's example (Ben's
phone), and debug builds also take `--es variant <names>` (comma-separated) to pick a state:

| Variant | Shows |
|---|---|
| `shared` | Party settings in reduced privacy (PartySettingsShared) |
| `refused` | "This server needs a code" after a refused code (PartyCodeRefused) |
| `error` | Quiet hours with equal times (QuietHoursError) |
| `off` | Wake-ups off by choice (WakeupsOn, switch on); Home's grey line (WakeupsOffHome) |
| `nothing` | Nothing can wake Pingers (Wakeups); Home's orange banner (WakeupsHome) |
| `delayed` | Android holds back wake-ups (WakeupsDelayed); Home's banner for it |
| `notice=clock`, `relay`, `hiding`, `dnd`, `knock`, `busy` | That notice at the top of Home (Notices) |
| `pending` | Home's "Waiting to be let in" row (HomePendingJoin) |
| `noname` | Open invite before a name is set, with the name field (JoinA1) |
| `background`, `notifications` | First run's step 2 or 3 (`--es screen Setup`, real or fake) |
| `setup=working`, `done`, `already`, `refused`, `keys`, `server-keys`, `offline`, `broken` | The first-phone setup link's step in that state (`--es screen SetupLink`, fake only); without it, the question, whose "Set up" ends set up after a moment (or offline with `setup=offline`) |

```sh
adb shell am start -n org.pingers.app.debug/org.pingers.app.MainActivity --ez fake true --es screen PartySettings --es variant shared
adb shell am start -n org.pingers.app.debug/org.pingers.app.MainActivity --ez fake true --es screen Home --es variant nothing,notice=clock
```

Without `fake` the same routes open on the real core; `variant` then only picks first run's
step. Downloaded maps always shows this phone's real maps and the map host's catalogue.

A first-phone setup link (what the appliance's setup page shows as a QR code) opens its step on
the real core, in any of its forms; on a fresh install the step comes before first run:

```sh
adb shell "am start -a android.intent.action.VIEW -d 'https://10.0.2.2:9443/j#s1.<payload>' org.pingers.app.debug"
adb shell "am start -a android.intent.action.VIEW -d 'pingers://join' --es invite 's1.<payload>' org.pingers.app.debug"
```

Screenshots of every screen from a Pixel 9 Pro emulator are in `docs/screenshots/`.

## Where the Rust core plugs in

1. **Build.** Done: see "Build" above.
2. **Repository.** Done: `data/CoreRepository.kt` opens `Core` (one per process, in
   `CoreRepositories`), takes a fresh `Snapshot` on every event, and maps each view model
   with `data/CoreMapping.kt`. Drafts (New party, Spot editor, meet point, an answer) are
   sent to Rust with each read, so pingers-screens decides the `can*` fields. Draw area
   uses pingers-screens' polygon model through `AreaEditor`.
3. **Events.** Done: `CoreRepository.events` routes `meetup_started` (with the
   notification), `location_request` and `meetup_ended` in `MainActivity`.
4. **Platform.** Done: `data/FfiPlatform.kt` implements `uniffi.pingers.Platform` over
   the platform package's `AndroidPlatform` (Keystore secure store, location, geofences,
   DND, WorkManager, push). `data/CoreHost.kt` holds the process's one `Core`, opened on
   first use by the UI or by a receiver, service or worker; `PingersApp` points
   `PlatformHost.sinkProvider` at it, so geofences, pushes, wakes, fixes and DND changes
   reach the core. It creates the database key in the secure store on first run. Logcat
   tag `PingersCore` marks each entry point and its outcome (no coordinates, ids or
   tokens). The compass heading (`Heading.kt`) stays in the shell.
5. **Client.** `Core.open` answers `Unavailable` until the L3 crates behind
   pingers-client (parties, sync, wake, meetup-session) merge; until then every entry
   point logs `core not open`.
6. **Words.** Text that pingers-words generates (the "What your server sees" lists, the
   pause reason, zone schedules) is already plain strings in the state. `Words.kt` and the
   format strings can move to pingers-words once it covers them.
7. **Maps.** Done: MapLibre Native draws installed region packs (`ui/map/PackMap.kt`), the
   placeholder stays as the fallback with a "Download map for this area" prompt, and `maps/`
   holds the region picker's data, downloads and on-device place search. See
   [docs/maps.md](docs/maps.md).
