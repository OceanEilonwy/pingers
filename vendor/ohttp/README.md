# ohttp 0.8.0, vendored and patched for Pingers

Upstream: <https://github.com/martinthomson/ohttp>, crate `ohttp` 0.8.0 from crates.io, by
Martin Thomson. Licensed under either of Apache-2.0 (`LICENSE-APACHE`) or MIT (`LICENSE-MIT`), at
your option; the patches below are under the same terms. The commit that added this directory
holds the unmodified upstream files, so `git diff` against it shows every change.

Only pingers-ohttp depends on this crate. It is excluded from the workspace (root `Cargo.toml`),
so it keeps upstream's code style and is not linted with the Pingers rules.

## Why vendored

Pingers needs three things the published crate cannot give (design B.4.9, H.1 #11;
`crates/pingers-ohttp/CONTRACT.md`):

1. **Injected randomness.** Upstream draws the HPKE ephemeral key, the response nonce and
   generated keys from the thread RNG. Pingers passes `&mut impl CryptoRng` everywhere (style
   guide §6), so tests and test vectors are deterministic.
2. **Chunked OHTTP without I/O.** Upstream's `stream` module is built on `futures`
   `AsyncRead`/`AsyncWrite`. pingers-ohttp is sans-I/O (layer L1).
3. **The two bugs in upstream's `stream` module** reported as ohttp issue #102:
   - a panic with ChaCha20-Poly1305 responses: the response nonce buffer is a fixed 16 bytes,
     but the nonce is `max(Nn, Nk)`, which is 32 bytes for ChaCha20-Poly1305;
   - zero-length non-final chunks are accepted, which draft-ietf-ohai-chunked-ohttp-08 §6
     says a receiver must treat as a decryption error.

## What changed

- **Removed:** the NSS backend (`src/nss`, `build.rs`, `bindings/`), the `stream` module, the
  thread-RNG helper (`src/rand.rs`), the `unsafe-print-secrets` feature, upstream's unit tests
  (they used the old signatures; pingers-ohttp's tests and vectors cover this code, including
  the RFC 9458 and chunked-draft examples), `ClientRequest::from_encoded_config_list`,
  `init()`, and the Cargo features (the crate is always client + server + rust-hpke).
- **Randomness from the caller:** `HpkeS::new`, `generate_key_pair`, `KeyConfig::new`,
  `ClientRequest::encapsulate` and `ServerResponse::encapsulate` take
  `&mut impl rand_core::CryptoRng`. The `hpke`, `aes-gcm` and `chacha20poly1305` dependencies
  are built without their OS-RNG features, so nothing here can reach the system RNG.
  `ServerResponse` now draws its nonce when the response is sealed, not at decapsulation.
- **Added `src/chunked.rs`** (replaces `stream.rs`): sealing and opening of chunks for chunked
  OHTTP with no framing or I/O. It sizes the response nonce as `max(Nn, Nk)` for every AEAD,
  refuses to seal an empty non-final chunk, and returns `Error::EmptyChunk` when an opened
  non-final chunk is empty. Entry points: `ClientRequest::encapsulate_chunked`,
  `Server::decapsulate_chunked`.
- **Added `KeyConfig::import` and `rh::hpke::import_private_key`:** load a stored private key.
- **Panics turned into errors:** `KeyConfig::new` and `KeyConfig::derive` with no supported
  suite, `Server::new` without a private key, a missing private key in
  `decode_request_header`, `ClientRequest::from_config` with no suite, and AEAD
  sequence-number overflow (which upstream would wrap or overflow) now return errors.
- `REQUEST_HEADER_LEN` is public; `Error::ExtraData`, `NotReady` and `WriteAfterClose` (stream
  only) are gone and `Error::EmptyChunk` is new.

## Upstreaming

The two `stream` fixes should go upstream (design H.1 #11). Once a release has them and lets
callers supply randomness, replace this directory with the published crate.
