# ADR-0005: Bound Batch Memory with an Optional In-Flight Byte Budget

## Status

Accepted

## Context

`imageopt` optimizes many files in parallel via rayon
([`engine::optimize_paths`]). Concurrency was previously bounded only by the
thread-pool size (CPU cores, or `--jobs`). Peak memory therefore scales with
`jobs x per-file footprint`, where per-file footprint includes the original
bytes, every candidate encoding, and the fully decoded pixel buffer.

For typical assets this is fine. But a directory containing a few very large
images, processed on a memory-constrained runner (small CI containers), can
decode several large images at once and exhaust memory. `--jobs` can be lowered
to compensate, but that also throttles the common case of many small files and
requires the user to know the failure ahead of time.

## Decision

Add an optional in-flight byte budget to the batch engine.

- `OptimizeOptions::max_in_flight_bytes: Option<u64>` defaults to `None`
  (unbounded), preserving existing behavior.
- When set, `optimize_paths` throttles admission using a byte-counting
  semaphore keyed on each file's on-disk size, so the combined size of files
  processed concurrently stays under the budget.
- A file larger than the whole budget is still admitted alone: acquire requests
  are clamped to the budget so the semaphore can never deadlock.
- The CLI exposes this as `--max-in-flight-mb <MB>`.

On-disk size is used as a proxy for memory cost. It is imperfect (a small
compressed PNG can decode to a large pixel buffer), but it is cheap to obtain
before reading the file and requires no format-specific accounting. Files are
admitted in input order; combined with the existing order-preserving result
collection, output ordering is unchanged.

## Alternatives Considered

- **Only document lowering `--jobs`.** Simple, but couples the small-file
  throughput knob to the large-file memory problem and needs foreknowledge of
  the failure.
- **Budget on decoded pixel bytes.** More accurate, but pixel dimensions are
  not known until after a header parse/decode, which is exactly the work we are
  trying to gate.
- **A global allocator cap / OOM handler.** Turns an over-budget batch into a
  hard failure rather than graceful throttling, and is invasive.
- **Make the budget the default.** Rejected to keep behavior unchanged for
  existing users; opt-in avoids surprising throughput changes.

## Consequences

Positive:

- Large-image batches can be bounded to a predictable memory ceiling without
  serializing the small-file common case.
- The engine concurrency contract is explicit: unbounded by default, throttled
  when a budget is set, never deadlocking on a single oversized file.

Negative:

- The proxy (file size) can under- or over-estimate true memory use.
- Blocking rayon workers on the semaphore parks threads while large files drain;
  this is acceptable for a batch tool but is a behavior change under load.

## Trade-Offs

Predictable memory usage on constrained runners is prioritized over squeezing
maximum parallelism, but only when the user opts in. The default remains
maximum throughput.
