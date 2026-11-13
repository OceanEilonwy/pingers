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
