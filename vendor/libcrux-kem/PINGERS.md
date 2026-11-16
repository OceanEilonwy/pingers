# libcrux-kem 0.0.9, vendored and patched for Pingers

Upstream: <https://github.com/cryspen/libcrux>, crate `libcrux-kem` 0.0.9 from crates.io, by
Cryspen. Licensed under Apache-2.0 (`LICENSE-APACHE`); the patch is under the same terms. The
commit that added this directory holds the unmodified upstream files, so `git diff` against it
shows every change. Wired in with `[patch.crates-io]` in the root `Cargo.toml`; excluded from
the workspace, so it keeps upstream's style.

## Why vendored

pingers-mls's `x-wing` feature (design B.8, suite 0x004D) needs `openmls_libcrux_crypto` 0.4.0,
the only OpenMLS provider with X-Wing. It reaches this crate through `hpke-rs-libcrux` 0.7.0,
which pins `libcrux-kem = "=0.0.9"`. 0.0.9 has two advisories, fixed upstream in 0.0.10, which
the pin keeps out of reach:

- **RUSTSEC-2026-0331** (libcrux #1560): `XWingKemDraft06PublicKey::decode` (and the
  X25519MlKem768Draft00 public and private key decoders) slice the input before checking its
  length, so a short X-Wing key from a malicious KeyPackage, leaf or path panics.
- **RUSTSEC-2026-0330** (libcrux #1595): `PublicKey::encapsulate_derand` slices a seed shorter
  than 64 bytes for the hybrid KEMs and panics.

## What changed

Only those two fixes, backported from 0.0.10 into `src/kem.rs` (each marked
`Pingers backport`): the decoders check the exact length (`2400 + 32` private, `1184 + 32`
public) before splitting, and `encapsulate_derand` checks for a 64-byte seed before splitting it.
The version stays 0.0.9 so the `=0.0.9` pin resolves to this copy. 0.0.10's other changes
(`TryCryptoRng` bounds, the `Ss::MlKem512` variant fix for ML-KEM-512 decapsulation, which
Pingers does not use) are not taken. `Cargo.lock`, `.cargo-ok` and `.cargo_vcs_info.json` from
the registry copy were dropped; Apache-2.0 text added as `LICENSE-APACHE` (the crate ships none).

## When to drop it

When `openmls_libcrux_crypto` (through `hpke-rs-libcrux`) releases on `libcrux-kem` 0.0.10 or
later: delete this directory and its `[patch.crates-io]` entry, update the lock file, and check
`cargo deny check` and `cargo audit`.
