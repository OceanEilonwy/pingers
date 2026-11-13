# Pingers for Android

The Android shell: Kotlin, Jetpack Compose and Material 3, min SDK 29. It draws
every screen of the design canvas (`docs/design-canvas`). The app runs on
`CoreRepository`, over the Rust core (`crates/pingers-ffi`, through UniFFI): each
pingers-screens view model arrives as a record and maps one to one onto `UiState`.
Until pingers-client's API merges, `Core` opens but every other call answers
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
./gradlew assembleDebug assembleRelease lint
./gradlew installDebug
./gradlew assembleRelease -PpingersSplits   # one APK per ABI, plus a universal one
```

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
| `MainActivity.kt` | Edge-to-edge host, routes, notification and invite-link intents. |
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

All copy is in `res/values/strings.xml`. Fonts are in `res/font` (both SIL OFL).

**Reaching every screen.** Most screens link as the canvas does. Some screens only open
from outside the app: an invite link (`pingers://join#...`), a push, or a request from
another member. For those, the gear on Home has a "Preview events" menu: open an invite
link, co-presence alert (posts the real notification too), location request, and meetup
ended. Debug builds also take `--es screen <Route>` on the launch intent:

```sh
adb shell am start -n org.pingers.app.debug/org.pingers.app.MainActivity --es screen Compass
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
4. **Platform.** `data/InterimPlatform.kt` stands in (in-memory secure store, no
   location) until the platform package implements `uniffi.pingers.Platform`; then
   `CoreRepositories` passes that instead. The compass heading (`Heading.kt`) stays in
   the shell.
5. **Client.** `Core`'s calls answer `Unavailable` until pingers-client's async API
   merges; each then becomes one client call in `crates/pingers-ffi/src/core.rs`.
6. **Words.** Text that pingers-words generates (the "What your server sees" lists, the
   pause reason, zone schedules) is already plain strings in the state. `Words.kt` and the
   format strings can move to pingers-words once it covers them.
7. **Maps.** Done: MapLibre Native draws installed region packs (`ui/map/PackMap.kt`), the
   placeholder stays as the fallback with a "Download map for this area" prompt, and `maps/`
   holds the region picker's data, downloads and on-device place search. See
   [docs/maps.md](docs/maps.md).
