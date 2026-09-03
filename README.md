# imageopt

Cross-platform image optimizer (JPEG, PNG, WebP, SVG, static GIF). One
self-contained binary, no runtime codec tools. Lossless by default; writes
atomically and never replaces a file with a larger or undecodable result.

Use it locally or as a CI gate / commit-back step. A composite GitHub Action
downloads the matching release binary.

## Quickstart

Build requires a Rust toolchain (MSRV **1.82**), a C compiler, **nasm**, and
**cmake** (mozjpeg and libwebp compile from source):

```bash
# macOS:    brew install nasm cmake
# Ubuntu:   sudo apt-get install -y nasm cmake
# Windows:  choco install nasm cmake   (MSVC toolchain)

cargo build --release
./target/release/imageopt --dry-run assets/
```

Or install the CLI: `cargo install --path crates/cli`

Prebuilt binaries: [Releases](https://github.com/newmatik/image-optimizer/releases)
(macOS arm64/x64, Linux x64/arm64, Windows x64).

## Usage

```bash
imageopt [PATHS...] [OPTIONS]
```

`PATHS` are files, directories, or globs (quote globs so the tool expands them).

```bash
imageopt logo.png photo.jpg          # in-place
imageopt assets/                     # image files in that directory
imageopt -r assets/                  # …and subdirectories
imageopt "src/**/*.{png,jpg}"        # recursive glob
imageopt --dry-run assets/           # preview, write nothing
imageopt --check assets/             # CI: exit 1 if anything could shrink
imageopt --lossy -q 75 photos/       # lossy, quality 75
imageopt --backup logo.png           # keep logo.png.orig
imageopt --json assets/ > report.json
```

In-place is the default. `--dry-run` / `--check` write nothing.

| Flag | Description |
|------|-------------|
| `-r, --recursive` | Recurse into subdirectories (directory args only; use `**` in globs). |
| `--lossy` | Allow lossy recompression. |
| `-q, --quality <1-100>` | Lossy quality (implies `--lossy`). |
| `--png-level <0-6>` | oxipng effort; 6 enables Zopfli. Default 3. |
| `--min-savings <PERCENT>` | Rewrite only if savings ≥ this. Default 0 (lossless) / 10 (`--lossy`). |
| `--strip <all\|color\|none>` | Metadata: strip all, keep ICC, or keep everything. Default: keep ICC — with `--lossy`, default becomes `all`. |
| `--dry-run` | Report only. |
| `--backup` | Copy original to `<name>.orig` once (never clobber an existing backup). |
| `--check` | Write nothing; exit 1 if any file could be optimized or failed. |
| `--json` | Machine-readable report (stdout). |
| `-j, --jobs <N>` | Parallel workers (default: CPU cores). |
| `--max-in-flight-mb <MB>` | Cap combined size of files decoded at once. Default: unbounded. |
| `--keep-larger` | Keep a re-encode even if larger. |
| `--quiet` | Summary only. |

Directory walks only pick known image extensions. Pass a file or glob to
optimize an extensionless / oddly named image. Shallow globs (`*.png`) do not
walk nested directories; use `**` for that.

### Idempotency

**Lossless** is deterministic: a candidate is written only if it is strictly
smaller, so a second run reports `already optimal`.

**Lossy** can shave a sliver every run. `--lossy` therefore defaults
`--min-savings` to **10%**, so a commit-back loop converges after one pass.
JPEG lossy also skips a destructive re-encode when the source quantization
already looks at or below the requested quality. Pass `--min-savings 0` only
if you are not re-running automatically.

### Exit codes

* `0` — success (`--check`: nothing to do).
* `1` — `--check` and at least one file could be optimized or failed.
* `2` — no matching input files.

Failed files are left untouched. Without `--check`, failures are reported but
the process still exits 0.

### JSON

`--json` emits `summary` plus per-file `results`. `status` is `optimized`,
`already_optimal`, `skipped`, or `failed`. For `skipped` and `failed`, the
message is in `error`. Skipped files do not fail `--check`; failed files do.

```json
{
  "summary": {
    "total": 1,
    "optimized": 1,
    "already_optimal": 0,
    "skipped": 0,
    "failed": 0,
    "original_size": 1024,
    "optimized_size": 768,
    "saved_bytes": 256,
    "saved_percent": 25.0,
    "elapsed_ms": 12,
    "formats": { "png": 1 }
  },
  "results": [
    {
      "file": "assets/logo.png",
      "format": "png",
      "status": "optimized",
      "error": null,
      "original_size": 1024,
      "optimized_size": 768,
      "saved_bytes": 256,
      "saved_percent": 25.0,
      "elapsed_ms": 12
    }
  ]
}
```

`summary.elapsed_ms` is the sum of per-file times, not wall-clock.

## Formats

| Format | Lossless (default) | Lossy (`--lossy`) | Engine |
|--------|:---:|:---:|--------|
| JPEG | DCT coefficient re-write (progressive, optimized Huffman) | re-encode at quality | mozjpeg |
| PNG  | IDAT recompress + reductions (Zopfli at `--png-level 6`) | palette quantization | oxipng + libimagequant |
| WebP | lossless re-encode when it cannot drop kept metadata | re-encode at quality | libwebp |
| SVG  | normalize + minify (text preserved) | reduced coordinate precision | usvg |
| GIF  | static GIFs re-encoded losslessly | — | gif (pure Rust) |

Notes:

* JPEG lossless is bit-for-bit on pixels (coefficients rewritten).
* Animated GIF and animated WebP are skipped (not flattened).
* WebP re-encode is from decoded pixels. If the file has an ICC profile and
  the policy is keep-color (the lossless default), it is skipped rather than
  stripping the profile. `--strip all` or `--lossy` (which defaults to strip
  all) allows the rebuild.
* SVGs with SMIL, `<script>`, `on*=` handlers, CSS animation,
  `<foreignObject>`, or external/`data:` `<use>` are skipped. Output is
  visually lossless, not byte-identical.
* AVIF is detected and skipped until an optimizer exists.
* Lossy JPEG/PNG/WebP rebuilds drop metadata, so they only run when the
  policy is strip-all (the `--lossy` default). `--strip color`/`none` with
  `--lossy` falls back to lossless for those files.

## GitHub Action

Composite action (Linux, macOS, Windows). It downloads a **release** binary
(the action ref if that tag exists, else `latest`), verifies the SHA-256, and
runs it — it does not compile the PR.

Optimize and commit:

```yaml
- uses: actions/checkout@v4
- uses: newmatik/image-optimizer@v1
  with:
    paths: "src/assets"
    recursive: "true"
- run: |
    git config user.name  "github-actions[bot]"
    git config user.email "github-actions[bot]@users.noreply.github.com"
    git add -A
    git diff --cached --quiet || git commit -m "chore: optimize images"
    git push
```

Lint gate (fail the PR if images can still shrink):

```yaml
- uses: actions/checkout@v4
- uses: newmatik/image-optimizer@v1
  with:
    paths: "src/assets"
    recursive: "true"
    check: "true"
```

Inputs: `paths` (required), `lossy`, `quality`, `min-savings`, `recursive`,
`strip`, `check`, `dry-run`, `json`, `version`, `extra-args`.

## Architecture

```
crates/cli   imageopt binary: args, path expansion, progress, table/JSON, exit codes
crates/core  imageopt-core: format detect → codec candidates → validate → pick smallest → atomic write
```

Codecs only *propose* encodings. The engine re-decodes candidates, keeps the
smallest valid one (unless `--keep-larger`), and writes via temp file → fsync →
rename. C-codec panics are caught per file. Core has no CLI/async/HTTP
dependencies.

ADRs and roadmap: [`docs/architecture`](docs/architecture/README.md).

## Develop

```bash
cargo build --all-features
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo run -- --dry-run <dir>
```

CI is `.github/workflows/ci.yml` (Linux/macOS/Windows, MSRV 1.82, audit/deny,
coverage artifact). First build is slow (C codecs); later builds use `target/`.

Release profile must keep `panic = "unwind"`: mozjpeg reports errors by
unwinding, which the engine converts to a per-file failure. `panic = "abort"`
would kill the process.

Fuzz `optimize_bytes` from `fuzz/` (separate workspace): `cargo fuzz run optimize_bytes`.

## Contribute

* Match existing module boundaries: engine/codecs in `crates/core`, UX in `crates/cli`.
* One logical change per commit. Add a test when you touch uncovered behavior.
* Do not rewrite JPEG FFI or swap codecs without a strong reason.
* Do not add a server, GUI, or extra runtime tools to the core crate.
* GPL-3.0-or-later (libimagequant). See [LICENSE](LICENSE).
