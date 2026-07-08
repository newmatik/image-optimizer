# image-optimizer (`imageopt`)

A fast, reliable, **cross-platform** image optimizer in the spirit of
[ImageOptim](https://imageoptim.com/) — but it runs anywhere (Linux, macOS,
Windows) as a single self-contained binary, with **no runtime dependencies** and
**no external tools to install**.

It strips junk and recompresses images to the smallest valid output, reports how
much space was saved, and is designed to drop straight into **CI/CD pipelines**
(there's a ready-made GitHub Action).

```
FILE          FORMAT  ORIGINAL   NEW        SAVED    STATUS
hero.png      png     625.7 KB   822 B      99.9%    optimized
photo.jpg     jpeg    14.7 KB    9.6 KB     34.6%    optimized
icon.svg      svg     383 B      327 B      14.6%    optimized
image.webp    webp    1.5 KB     694 B      53.6%    optimized
static.gif    gif     1.9 KB     1.7 KB     7.7%     optimized
─────────────────────────────────────────────────────────────
TOTAL  5 optimized   644.0 KB → 13.1 KB  (-98.0%, saved 630.9 KB)
```

## Why

* **One binary, everywhere.** All codecs (mozjpeg, oxipng, libimagequant,
  libwebp) are compiled in. Nothing to `apt install` at runtime, no Node, no
  Python, no shelling out.
* **Never corrupts or enlarges a file.** Every re-encode is validated by
  re-decoding it before it's written, the smallest valid result wins, and files
  are written atomically (temp file → fsync → rename). If nothing smaller can be
  made, the original is left exactly as-is.
* **Lossless by default**, opt-in lossy (`--lossy`) for much smaller files.
* **Parallel.** Files are optimized across all CPU cores.

## Supported formats

| Format | Lossless (default) | Lossy (`--lossy`) | Engine |
|--------|:---:|:---:|--------|
| JPEG | ✅ jpegtran-style coefficient re-optimization (progressive, optimized Huffman) | ✅ re-encode at quality | mozjpeg |
| PNG  | ✅ IDAT recompress + reductions (+Zopfli at `--png-level 6`) | ✅ palette quantization | oxipng + libimagequant |
| WebP | ✅ lossless re-encode | ✅ re-encode at quality | libwebp |
| SVG  | ✅ normalize + minify (text preserved) | ✅ reduced coordinate precision | usvg/resvg |
| GIF  | ✅ static GIFs re-encoded losslessly | — | gif (pure Rust) |

Notes:
* **JPEG lossless is truly lossless** — the DCT coefficients are re-written, so
  pixels are bit-for-bit identical.
* **Animated GIFs are left untouched** (skipped). Robust animated optimization
  needs gifsicle, which is not safe to call from the parallel engine; this is a
  planned enhancement.
* **SVGs using SMIL animation, `<script>`, or `<foreignObject>` are left
  untouched** so nothing is ever silently dropped.
* AVIF files are detected but left untouched until an AVIF optimizer is added.

## Install

### Prebuilt binaries

Download the archive for your platform from the
[Releases](https://github.com/newmatik/image-optimizer/releases) page, extract,
and put `imageopt` on your `PATH`. Targets: macOS (arm64, x64), Linux (x64,
arm64), Windows (x64).

### From source

Requires a Rust toolchain plus a C toolchain with **nasm** and **cmake** (needed
to build mozjpeg/libwebp):

```bash
# macOS:    brew install nasm cmake
# Ubuntu:   sudo apt-get install -y nasm cmake
# Windows:  choco install nasm cmake   (use the MSVC toolchain)

cargo install --path crates/cli   # or: cargo build --release
```

## Usage

```bash
imageopt [PATHS...] [OPTIONS]
```

`PATHS` can be files, directories, or glob patterns.

```bash
imageopt logo.png photo.jpg          # optimize specific files in place
imageopt assets/                     # every image in a directory
imageopt -r assets/                  # …and all subdirectories
imageopt "src/**/*.{png,jpg}"        # glob (quote so the tool expands it)
imageopt --dry-run assets/           # preview savings, change nothing
imageopt --lossy -q 75 photos/       # lossy, quality 75
imageopt --backup logo.png           # keep logo.png.orig
imageopt --json assets/ > report.json
```

### Options

| Flag | Description |
|------|-------------|
| `-r, --recursive` | Recurse into subdirectories. |
| `--lossy` | Allow lossy recompression. |
| `-q, --quality <1-100>` | Quality for lossy encoders (implies `--lossy`). |
| `--png-level <0-6>` | oxipng effort; 6 enables Zopfli (slowest). Default 3. |
| `--min-savings <PERCENT>` | Only rewrite a file if it shrinks by at least this much. Default 0 (lossless) / 10 (`--lossy`); see the idempotency note. |
| `--strip <all\|color\|none>` | Metadata: strip all, keep ICC color profile, or keep everything. Default: keep the color profile — but with `--lossy` the default becomes `all` (see note). |
| `--dry-run` | Report what would change without modifying files. |
| `--backup` | Copy each original to `<name>.orig` before overwriting. |
| `--check` | CI gate: write nothing; exit non-zero if any file could be optimized. |
| `--json` | Machine-readable JSON output. |
| `-j, --jobs <N>` | Parallel workers (default: CPU cores). |
| `--keep-larger` | Keep a re-encode even if larger than the original. |
| `--quiet` | Only print the final summary. |

By default `imageopt` **optimizes files in place** (writes are atomic). Use
`--dry-run` to preview or `--backup` to keep originals.

> **Lossy and metadata:** lossy re-encoders (JPEG/PNG/WebP) rebuild the image
> from pixels and cannot preserve an embedded ICC profile, so `--lossy` defaults
> to stripping all metadata. If you pass `--strip color`/`--strip none` together
> with `--lossy`, the metadata policy is honored and those files fall back to
> lossless optimization (the lossy candidate is skipped). SVG `--lossy` only
> reduces coordinate precision and is unaffected.

### Idempotency (safe to re-run)

**Lossless optimization is deterministic and idempotent**: the transforms are
exact, and a candidate is only written if it's *strictly smaller* than the
current file, so the second run on the same files finds nothing to do (`already
optimal`) and never rewrites or degrades them. Safe to run on every CI push.

**Lossy** re-encoding, by contrast, can shave a sliver on *every* run, which
would slowly degrade an image across repeated runs. To prevent that, `--lossy`
defaults `--min-savings` to **10%**: after the first pass, re-encoding no longer
clears the threshold, so the file is left untouched — lossy becomes effectively
idempotent (it converges after one pass). Pass `--min-savings 0` to squeeze every
last byte, but don't do that in a repeated commit-back workflow.

This matters most in CI. If you run `imageopt` on every push, prefer lossless
mode or keep the default lossy threshold so JPEG/WebP assets are not recompressed
again and again for marginal savings. JPEG lossy mode also checks the source
quantization table and skips destructive re-encoding when the file already
appears to be at or below the requested quality.

### Exit codes

* `0` — success.
* `1` — with `--check`, at least one file could be optimized (or failed).
* `2` — no matching input files.

### JSON output

`--json` emits a stable top-level `summary` plus per-file `results`, intended
for CI systems and dashboards:

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

`status` is one of `optimized`, `already_optimal`, `skipped`, or `failed`.
Skipped files are left untouched and do not fail `--check`; failed files do.

## Use in GitHub Actions

This repo ships a composite action (Linux, macOS, and Windows runners).

**Optimize images and commit the result:**

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

**Fail a PR if images aren't optimized (a lint gate):**

```yaml
- uses: actions/checkout@v4
- uses: newmatik/image-optimizer@v1
  with:
    paths: "src/assets"
    recursive: "true"
    check: "true"
```

Action inputs: `paths` (required), `lossy`, `quality`, `min-savings`, `recursive`, `strip`,
`check`, `dry-run`, `json`, `version`, `extra-args`.

You can also just download the binary in any workflow and run it directly — see
the release assets.

## How it works

The engine is a small library crate (`imageopt-core`) consumed by the CLI. For
each file it detects the format by content, asks the matching codec to *propose*
candidate encodings, then keeps the **smallest candidate that re-decodes
cleanly** — and only if it's smaller than the original. Codec calls (which cross
into C libraries) are run on a panic-catching boundary, so a single malformed
image is reported as `failed` and never takes the process down or corrupts the
original.

The library has no async or HTTP dependencies; an HTTP API and a desktop GUI can
be added later as additional front-ends without touching the engine.

Architecture decisions and the improvement roadmap live in
[`docs/architecture`](docs/architecture/README.md).

## License

GPL-3.0-or-later. This project uses GPL-licensed compression libraries
(libimagequant) to match ImageOptim's compression quality. See [LICENSE](LICENSE).
