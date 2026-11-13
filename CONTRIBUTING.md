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
`cargo xtask layers` and `cargo deny check`. CI also runs `cargo mutants` on changed crates and
the fuzz targets for a short time.

Changing anything listed as fixed in a crate's `CONTRACT.md` changes an interface other crates
build against: say so in the pull request and update the contract in the same change.
