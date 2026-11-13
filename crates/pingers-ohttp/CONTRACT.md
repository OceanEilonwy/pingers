# pingers-ohttp contract

Sans-IO OHTTP (RFC 9458) and chunked OHTTP (draft-ietf-ohai-chunked-ohttp-08), client and
gateway. HPKE suite fixed: X25519 / HKDF-SHA256 / AES-128-GCM.

## Fixed (other crates build against these)

- Key configs: `KeyConfig::{decode, key_id, encoded, hash}`, `check_pin(&KeyConfig,
  &KeyConfigHash)` (implemented), `GatewayKey::{from_bytes, generate, config}`.
- Binary HTTP: `BinaryRequest { method, path, headers, body }`, `BinaryResponse { status,
  headers, body }`, `Header`, `StatusCode`, each with `encode`/`decode`.
- One-shot: `encapsulate_request(&KeyConfig, &BinaryRequest, rng) -> EncapsulatedRequest
  { message, response_context }`, `ResponseContext::open(self, bytes)`;
  `Gateway::new(keys)`, `Gateway::decapsulate(bytes) -> DecapsulatedRequest { request,
  responder }`, `Responder::seal(self, &BinaryResponse, rng)`.
- Chunked: `ChunkedClient::start(&KeyConfig, &BinaryRequest, rng) -> ChunkedClientStart
  { preamble, client }`, `ChunkedClient::{seal_chunk, response_decoder}`,
  `ResponseChunkDecoder::{feed, finish}`; `ChunkedGateway::{accept, request, request_decoder,
  response_preamble, seal_chunk}`, `RequestChunkDecoder::{feed, finish}`; `ChunkPosition`,
  `OpenedChunk`. Decoders accept bytes in any split and return whole chunks.
- `OhttpError` variants, including `EmptyNonFinalChunk`, `Truncated`, `KeyMismatch`.

## Implementer decides

- Whether to use the `ohttp` crate's `stream` module (patched for the zero-length check and the
  ChaCha20 panic, design B.4.9 and H.1 #11) or a vendored copy; record which here.
- How randomness reaches the HPKE layer. The contract takes `&mut impl CryptoRng`; if the
  `ohttp` crate cannot accept an injected RNG, vendor or patch it rather than drop the
  parameter (style guide §6).
- Fuzz targets for every decoder and for the chunk framing (required).

## Decisions

- **Binary HTTP types live here, not in pingers-proto**: they are RFC 9292 framing for this
  layer, not Pingers wire formats; the CBOR bodies inside them are pingers-proto's.
- **Key config hashes come from pingers-crypto** (`key_config_hash`), so only pingers-crypto
  calls SHA-256.
- **A gateway holds several keys** so key rotation can overlap.
- **`ohttp` is vendored and patched** in `vendor/ohttp` (README there lists every change):
  randomness is injected everywhere, and upstream's futures-based `stream` module is replaced
  by sans-I/O chunk sealing and opening that sizes the response nonce as `max(Nn, Nk)` (the
  ChaCha20 panic) and rejects empty non-final chunks (draft-08 §6), both from ohttp issue #102.
  The `hpke` dependency is built without `getrandom`, so nothing can reach the OS RNG.
  Chunk framing (varints, any split, size limits) is in this crate, `src/framing.rs`.
- **Contract change, `ChunkedGateway::accept(gateway, preamble, rng)`** (was without `rng`) and
  **`ChunkedGateway::seal_chunk(payload, position)`** (was with `rng`): `response_preamble()`
  takes no RNG and returns the response nonce, so the nonce has to exist by then; drawing it
  in `accept` is the only place with an RNG before the preamble is needed. Sealing a chunk
  needs no randomness, so the parameter went.
- **Inner content of a chunked exchange**: the first request chunk (inside `preamble`) is the
  known-length Binary HTTP request head; every later chunk in either direction is one opaque
  frame. The outer format is exactly draft-08's, so relays need nothing special.
- **`accept` returns `Truncated`** when `preamble` ends before the head chunk does; the caller
  reads more and calls again. Bytes after the head chunk are kept and returned by the first
  `RequestChunkDecoder::feed` (call it, even with no bytes, before `finish`).
- **`request_decoder(&self)` works once**: there is one request stream with one HPKE sequence,
  so the first call takes it (a `Cell` inside, which makes `ChunkedGateway` `Send` but not
  `Sync`); a later decoder fails with `OpenFailed`.
- **Decoders are poisoned by an error**: after any error, `feed` and `finish` return
  `OpenFailed`, since the AEAD sequence can no longer be trusted.
- **`finish` returns `Some`** on success: the final chunk always exists (it may be empty).
- **New variant `OhttpError::ChunkTooLarge`** (the enum is `non_exhaustive`): chunks are capped
  at upstream's 16 KiB of plaintext so a peer cannot make a decoder buffer without bound; a
  length prefix above the cap is refused before buffering. `NotImplemented` (the stub marker)
  is gone with the stubs.
- **`TooManyChunks`** is reported when HPKE or the AEAD sequence runs out (2^64 chunks); the
  vendored crate errors instead of wrapping.
- **Key config decoding** accepts configs that list other suites as long as one is
  HKDF-SHA256 / AES-128-GCM, keeps the bytes exactly as served (so the pin matches), and
  rejects trailing bytes, other KEMs, and empty or misaligned suite lists. Requests always use
  AES-128-GCM; the gateway refuses any other suite with `UnsupportedSuite`. Configs served by
  `GatewayKey::config` offer only that suite.
- **Binary HTTP encoding** is known-length with trailing empty sections omitted (RFC 9292
  §3.8), scheme `https` and no authority; decoding also accepts the indeterminate-length form
  and zero padding, and refuses trailers, informational responses, upper-case or empty header
  names, an empty method and a path not starting with `/`.
- **Test vectors**: `vectors/ohttp/encapsulation.json` and `vectors/ohttp/chunked.json`
  (load as `ohttp/encapsulation`, `ohttp/chunked`). The gateway side also reproduces RFC 9458
  Appendix A and draft-08 Appendix A byte for byte (`tests/rfc_examples.rs`,
  `src/chunked/tests.rs`); their client sides give an ephemeral secret key, while HPKE here
  derives it from 32 random bytes, so they cannot be replayed.
- **Fuzz targets** live in `fuzz/` (own workspace, nightly `cargo fuzz`).
