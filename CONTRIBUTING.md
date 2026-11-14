# Contributing

## The rules

[`docs/style-guide.md`](docs/style-guide.md) is binding. Read it before writing code. The short
version: newtypes for every domain value, no panics (including indexing, arithmetic and casts),
no `bool` parameters, no tuples in public signatures, injected time and randomness, secrets in
their own types, nothing identifying in logs or errors, and a doc comment on every public item.

Exceptions are written next to the code as `#[expect(lint, reason = "...")]`, never `#[allow]`.

Test modules carry `#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests
fail")]` only when at least one `Result`-returning test in them asserts: `expect` fails the
build when the lint would not have fired, so a module whose tests all return `()` must leave it
out.

## Every crate

- Lives under `crates/` and declares its layer in its manifest:

  ```toml
  [package.metadata.pingers]
  layer = 1        # 0 to 5 as in docs/architecture.md, or "dev"
  ```

  `cargo xtask layers` fails if a crate depends on a crate in the same or a higher layer, or if
  a crate in L0 or L1 depends on `tokio`, `hyper`, `reqwest` or `rusqlite`.

- Opts into the workspace lints, and takes shared fields from the workspace:

  ```toml
  [package]
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true
  publish.workspace = true

  [lints]
  workspace = true
  ```

- Uses dependency versions from `[workspace.dependencies]` (`name = { workspace = true }`).
  A new dependency needs a reason in the pull request.

- Starts its `lib.rs` (or `main.rs`) with the crate's single job, as in the architecture:

  ```rust
  // SPDX-License-Identifier: AGPL-3.0-only
  //! <the crate's job, one line>
  #![forbid(unsafe_code)]
  ```

## Every source file

The first line of every `.rs` file is the SPDX header:

```rust
// SPDX-License-Identifier: AGPL-3.0-only
```

## Every pull request

Run `cargo xtask ci` before pushing. It runs, in order: `cargo fmt --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test`,
`cargo xtask layers`, `cargo xtask no-python` and `cargo deny check`. It stays quick enough to
run on every change.

Before asking for review, run `cargo xtask ci --full`. It runs everything `ci` does, then:

- `cargo mutants` on every crate with a file changed since `main` (the merge base, working
  tree included). Each surviving or timed-out mutant fails the run unless
  [`mutants-allow.toml`](mutants-allow.toml) lists it with a reason, normally that it is
  equivalent. An entry that no longer survives in a crate that was mutated also fails, so the
  file cannot go stale. Kill a new survivor with a test rather than listing it.
- every fuzz target under `crates/*/fuzz` for 10 seconds, with `cargo +nightly fuzz run`.

Options: `--base <ref>` measures changes against another branch or commit, and
`--fuzz-seconds <n>` changes the time box. It needs `cargo-mutants`, `cargo-fuzz` and a nightly
toolchain. Mutating a large crate takes a while; results land in `target/xtask-mutants/`.

## Tools and test vectors

Tools are written in Rust, as `cargo xtask` commands; there is no Python in the repository, and
`cargo xtask no-python` (a `ci` step) fails on any `.py` file or Python shebang.

`vectors/proto/*.json` and `vectors/meetup/*.json` are generated: edit the generator in
`xtask/src/vectors/` and run `cargo xtask vectors`, never the JSON by hand. The generator uses
its own CBOR writer and formulas, not pingers-proto or pingers-meetup, so each vector is
computed twice. `cargo test` (and `cargo xtask vectors --check`) fails if a committed file
differs from what the generator writes. The other vector directories are written by hand; see
[`vectors/README.md`](vectors/README.md).

Changing anything listed as fixed in a crate's `CONTRACT.md` changes an interface other crates
build against: say so in the pull request and update the contract in the same change.
