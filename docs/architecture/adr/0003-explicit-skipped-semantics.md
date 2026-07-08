# ADR-0003: Make Skipped Semantics Explicit

## Status

Accepted

## Context

The engine keeps the original file when a codec produces no usable candidate.
That is safe, but it currently makes different situations look the same:

- A file is already optimally encoded.
- A format or feature is intentionally unsupported.
- A codec chooses to skip a file to avoid unsafe transformations.

Examples include animated GIFs and SVGs that contain scripting, animation, or
unsupported content. These are intentionally left untouched, but reporting them
as `AlreadyOptimal` can mislead users and CI systems.

For a tool commonly run as a CI action in other repositories, clear status
semantics matter. Users need to know whether a file was optimal, skipped by
policy, unsupported by this build, or failed.

## Decision

Report intentional non-error skips as `Skipped` with clear reasons instead of
collapsing them into `AlreadyOptimal`.

Cases that should be explicit include:

- Animated GIFs.
- SVGs with unsafe or unsupported constructs.
- Non-UTF-8 SVG inputs such as gzipped `.svgz`.
- Detected formats that are not optimized by the current build.
- AVIF detection until an AVIF optimizer is actually implemented.

The engine and codec API should support this without treating normal skips as
fatal failures.

## Alternatives Considered

- Leave zero candidates as `AlreadyOptimal`. This preserves the current simple
  codec API, but hides important user-facing behavior.
- Treat all unsupported cases as failures. This is too noisy for normal image
  trees and would make CI gates fail for intentionally skipped files.
- Rely on README documentation only. Documentation helps, but machine-readable
  JSON and CLI output still need accurate statuses.

## Consequences

Positive:

- CI output becomes easier to understand and act on.
- JSON consumers can distinguish optimal files from intentionally skipped
  files.
- Documentation and runtime behavior stay aligned.

Negative:

- The codec-to-engine contract may need a small internal change.
- Existing output snapshots or consumers may need to account for more precise
  `skipped` statuses.

## Trade-Offs

User clarity is prioritized over preserving an overly simple internal meaning
for “no candidates.” The engine should still avoid failing normal runs when a
file is intentionally skipped.
