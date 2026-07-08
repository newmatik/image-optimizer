# AGENTS.md

## Cursor Cloud specific instructions

`imageopt` is a single Rust workspace (no services, no runtime deps) that builds a
cross-platform image-optimizer CLI. There is nothing to "run" as a server — the
product is the `imageopt` binary. See `README.md` for full usage and `crates/`
for the code (`crates/core` = engine library, `crates/cli` = binary).

### Toolchain / build gotcha
- The codecs `mozjpeg` and `webp` compile bundled C libraries at build time, so a
  C toolchain plus **`nasm`** and **`cmake`** must be present. `cmake` and `clang`
  are already in the base image; `nasm` is installed during environment setup and
  persists in the VM snapshot. If a build ever fails with an assembler/`nasm` error,
  reinstall it with `sudo apt-get install -y nasm`.
- The first build is slow (compiles the C codecs from source); subsequent builds
  are fast thanks to `target/` caching.

### Standard commands (mirror `.github/workflows/ci.yml`)
- Build: `cargo build --all-features`
- Lint: `cargo fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test --all-features`
- Run the CLI: `cargo run -- <PATHS...> [OPTIONS]` or the built binary at
  `target/debug/imageopt` (e.g. `imageopt --dry-run <dir>` to preview savings).

### Notes
- Optimization is **in place by default** (writes are atomic). Use `--dry-run`
  when experimenting so you don't rewrite fixtures; lossless runs are idempotent.
