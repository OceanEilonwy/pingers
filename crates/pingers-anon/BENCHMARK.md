# RLN proving benchmark (design H.1 blocker 5)

`examples/prove_bench.rs` builds a 10,000-leaf membership tree, proves N times for a
424-byte board sync body, verifies N times, and prints wall times and the process's peak
resident memory (`VmHWM` from `/proc/self/status`, which Android has too). It uses the same
`prove` and `verify` the app and server call, with `rln`'s default features off, so proving
is single-threaded.

## On this machine

```sh
CARGO_BUILD_JOBS=4 cargo build --release -p pingers-anon --features prover,verifier --example prove_bench
./target/release/examples/prove_bench 30
```

2026-11-13, AMD Ryzen 9 5950X, Linux, release build, while eight other builds ran (load
average 80 to 90 on 32 threads), so these are upper bounds:

| Measure | Result |
|---|---|
| Proof, wall, median of 30 | 1.0 s (p95 1.5 s; 1.5 s median in a busier run) |
| Proof, CPU time | about 0.52 s each (17.0 s user for setup + 31 proofs + 30 verifies, minus 1.35 s for setup + 1 proof) |
| First proof (decodes the embedded key) | 0.85 s |
| Verification, median | 3 ms (12 ms in the busier run) |
| Peak RSS, whole process | 15.8 MiB (2.8 MiB at start, 4.9 MiB with the tree, 15.5 MiB after the first proof) |
| Tree of 10,000 leaves, built in one batch | 0.55 s |

Repeat on an idle machine before quoting the time. The memory figure does not depend on load:
the key and witness graph for `RLN(20, 16)` (5,820 constraints) are small, so proving fits in
about 11 MiB above the baseline. That is far below the "tens of MB" design B.4.8 assumed,
which is worth re-checking against the iOS extension budget (blocker 3).

## On an Android emulator

2026-11-14, Pixel 9 Pro AVD (Android 16, x86_64 system image, 4 virtual CPUs), started with
`-gpu host -accel on` (KVM 12; renderer the host AMD RX 6800 XT, not SwiftShader) on the Ryzen 9
5950X above at load average 12 to 20, `--profile android` build for `x86_64-linux-android`, three
runs of `prove_bench 30`:

| Measure | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| Proof, median of 30 | 386 ms | 366 ms | 367 ms |
| Proof, p95 | 486 ms | 400 ms | 423 ms |
| First proof (decodes the key) | 425 ms | 367 ms | 393 ms |
| Verification, median | 1.8 ms | 1.7 ms | 2.2 ms |
| Tree of 10,000 leaves, one batch | 229 ms | 216 ms | 217 ms |
| Self-test (`SelfTestPlan::DESIGN`) | Passed, p95 369 ms, 7.7 s | Passed, p95 377 ms, 7.7 s | Passed, p95 415 ms, 8.1 s |
| Peak RSS | 17.1 MiB | 17.3 MiB | 17.1 MiB |

An emulator runs x86_64 code on the host's cores under KVM, so this is near desktop speed, not a
phone's: it shows the shipped build and the self-test work end to end on Android, not a device
class. Against the 7 s wake budget (pingers-client `WAKE_BUDGET`) a proof is about 6 %; the
engine also proves only when the wake's remaining time covers the self-test's p95 plus the
request (`MIN_REQUEST_TIME`, 2 s), and spends a Privacy Pass token otherwise. In a wake a proof
reads the stored Merkle path and root (no tree rebuild); the tree is rebuilt only by the hourly
refresh in a new process (about 0.2 s here at 10,000 devices).

## On an Android phone

Needs `cargo-ndk`, an NDK (the workspace builds with NDK 27), `adb`, and a phone with USB
debugging on. No root is needed: `/data/local/tmp` is writable and executable for the shell
user.

```sh
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/27.0.12077973
rustup target add aarch64-linux-android   # once

# Same optimisation as the shipped library (`android` profile: LTO, one codegen unit).
CARGO_BUILD_JOBS=4 cargo ndk -t arm64-v8a build --profile android \
    -p pingers-anon --features prover,verifier --example prove_bench

adb push target/aarch64-linux-android/android/examples/prove_bench /data/local/tmp/
adb shell chmod 755 /data/local/tmp/prove_bench

# Plugged in, screen on, battery above 50 %, after a minute idle. Run it three times.
adb shell /data/local/tmp/prove_bench 30
```

Checked 2026-11-13: `cargo ndk -t arm64-v8a build --release ...` produces an aarch64 PIE for
Android 21 (5.3 MB, unstripped); `--release` is a quicker stand-in for `--profile android`
(then the binary is under `target/aarch64-linux-android/release/examples/`). Not yet run on a
phone.

For a 32-bit-only phone use `-t armeabi-v7a` and the `armv7-linux-androideabi` directory.

Record for each phone: model, SoC, Android version, the median and p95 proof time, the first
proof time, verification median and peak RSS. Blocker 5 passes for a device class when the
proof p95 is under 1.5 s.

Notes:

- A shell process runs at default priority with whatever cores the scheduler gives it, much as
  the app's background executor would. To see the big-core best case, pin it:
  `adb shell taskset f0 /data/local/tmp/prove_bench 30` (the mask depends on the SoC's core
  layout; `cat /sys/devices/system/cpu/cpu*/cpufreq/cpuinfo_max_freq` shows which are big).
- Thermal throttling shows up as a rising tail; if p95 is far above the median, rerun after
  the phone cools.
- `rln`'s `parallel` feature (rayon) is off. It may shorten proofs on 8-core phones at the cost
  of all cores for that second; measuring it means temporarily enabling `rln/parallel` in the
  workspace manifest.
