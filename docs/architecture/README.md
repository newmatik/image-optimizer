# Architecture Documentation

This directory records architectural decisions and the project roadmap for
`imageopt`. It is intended to make the evolution of the project easy to
rediscover as the CLI, GitHub Action, and reusable engine grow.

## Decisions

- [ADR-0001: Keep `imageopt-core` Frontend-Agnostic](adr/0001-frontend-agnostic-core.md)
- [ADR-0002: Treat GitHub Releases as the Infrastructure Layer](adr/0002-github-releases-as-infrastructure.md)
- [ADR-0003: Make Skipped Semantics Explicit](adr/0003-explicit-skipped-semantics.md)
- [ADR-0004: Treat Repeated-Run Lossy Safety as a Core Invariant](adr/0004-repeated-run-lossy-safety.md)

## Roadmap

- [Architecture Improvement Roadmap](roadmap.md)

## ADR Status Values

- `Proposed`: decision is recommended but not fully implemented.
- `Accepted`: decision has been adopted as project direction.
- `Superseded`: decision has been replaced by a newer ADR.
