# ADR-0001: Keep `imageopt-core` Frontend-Agnostic

## Status

Accepted

## Context

`imageopt` is primarily used in two ways:

- Developers run the CLI locally against files in their repository.
- Other projects run the composite GitHub Action in CI to check or optimize
  images.

The current workspace separates the reusable engine from the CLI:

- `crates/core` owns image detection, codec dispatch, candidate generation,
  candidate validation, result selection, and crash-safe file writes.
- `crates/cli` owns argument parsing, path expansion, progress display,
  reporting, and exit codes.

`imageopt-core` deliberately avoids CLI, async, HTTP, and hosted-service
dependencies so future frontends can reuse the engine without changing it.

## Decision

Preserve the current crate boundary and keep `imageopt-core`
frontend-agnostic.

File discovery, shell UX, progress bars, report formatting, and process exit
behavior should remain outside the core crate. Format detection, codec
dispatch, optimization policy, validation, and safe write behavior should
remain inside the core crate.

## Alternatives Considered

- Move more behavior into the CLI. This would simplify some short-term wiring
  but make future server, desktop, or library reuse harder.
- Add a server crate now. This is possible later, but premature without an
  explicit hosted-service requirement.
- Collapse to a single crate. This would reduce workspace complexity but blur
  product concerns and make testing boundaries less clear.

## Consequences

Positive:

- The engine remains reusable by the CLI, GitHub Action, tests, and possible
  future frontends.
- Codec and engine behavior can be tested without shell or CLI concerns.
- The product avoids unnecessary hosted-service operational complexity.

Negative:

- CLI behavior needs dedicated tests because the engine does not own path
  expansion, reporting, or exit-code semantics.
- Some policy decisions need careful placement so they do not leak frontend
  assumptions into the engine.

## Trade-Offs

Maintainability and reuse are prioritized over having fewer crates. The current
split is appropriate for a local/CI tool distributed as a single binary.
