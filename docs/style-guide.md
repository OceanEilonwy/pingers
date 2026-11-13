# Pingers code style guide

Status: draft, 2026-11-13. Applies to every Rust crate in the workspace (`pingers-architecture.md`). Where a rule can be checked by a tool, section 12 gives the setting; a rule that can be enforced is enforced in CI, not left to review.

Exceptions are allowed, but they are written down next to the code: `#[expect(lint, reason = "...")]`, never `#[allow]`. `expect` fails the build when the exception stops being needed, so stale exceptions cannot pile up.

---

## 1. Make wrong states unrepresentable

Use the type system to enforce correctness where possible.

- **Newtypes for every domain quantity and identifier.** `Meters`, `Degrees`, `Bucket`, `Epoch`, `LeafIndex`, `PartyId`, `SpotId`, `MailboxId`, `UnixSeconds`. Two values of different meaning never share a type, so they cannot be swapped by accident.
- **Parse, don't validate.** Constructors check invariants once and return `Result`; afterwards the type guarantees them. `Polygon::new(points) -> Result<Polygon, PolygonError>` guarantees at least three points and no self-intersection, so no function taking a `Polygon` re-checks.
- **Typestate for protocols.** A lease that has not been padded cannot be uploaded; a meetup that has ended cannot send frames. Encode states as types (`Lease<Unpadded>` to `Lease<Padded>`) or as enums whose variants carry only the data valid in that state.
- **No `bool` parameters.** Use a two-variant enum: `Visibility::AskEachTime`, not `ask: bool`. Call sites then read as sentences.
- **No `Option<bool>`.** Use a three-variant enum that names the third case.
- **No sentinel values.** No `-1`, `0` or empty string meaning "none"; use `Option` or an enum.
- **`#[non_exhaustive]` on public enums and structs that may grow**, so adding a variant is not a breaking change for other crates.
- **Secrets are their own types** (section 8).

## 2. Primitives in signatures need a reason

A public function that takes or returns `u32`, `f64`, `String`, `Vec<u8>` and so on must have a reason it is not a domain type. Acceptable reasons:

- It is a wire or byte boundary (`&[u8]` into a decoder in pingers-proto).
- It is the FFI boundary (pingers-ffi converts to domain types immediately).
- It is a genuinely generic helper with no domain meaning (rare; say so in the doc comment).

```rust
// No
pub fn exit_buffer(radius: f64) -> f64;
// Yes
pub fn exit_buffer(radius: Meters) -> Meters;
```

Private helpers may use primitives when the surrounding function has already established the meaning.

## 3. Structs over tuples

- No tuples in public signatures, and no `.0` or `.1` access outside the type's own module.
- Return a named struct: `fn split(..) -> Split { head, tail }`, not `(A, B)`.
- Tuple structs are allowed only for single-field newtypes (`struct Meters(f64)`), and even then the field is private and read through a method.
- Destructuring a local tuple from an iterator (`for (index, item) in ...`) is fine.

## 4. Errors

- **Every fallible operation returns `Result`.** Nothing reports failure through a panic, a log line or a default value.
- **One error enum per crate**, with `thiserror`. Variants name what went wrong in the crate's own terms (`PolygonError::SelfIntersecting`), and carry the context needed to act on it.
- **No `anyhow` or `Box<dyn Error>` in library crates.** They are allowed in binaries (`pingers-server`, `pingers-cli`) at the top level only.
- **Wrap, don't stringify.** Lower-level errors are kept as a `#[source]`, not formatted into a `String`.
- **Errors never contain secrets, coordinates, identifiers or tokens** (section 9).
- **`#[must_use]`** on functions whose result matters even when it is not a `Result` (builders, computed decisions).
- **Document errors.** Every public fallible function has an `# Errors` section.

## 5. No panics in production code

No `panic!`, `unreachable!`, `todo!`, `unimplemented!`, `.unwrap()` or `.expect()` outside tests. The same goes for the hidden panics:

- **Indexing:** use `.get()` instead of `slice[i]`; use `.split_at_checked()`, `.first()`, iterators.
- **Arithmetic:** use `checked_*`, `saturating_*`, or a domain type whose operations are defined. Wrapping arithmetic only where wrapping is the specification (counters, nonces), via `wrapping_*` and a comment.
- **Casts:** no `as` between numeric types; use `TryFrom` / `From`.
- **String slicing by byte index:** not allowed.
- **`debug_assert!`:** allowed for invariants that the types already guarantee, as documentation; never as the only check.

If something truly cannot happen, make the types prove it. If they cannot, return an error variant (`Internal`) rather than panicking: a panic in the iOS notification extension kills the extension and loses the wake.

## 6. Time, randomness and the outside world are injected

- **Time.** No `SystemTime::now()`, `Instant::now()`, `std::thread::sleep`, `tokio::time::sleep` or `tokio::time::Instant::now()` outside the clock implementations. Code takes a `Clock` (wall time, for buckets and expiries) and a `Timer` (monotonic, for timeouts and cadences) from the `pingers-time` crate. Tests use the manual clock from pingers-testkit and advance it explicitly.
- **Randomness.** No `rand::thread_rng()` or `rand::random()`. Functions that need randomness take `&mut impl CryptoRngCore`. Tests pass a seeded RNG, so dummy slots, nonces and padding are reproducible.
- **Environment.** No `std::env::var`, filesystem or network access below layer L2. Binaries read configuration once and pass typed config down.
- Exceptions are granted per call site with `#[expect(clippy::disallowed_methods, reason = "...")]`, for example inside the real `SystemClock`.

## 7. Tests are production code

Every test must add something: it covers a behaviour, edge case or regression that no other test covers. Tests are reviewed with the same care as the code they test.

- **Name the behaviour.** `exit_needs_buffer_beyond_fence`, not `test_presence_3`. If the name needs "and", it is two tests.
- **Ask "what bug would this catch?"** If the answer is "the same one as another test", delete one.
- **Measure it.** `cargo mutants` runs in CI on changed crates. A surviving mutant means a behaviour no test pins down; a test that kills no mutants adds nothing.
- **Deterministic.** Injected clock and seeded RNG; no network; no sleeps; no dependence on test order.
- **No `unwrap` in tests either.** Tests return `Result<(), TestError>` and use `?`, so a failure says what failed. Assertion macros (`assert_eq!`, `assert!(matches!(..))`) are the only panics allowed, and only in tests. Because `panic_in_result_fn` would flag assertions inside `Result`-returning tests, each test module carries one `#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]`.
- **Test vectors** for every derived key, tag and wire format, stored in `vectors/` and shared with any other implementation.
- **Property tests** (`proptest`) for geometry, state machines and encoders (decode of encode is identity).
- **Fuzz targets** (`cargo fuzz`) for every decoder and for chunked OHTTP.
- **Shared fixtures live in pingers-testkit** as builders, not copied between crates.
- **No `#[ignore]`** without a linked issue and a reason in the attribute.
- Coverage is reported but is not a target; mutation results are what count.

---

Additional rules:

## 8. Secrets

- Key material lives in types that wrap `zeroize::Zeroizing` and implement neither `Debug`, `Display`, `Clone` nor `Serialize` unless the design requires it, with a written reason.
- Compare secrets and MACs with `subtle::ConstantTimeEq`, never `==`.
- Only pingers-crypto calls cryptographic primitives. Other crates call its named functions (`derive_spot_key`, `seal_push_token`), and domain-separation labels exist only there, as constants.
- Meetup keys and content have an explicit lifetime: the type that holds them is dropped, and its storage rows deleted, when the meetup ends.

## 9. Privacy in logs and errors

- No coordinates, Party or Spot names, identifiers, tokens, IP addresses or message contents in logs, errors, panics or metrics.
- Values that could leak are wrapped in `Redacted<T>`, whose `Debug` prints only the type name.
- The location type's `Debug` prints `Location(redacted)`.
- Server logs are off by default; when on, they record counts and timings only.

## 10. Async and concurrency

- No tokio, and no async, below layer L2. Protocol crates are synchronous and sans-IO.
- No blocking calls inside async functions; use `spawn_blocking` for SQLCipher and proving.
- No detached `tokio::spawn`. Every spawned task has an owner that holds its handle and cancels it on shutdown.
- Every network call has a timeout from the injected `Timer`.
- Functions used in `select!` document whether they are cancellation-safe.

## 11. API shape, naming and documentation

- **Visibility:** `pub(crate)` by default; `pub` only for the crate's real interface. No glob re-exports.
- **More than three parameters:** use a struct or a builder.
- **No `impl Default`** for types with invariants that a default would violate.
- **Names are full words.** `distance`, not `dist`; `request`, not `req`. Units go in the type, or in the name when a primitive is unavoidable (`timeout_ms` only at the FFI boundary).
- **Every public item has a doc comment** saying what it is for and, where it is not obvious, why. Public fallible functions document `# Errors`.
- **Comments explain why, not what.** No commented-out code. `TODO` only with an issue link.
- **Unsafe:** `#![forbid(unsafe_code)]` in every crate except those listed with a reason (the pingers-ffi zeroing allocator). Each `unsafe` block has a `// SAFETY:` comment.

## 12. Dependencies and layering

- The layer rule from the architecture (a crate depends only on lower layers, no I/O below L2) is checked by `cargo xtask layers` in CI.
- `cargo deny` checks licences, advisories, duplicate versions and banned crates.
- Adding a dependency needs a reason in the pull request: what it does that we should not write, its maintenance and audit status.
- Wire types and their encodings exist only in pingers-proto. Domain types do not derive `Serialize` by default.
- The MSRV (minimum supported Rust version) is pinned in the workspace and raised deliberately.

## 13. Enforcement

Workspace `Cargo.toml`:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"
unused_must_use = "deny"
unreachable_pub = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# Section 5: no panics
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
indexing_slicing = "deny"
string_slice = "deny"
arithmetic_side_effects = "deny"
as_conversions = "deny"
panic_in_result_fn = "deny"
exit = "deny"
# Sections 1, 4, 11
fn_params_excessive_bools = "deny"
missing_errors_doc = "warn"
wildcard_enum_match_arm = "warn"
allow_attributes = "deny"          # use #[expect(.., reason = "..")]
allow_attributes_without_reason = "deny"
# Section 9 and hygiene
dbg_macro = "deny"
print_stdout = "deny"
print_stderr = "deny"
mem_forget = "deny"
```

`clippy.toml`:

```toml
disallowed-methods = [
  { path = "std::time::SystemTime::now", reason = "inject pingers_time::Clock" },
  { path = "std::time::Instant::now", reason = "inject pingers_time::Timer" },
  { path = "std::thread::sleep", reason = "inject pingers_time::Timer" },
  { path = "tokio::time::sleep", reason = "inject pingers_time::Timer" },
  { path = "tokio::time::Instant::now", reason = "inject pingers_time::Timer" },
  { path = "rand::thread_rng", reason = "take &mut impl CryptoRngCore" },
  { path = "rand::random", reason = "take &mut impl CryptoRngCore" },
  { path = "std::env::var", reason = "read config once in the binary" },
]
```

CI runs, on every pull request:
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo xtask layers`
- `cargo deny check`
- `cargo mutants` on changed crates
- the fuzz targets for a short time box

Each crate's `lib.rs` opts in with `[lints] workspace = true` in its manifest.
