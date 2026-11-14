# Pingers

Pingers tells friends when they are at the same place, and lets them meet up there, without
telling the server who is where or who is with whom. Location never leaves the phone in a form
the server can read; co-presence is matched on the phones against a blind board of random-looking
records, and everything after that runs inside an MLS group made only for the people present.

This repository holds the Rust workspace (protocol, client core, server, push relay and OHTTP
relay) and, later, the Android app.

## Documents

- [`docs/design.md`](docs/design.md): threat model, cryptographic design, data model, API.
- [`docs/architecture.md`](docs/architecture.md): the crates, their layers and their jobs.
- [`docs/style-guide.md`](docs/style-guide.md): binding code rules and how they are enforced.
- [`CONTRIBUTING.md`](CONTRIBUTING.md): what every change must do.
- [`vectors/README.md`](vectors/README.md): the shared test-vector format.
- Each protocol crate has a `CONTRACT.md` saying which parts of its interface are fixed.

## Build and test

Requires Rust 1.98 or later.

```sh
cargo build --workspace
cargo test --workspace --all-features
cargo xtask ci        # everything CI runs: fmt, clippy, tests, layer check, no-python, cargo deny
cargo xtask layers    # just the layer rule
cargo xtask vectors   # regenerate vectors/proto and vectors/meetup
```

`cargo xtask ci` runs `cargo deny check` only if `cargo-deny` is installed
(`cargo install cargo-deny --locked`). Mutation testing uses `cargo-mutants`.

The Android library is built per ABI with `cargo ndk` and the `android` profile
(`--profile android`).

## Licence

AGPL-3.0-only. See [`LICENSE`](LICENSE).
