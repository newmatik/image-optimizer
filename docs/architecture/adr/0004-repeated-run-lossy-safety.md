# ADR-0004: Treat Repeated-Run Lossy Safety as a Core Invariant

## Status

Accepted

## Context

`imageopt` is often run repeatedly:

- A developer may run it locally before committing.
- A repository may run it in CI as a check.
- A repository may run it in CI and commit optimized files back.

Lossless optimization is safe to repeat because it only rewrites files when a
smaller valid representation exists without degrading pixels.

Lossy optimization is different. Recompressing a JPEG, WebP, or lossy PNG
again and again for marginal savings can degrade visual quality over time.
This is especially dangerous in CI, where the tool may run automatically on
every push.

The CLI already mitigates this by making lossy mode opt-in and defaulting
`--lossy` to a 10% minimum savings threshold. That behavior lets a lossy pass
converge instead of repeatedly rewriting a file for tiny byte savings.

## Decision

Treat repeated-run lossy safety as a core product invariant.

The project should preserve these rules:

- Optimization is lossless by default.
- Lossy optimization requires explicit opt-in.
- Lossy CLI runs default to a meaningful minimum savings threshold.
- JPEG lossy optimization should skip destructive re-encoding when source
  quantization already appears to be at or below the requested target quality.
- The default lossy threshold should be tested so it does not regress.
- CI documentation should explain that repeated runs are expected to converge.
- Users may intentionally override the threshold, but that should be presented
  as an advanced trade-off.

For GitHub Action examples, prefer `check: true` or lossless optimization by
default. Show lossy CI usage only with clear threshold guidance.

## Alternatives Considered

- Always accept any smaller lossy candidate. This maximizes byte savings but
  risks silent generational degradation.
- Disable lossy mode in CI. This is safe, but too restrictive for teams that
  intentionally optimize content or marketing images.
- Store sidecar metadata to track prior optimization. This could be more
  precise, but it is invasive and unsuitable for arbitrary consumer
  repositories.
- Try to infer whether an image has already been optimized. This is unreliable
  across tools, encoders, metadata stripping, and asset pipelines.

## Consequences

Positive:

- Repeated CI runs converge instead of repeatedly degrading assets.
- Users can trust default behavior in local and CI workflows.
- The project can still support intentional lossy optimization for teams that
  understand the trade-off.

Negative:

- Some marginal file-size wins are intentionally left on the table.
- Users who want maximum compression must opt into lower thresholds.
- Tests need representative lossy fixtures to catch regressions.

## Trade-Offs

Image quality and user trust are prioritized over squeezing every last byte.
For a repo-local and CI-oriented tool, safe repeated behavior matters more than
aggressive marginal recompression.
