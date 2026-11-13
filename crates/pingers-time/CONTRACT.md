# pingers-time contract

Implemented in full. Other crates build against everything below.

## Fixed

- `UnixSeconds` (whole seconds since the epoch): `new`, `get`, `bucket`, `checked_add`,
  `checked_sub`, `saturating_duration_since`, `EPOCH`.
- `Bucket` (900 s): `LENGTH`, `containing`, `from_index` (rejects indexes whose start would
  overflow), `index`, `to_be_bytes` (the `be64(b)` of the derivations), `start`, `end`,
  `contains`, `next`, `previous`, `checked_add`, `checked_sub`, `saturating_buckets_since`.
- `BucketCount`: a span of buckets (`ZERO`, `ONE`, `new`, `get`).
- `trait Clock: Send + Sync { fn now(&self) -> UnixSeconds; }`, implemented for `&C` and
  `Arc<C>`.
- `trait Timer: Send + Sync { fn now(&self) -> MonotonicTime; fn sleep_until(&self, deadline)
  -> Sleep<'_>; fn sleep(&self, duration) -> Sleep<'_>; }`, implemented for `&T` and `Arc<T>`.
  `Sleep<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>`; dropping it cancels the sleep.
- `MonotonicTime`: elapsed time since the timer's origin.
- `SystemClock` (the one production reader of the wall clock; a pre-1970 clock reads as the
  epoch, and `try_now` reports it).
- `ManualClock` behind feature `test-util`: `new`, `advance` (moves wall and monotonic time,
  wakes due sleepers), `set_wall` (moves wall time only, for skew tests). Clones share one clock.
- `TimeError`: `BucketOutOfRange`, `BeforeEpoch`.

## Decisions

- **No production `Timer` here.** A real timer must sleep, which needs an async runtime, and
  L0 may not depend on tokio. The tokio-backed timer lives in the lowest crate that has tokio
  (pingers-transport, as `TokioTimer`); server binaries use the same type.
- **pingers-time is L0 and other L0 crates depend on it.** The architecture places it in L0
  beside pingers-proto, pingers-geo and pingers-crypto, which all need `Bucket` or
  `UnixSeconds`. These same-layer dependencies are listed with reasons under
  `[workspace.metadata.pingers] same-layer` in the workspace manifest, and
  `cargo xtask layers` allows only those.
- Wall time is in whole seconds; `ManualClock` keeps sub-second precision internally so small
  advances add up.
