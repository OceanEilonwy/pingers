# Test vectors

Known-answer tests for every derived key, tag, sealed format and wire encoding (style guide §7),
shared with any other implementation of the Pingers protocol. Rust tests load them with
`pingers_testkit::load_vectors("<name>")`.

## Files

One JSON file per topic, named after it: `vectors/<name>.json`, for example
`presence-key-schedule.json`, `board-beacon-seal.json`, `knock-seal.json`, `record-encoding.json`.
The crate that owns the derivation or format owns its file (mostly pingers-crypto and
pingers-proto). `example.json` shows the format and is used by pingers-testkit's own tests.
A crate with several files may group them in a subdirectory: `vectors/geo/polygon.json` is
loaded as `load_vectors("geo/polygon")`, and its `name` is `geo/polygon`.

## Format

```json
{
  "name": "presence-key-schedule",
  "description": "K_spot, K_tag and tag(b0, r) from design B.4.1",
  "vectors": [
    {
      "name": "first-spot-bucket-zero",
      "inputs": {
        "presence_root": "000102...1f",
        "spot_id": "a0a1...af",
        "bucket": 0,
        "randomness": "b0b1...bf"
      },
      "expected": {
        "spot_key": "…",
        "tag_key": "…",
        "tag": "…"
      }
    }
  ]
}
```

- **`name`**: the topic; must match the file name without `.json`.
- **`description`**: what is covered, with the design section that defines it.
- **`vectors`**: the cases. Each has:
  - **`name`**: unique within the file, descriptive (`empty-segment`, `max-length-payload`).
  - **`inputs`**: every value the computation takes, by field name.
  - **`expected`**: every value it produces, by field name. Include intermediate values
    (for example `spot_key` on the way to `tag`) so a mismatch shows where implementations part.
- **Bytes** are lower-case hex strings with no prefix or separators. Empty bytes are `""`.
- **Integers** (bucket indexes, epochs, leaf indexes, counts) are JSON numbers. Values that may
  exceed 2⁵³ are hex strings of their big-endian encoding instead, and the field name says so
  (`epoch_be64`).
- **Text** (labels, chat bodies) is a JSON string in UTF-8.
- **Randomness**: a computation that takes randomness lists the random bytes it consumes as an
  input (`nonce`, `randomness`), so the vector is deterministic. Vectors never depend on a
  particular RNG algorithm.
- **Failures**: a case that must be rejected has `"expected": { "error": "<kind>" }`, where the
  kind is the error variant name in the owning crate (`OpenFailed`, `WrongLength`).
- Field names are `snake_case` and use full words, matching the Rust parameter names.

Files are formatted with two-space indentation and end with a newline. Unknown top-level or
per-vector keys are rejected by the loader, so typos fail loudly.
