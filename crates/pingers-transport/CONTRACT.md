# pingers-transport contract

Client network I/O (architecture §3, design B.12 and B.4.9). L2: tokio, hyper, rustls.

## API (what pingers-sync, pingers-meetup-session and pingers-client build against)

- `Transport` trait: `request(class, BinaryRequest) -> BinaryResponse`, `stream(BinaryRequest)
  -> Box<dyn DuplexStream>`, `kind()`. `PrivacyClass::{Anonymous, Identified, Stream}`.
- `DuplexStream`: `send(frame)`, `receive() -> Option<frame>` (cancellation-safe; `None` when
  the gateway ended the stream), `close()` (final chunk, waits for the gateway's). Dropping
  it aborts the exchange.
- `Router::new(Routes { anonymous, identified, stream })` implements `Transport` and routes by
  class; it refuses a `Direct` transport for `anonymous` or `stream`. `kind_for(class)` feeds
  "What your server sees".
- `OhttpTransport::new(OhttpConfig::new(relays, pin, TlsRoots), Arc<dyn Timer>, TransportRng)`:
  anonymous one-shots and the live channel. `key_config_hash()`, `repin(KeyPin)`.
  `Relay::new(request_url, key_config_url)`; `KeyPin::{Pinned(KeyConfigHash), FirstUse}`.
  Tuning (`timeouts`, `backoff`, `live`, `max_response_bytes`) are public fields with defaults.
- `DirectTransport::new(&DirectConfig::new(https_origin, TlsRoots), Arc<dyn Timer>)`:
  `Identified` only.
- `TlsRoots::{mozilla, from_der}`; `TokioTimer`; `TransportRng = Box<dyn CryptoRng + Send>`.
- `TransportError`: `NoRelay` (queue or drop, never fall back), `Ohttp(OhttpError::KeyMismatch)`
  (raise `Notice::RelayKeyMismatch`; latched until `repin`), `ClassNotAllowed`, `Timeout`,
  `Status`, `Connection`, `StreamClosed`, `InvalidUrl`, `InvalidRequest`,
  `InvalidCertificate`, `TooLarge`, `BudgetExhausted` (unused here; for callers' budgets).

## Wire conventions shared with pingers-server (live channel)

- The inner request head of a chunked exchange is the caller's `BinaryRequest` (for example
  `POST /v1/groups/{id}/live` with the group auth token); each later chunk is one frame.
- `KEEP_ALIVE_CHUNK` (`[0x00]`) is a transport keep-alive: sent after 20 s without other
  chunks, discarded by the receiver. **The gateway must send one too** at the same cadence:
  the client treats 65 s without any chunk as a dropped exchange and reconnects.
- **The gateway answers a chunked request's head before reading more chunks.** The client
  waits 10 s for the response head of a full-duplex exchange; if it does not come, the relay
  is taken to buffer request bodies and the split channel is used on it from then on.
- Split channel: a chunked request whose request stream ends right after its head (an empty
  final chunk) is a downlink only; each uplink frame is a one-shot OHTTP request with the same
  method, path and headers and the frame as body.
- A client closing cleanly sends an empty final chunk; the gateway answers with its own final
  chunk. A response without a final chunk is a drop (reconnect), not an end.

## Decisions

- **No direct path for anonymous or stream traffic exists in code.** `OhttpTransport` refuses
  `Identified` (its connection must not carry account-named requests); `DirectTransport`
  refuses `Anonymous` and `Stream`; the router refuses a direct route for either. With no
  relay reachable the request is not sent (`NoRelay`).
- **Fail-over only when nothing was sent.** An anonymous request moves to the next relay only
  if no connection to the last could be made; after that a failure is returned, so a request
  is never delivered twice. Timeouts and connection failures make a relay rest
  (`Backoff`: 1 s doubling to 60 s); relays are tried in configured order.
- **Key config** is fetched through the relay (`GET`), decoded, and checked against the pin
  once per transport, then cached. A mismatch is latched: every request and stream fails
  closed without network until `repin`. `FirstUse` adopts the first config's hash.
- **Connections.** One HTTP/2 connection per origin per transport, re-made when closed; the
  live channel and one-shots to the same relay share it. Every task (the connection driver,
  hyper's stream tasks, the live-channel driver) lives in a `JoinSet` or `JoinHandle` owned by
  the connection or the stream handle and is aborted with it; hyper is given an executor that
  spawns into that set.
- **TLS**: TLS 1.3 only, ALPN `h2` required, kx groups X25519MLKEM768, X25519, P-256 in that
  order (aws-lc-rs). `http://` relay URLs mean HTTP/2 prior knowledge without TLS, for a relay
  on loopback or a private tunnel (the reference relay speaks only that); `DirectTransport`
  requires `https://`.
- **Timeouts**: connect 10 s (TCP + TLS + h2), request 30 s (to the whole response), live
  probe 10 s, all on the injected `Timer`.
- **Live channel reconnects** immediately once, then after `Backoff::delay(n)`; up to
  `max_reconnects` (8) failures in a row, then `receive` returns the last error. Frames given
  to `send` while reconnecting are kept (at most 16, oldest dropped) and sent first on the
  new exchange; a frame sent just as an exchange broke may be lost. A chunk that fails to open
  is fatal (`Ohttp`), never retried.
- **Integration tests** compile the reference relay's own `config.rs` and `relay.rs` with
  `#[path]`, because pingers-ohttp-relay is a binary without a library target.
- **Surviving mutants (cargo mutants), both unobservable:** `Io::poll_flush` and
  `Io::poll_shutdown` replaced with `Ok(())`. Over plain TCP both are no-ops; over TLS,
  tokio-rustls writes each record as it is produced, so nothing waits on a flush, and the
  close_notify a shutdown would send changes nothing the peer acts on (HTTP/2 ends streams
  itself). They stay as plain delegations because skipping them would be wrong in general.
