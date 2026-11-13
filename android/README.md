# Pingers for Android

The Android shell: Kotlin, Jetpack Compose and Material 3, min SDK 29. It draws
every screen of the design canvas (`docs/design-canvas`) on **fake data**. The Rust
core is linked (`crates/pingers-ffi`, through UniFFI), but so far it answers only two
calls: the core version and the current time bucket, shown under "Under the hood" on
"How Pingers keeps you private". Networking and push arrive with the client API.

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
| `data/FakeRepository.kt` | The canvas's example data (Climbing crew at Riverside Park) with just enough behaviour for every control. |
| `data/PolygonEditor.kt` | The Draw area editing model (tap to add, close on the first point, drag, midpoints, select and delete, undo, clear), kept out of the composables. |
| `Navigator.kt` | `Route` (every canvas screen) and a plain back stack, as in monokulo-fob. |
| `MainActivity.kt` | Edge-to-edge host, routes, notification and invite-link intents. |
| `CoPresenceNotifier.kt` | The co-presence alert as a real notification with "Open meetup" and "Hide me here". |
| `CoreInfo.kt` | The calls into the Rust core so far (`coreVersion`, `currentBucket`), for the Security screen. |
| `Heading.kt` | Compass heading from the rotation-vector sensor. |
| `Words.kt` | Joins strings from `strings.xml` (name lists, directions, areas). |
| `ui/theme`, `ui/components` | Canvas colours, Space Grotesk and Inter type, the canvas's SVG icons, shared rows, cards and buttons. |
| `ui/map/StylisedMap.kt` | The canvas's placeholder map (land, water, park, roads) drawn with Canvas. Overlays use map units. |
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
2. **Repository.** Write `CoreRepository : PingersRepository`. It opens `Core` with a
   `Platform` and an `Events` implementation, maps each pingers-screens view model to the
   matching `UiState` class, and turns each action into one `Core` call (for example
   `createParty` to `create_party`, `answerLocationRequest` to `answer_location_request`,
   `drawTapMap` to the pingers-screens polygon model). Swap it in for `FakeRepository`
   in `MainActivity`. The composables don't change.
3. **Events.** `Events` callbacks update the flows. `meetup_started` posts through
   `CoPresenceNotifier`, and `location_request` and `meetup_ended` go to their routes.
   Today the Home debug menu raises these by hand (the `simulate*` methods).
4. **Platform.** Implement the shell's jobs from `docs/architecture.md` section 4 here:
   Keystore secure store, Fused Location, geofences, DND, WorkManager wake, push tokens,
   and the foreground service. The compass heading (`Heading.kt`) stays in the shell. The
   core supplies only the bearing (`Core.compass`).
5. **Words.** Text that pingers-words generates (the "What your server sees" lists, the
   pause reason, zone schedules) is already plain strings in the state. `Words.kt` and the
   format strings can move to pingers-words once it covers them.
6. **Maps.** Swap `StylisedMap` for MapLibre. Overlays already take `MapPoint`s, so only
   the unit-to-screen mapping changes.
