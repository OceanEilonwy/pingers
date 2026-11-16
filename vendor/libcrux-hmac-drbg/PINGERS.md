# libcrux-hmac-drbg 0.0.1, vendored and patched for Pingers

Upstream: <https://github.com/cryspen/libcrux>, crate `libcrux-hmac-drbg` 0.0.1 from crates.io,
by Cryspen. Licensed under Apache-2.0 (`LICENSE-APACHE`); the patch is under the same terms. The
commit that added this directory holds the unmodified upstream files, so `git diff` against it
shows every change. Wired in with `[patch.crates-io]` in the root `Cargo.toml`; excluded from
the workspace, so it keeps upstream's style.

## Why vendored

pingers-mls's `x-wing` feature (design B.8, suite 0x004D) needs `openmls_libcrux_crypto` 0.4.0,
the only OpenMLS provider with X-Wing, and it pins `libcrux-hmac-drbg = "0.0.1"` (an exact pin
for a 0.0.x version). 0.0.1 has one advisory, fixed upstream in 0.0.2:

- **RUSTSEC-2026-0329** (libcrux #1558): the auto-reseeding `HmacDrbgRng::fill_bytes` hands an
  empty remainder to `generate` when the length is a non-zero multiple of 65,536; `generate`
  refuses empty output and the wrapper turns that into `unreachable!`, a panic.

## What changed

Only that fix, backported from 0.0.2 (marked `Pingers backport`): `generate` accepts an empty
output, and the wrapper's `try_fill_bytes` iterates `chunks_mut(MAX_GENERATE_BYTES)`, which yields
no empty chunk. The error variant keeps its 0.0.1 name `RequestInvalid` (0.0.2 renamed it), so
the API the pinned dependant compiles against is unchanged; its doc and the crate's own test for
zero-byte requests follow the new behaviour. The version stays 0.0.1 so the pin resolves to this
copy. 0.0.2's other changes (the `sys_rng` feature split, testing APIs) are not taken.
`Cargo.lock`, `.cargo-ok` and `.cargo_vcs_info.json` from the registry copy were dropped;
Apache-2.0 text added as `LICENSE-APACHE` (the crate ships none).

## When to drop it

When `openmls_libcrux_crypto` releases on `libcrux-hmac-drbg` 0.0.2 or later: delete this
directory and its `[patch.crates-io]` entry, update the lock file, and check `cargo deny check`
and `cargo audit`.
