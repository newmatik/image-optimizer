# ADR-0002: Treat GitHub Releases as the Infrastructure Layer

## Status

Accepted

## Context

`imageopt` is not a hosted service. Its operational surface is:

- Rust source code in this repository.
- GitHub Actions CI.
- Multi-platform binaries attached to GitHub Releases.
- A composite GitHub Action that downloads and runs the correct binary in
  consumer repositories.

There is currently no Docker image, Cloudflare Worker, Supabase project,
Sentry service, Kubernetes deployment, Terraform configuration, or long-running
infrastructure.

That is a good fit for the product: a repo-local image optimizer used by
developers and CI systems.

## Decision

Treat GitHub Releases and the composite GitHub Action as the infrastructure
layer for the project.

Improve this layer by making releases reproducible and verifiable:

- Build release assets in a matrix.
- Upload matrix outputs as artifacts.
- Publish all artifacts from a single final job.
- Validate that the release tag matches the crate version.
- Generate checksums consistently for every platform.
- Verify checksums in the composite action before executing downloaded
  binaries.
- Consider artifact attestations after checksum verification and single-job
  publishing are in place.

## Alternatives Considered

- Containerize the CLI. This can be useful for some consumers, but it weakens
  the single-binary promise and is not necessary for GitHub Action usage.
- Host an optimization API. This would add upload limits, queueing,
  backpressure, storage, abuse controls, observability, and incident response.
  Those costs are not justified for the current product shape.
- Publish only to crates.io. This would help Rust users but would not replace
  prebuilt binaries for GitHub Actions and non-Rust consumers.

## Consequences

Positive:

- The project keeps a low operational footprint.
- Consumer repositories can use the tool without building codec dependencies.
- Release hardening can focus on GitHub-native mechanisms: artifacts,
  checksums, provenance, and action smoke tests.

Negative:

- There is no central hosted-service observability.
- Release quality depends on CI coverage and artifact verification rather than
  runtime monitoring.
- GitHub availability remains part of the distribution path.

## Trade-Offs

Operational simplicity and consumer convenience are prioritized over hosted
service flexibility. Cloud infrastructure should only be added if a hosted
image optimization product becomes an explicit requirement.
