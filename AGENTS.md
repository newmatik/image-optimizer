# AGENTS.md

Operating manual for work in this repo. The product is the `imageopt` binary
(no server, no runtime services).

## Layout

```
crates/core/          imageopt-core library (engine + codecs)
  src/engine.rs       optimize_bytes / optimize_file / optimize_paths, atomic write, in-flight budget
  src/codecs/         one module per format; only proposes candidates
  src/format.rs       magic-byte detection (not extension)
  src/options.rs      OptimizeOptions, MetadataPolicy, allow_lossy_rebuild()
  tests/engine.rs     codec + I/O + batch invariants
crates/cli/           imageopt binary
  src/args.rs         clap; maps flags → OptimizeOptions (lossy min-savings default 10%)
  src/run.rs          path expansion (files, dirs, globs) + progress bar
  src/report.rs       table, summary, JSON, exit codes
  tests/cli.rs        process-level JSON / --check
action.yml            composite GH Action: download release binary, sha256, run
fuzz/                 separate workspace; target optimize_bytes
docs/architecture/    ADRs + roadmap
.github/workflows/    ci.yml, release.yml, action-smoke.yml
```

`crates/core` must stay frontend-agnostic: no clap, no async, no HTTP. File
discovery, progress, reporting, and exit codes stay in `crates/cli`.

## Build / test / lint

Mirror CI (`.github/workflows/ci.yml`):

```bash
cargo build --all-features
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Run the CLI: `cargo run -- --dry-run <PATHS…>` or `target/debug/imageopt`.

MSRV is `rust-version = "1.82"` in the workspace `Cargo.toml`. Toolchain file
pins `stable` plus clippy/rustfmt.

## Toolchain gotchas

* mozjpeg and libwebp build bundled C. Need a C compiler, **nasm**, and **cmake**.
  * macOS: `brew install nasm cmake`
  * Ubuntu: `sudo apt-get install -y nasm cmake`
  * Windows: `choco install nasm cmake` + MSVC
* First build is slow; `target/` caches the C codecs.
* **Do not set `panic = "abort"`** in release (see `Cargo.toml`). libjpeg
  reports fatal errors via `resume_unwind`; the engine `catch_unwind`s that
  into a per-file `Failed`. Abort would kill the process on a bad JPEG.
* Windows release links the CRT statically (`.cargo/config.toml`) so the
  `.exe` has no redistributable dependency.

## Invariants (do not break)

* Never write a candidate that does not re-decode (`Optimizer::validate`).
* Never replace a file with a larger encoding unless `keep_larger`.
* In-place writes: temp in the same dir → copy permissions → fsync → rename →
  best-effort dir fsync (Unix).
* `--backup` uses `create_new`; an existing `.orig` is left alone. Failed
  backup copies delete the partial `.orig`.
* Lossless default; lossy is opt-in. CLI `--lossy` defaults `--min-savings` to
  10 so CI commit-back loops converge.
* Lossy JPEG/PNG/WebP pixel-rebuilds drop metadata → only when
  `allow_lossy_rebuild()` (lossy **and** `StripAll`).
* Animated GIF / animated WebP: skip, never flatten.
* Unsafe SVG (script, SMIL, event handlers, CSS animation, foreignObject,
  external `use`): skip.
* AVIF: detect, skip, no optimizer yet.
* `optimize_paths` preserves input order. `--max-in-flight-mb` is a byte
  semaphore on on-disk size; a file larger than the budget still runs alone.

## Where the tricky code is

* `crates/core/src/codecs/jpeg.rs` — unsafe mozjpeg-sys jpegtran, RAII destroy
  guards, `C-unwind` `error_exit`. Touch only with a failing test first.
* `crates/core/src/codecs/webp.rs` — RIFF chunk walk (animation flag / ANIM
  chunk; ICC/EXIF/XMP vs metadata policy). Pixel rebuild cannot copy ICC.
* `crates/core/src/codecs/svg.rs` — conservative substring denylist, not a
  full SVG/CSS parser.
* `crates/core/src/engine.rs` — candidate pick, decompression-bomb
  (`max_pixels`, including WebP header probe), `ByteSemaphore`.
* `crates/cli/src/run.rs` — glob walk depth (`**` unbounded; `*.png` is
  depth 1). Directory walks only include known image extensions.
* `action.yml` — inputs via env (no shell interpolation). Downloads a
  **released** binary, not the PR compile. `action-smoke.yml` therefore tests
  the wrapper + latest/pinned release, not unreleased CLI changes. CLI
  changes are covered by `cargo test` in `ci.yml`.

## What not to do

* Do not add gifsicle (non-reentrant CLI entry, unsafe under rayon).
* Do not put path expansion, progress bars, or process exit policy in core.
* Do not treat GitHub Action smoke as proof the current branch’s CLI works.
* Do not “upgrade” `dtolnay/rust-toolchain` in Dependabot for the MSRV job
  (see `.github/dependabot.yml`); that pin is the Rust version, not an action tag.
* Do not remove `RUSTSEC-2026-0192` / `RUSTSEC-2026-0206` ignores in
  `deny.toml` and `.cargo/audit.toml` until usvg drops rustybuzz/fontdb.
  Keep the two files in sync.
* Do not rewrite working codec code for style.

## Tests

* Core: generated fixtures in `crates/core/tests/engine.rs` (no binary
  fixtures in git). Prefer that over checking in images.
* CLI unit tests for path expansion live next to `run.rs`.
* `--dry-run` when experimenting on real files; lossless is idempotent but
  still overwrites on the first win.
* Fuzz: `cargo fuzz run optimize_bytes` from `fuzz/` after `cargo install cargo-fuzz`.

## Releases

Tag `vX.Y.Z` must match workspace `version`. `release.yml` builds the matrix,
checksums, then one publish job. Composite action verifies `*.sha256` before
exec.

License: GPL-3.0-or-later (libimagequant).
