//! The optimization engine: dispatch, candidate selection, validation, and
//! crash-safe writing.

use std::fs;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::codecs::{self, CandidateSet};
use crate::error::{panic_message, Error};
use crate::format::{detect_format, ImageFormat};
use crate::options::OptimizeOptions;
use crate::result::{OptimizeResult, OptimizeStatus, OptimizedImage};

/// Where optimized bytes should go.
#[derive(Clone, Debug)]
pub enum OutputSink {
    /// Overwrite the original file atomically. When `backup` is set, the
    /// original is first copied to `<name>.orig`.
    InPlace { backup: bool },
    /// Write results into `root`, leaving the source untouched. Each file is
    /// written to `root/<file_name>` (creating `root` if needed), so callers
    /// flattening multiple source directories must ensure file names are unique.
    /// Unlike [`OutputSink::InPlace`], every non-failed file is written
    /// (optimized where possible, otherwise the original bytes) so the
    /// destination is a complete copy of the batch.
    Directory { root: PathBuf },
    /// Compute results but write nothing.
    DryRun,
}

/// Progress callbacks emitted by [`optimize_paths`].
#[derive(Clone, Debug)]
pub enum ProgressEvent {
    /// A file is about to be processed.
    Started {
        index: usize,
        total: usize,
        name: String,
    },
    /// A file finished processing. (The full results are also returned from
    /// [`optimize_paths`]; this event is for live progress only.)
    Finished { index: usize, total: usize },
}

/// Optimize raw image bytes in memory. Never panics on bad input; codec panics
/// are caught and reported as [`Error::Panicked`].
pub fn optimize_bytes(input: &[u8], opts: &OptimizeOptions) -> Result<OptimizedImage, Error> {
    let format = detect_format(input);
    let original_size = input.len() as u64;

    let codec = match codecs::codec_for(format) {
        Some(c) => c,
        None => {
            return Ok(OptimizedImage {
                bytes: input.to_vec(),
                format,
                original_size,
                optimized_size: original_size,
                status: OptimizeStatus::Skipped {
                    reason: skip_reason(format),
                },
            });
        }
    };

    // Decompression-bomb guard: refuse oversized raster images before any full
    // decode. SVG is vector (no pixel budget); Unknown was handled above.
    if !matches!(format, ImageFormat::Svg) {
        if let Some((w, h)) = pixel_dimensions(input) {
            let pixels = w as u64 * h as u64;
            if pixels > opts.max_pixels {
                return Err(Error::TooLarge {
                    pixels,
                    limit: opts.max_pixels,
                });
            }
        }
    }

    // Codec calls cross into C libraries (mozjpeg, libwebp, …) which report
    // errors by unwinding. Catch any unwind so one bad file can never abort the
    // process; the original is preserved.
    let candidate_set = catch_unwind(AssertUnwindSafe(|| codec.candidates(input, opts)))
        .map_err(|p| Error::Panicked(panic_message(p)))??;
    let candidates = match candidate_set {
        CandidateSet::Candidates(candidates) => candidates,
        CandidateSet::Skipped { reason } => {
            return Ok(OptimizedImage {
                bytes: input.to_vec(),
                format,
                original_size,
                optimized_size: original_size,
                status: OptimizeStatus::Skipped { reason },
            });
        }
    };

    let (bytes, status) = pick_best(
        input,
        candidates,
        codec.as_ref(),
        opts.keep_larger,
        opts.min_savings_percent,
    );
    Ok(OptimizedImage {
        optimized_size: bytes.len() as u64,
        bytes,
        format,
        original_size,
        status,
    })
}

/// Pick the smallest valid candidate. Returns the original bytes with
/// [`OptimizeStatus::AlreadyOptimal`] if nothing smaller (and valid) is found,
/// or if the best candidate doesn't save at least `min_savings_percent`.
/// The codec validates its own output (see [`codecs::Optimizer::validate`]) so
/// we never replace a good original with something that doesn't re-decode.
fn pick_best(
    input: &[u8],
    candidates: Vec<Vec<u8>>,
    codec: &dyn codecs::Optimizer,
    keep_larger: bool,
    min_savings_percent: f64,
) -> (Vec<u8>, OptimizeStatus) {
    let mut sorted = candidates;
    sorted.sort_by_key(Vec::len);
    for cand in sorted {
        if cand.is_empty() {
            continue;
        }
        if cand.len() >= input.len() && !keep_larger {
            continue;
        }
        if !codec.validate(&cand) {
            continue;
        }
        // This is the smallest valid candidate (best savings). If a *smaller*
        // candidate doesn't clear the threshold, no other will either, so keep
        // the original. The gate is keyed on the candidate actually being
        // smaller (not on `keep_larger`) so `--min-savings` is honored even when
        // `keep_larger` is set; `keep_larger` only governs non-smaller outputs.
        if cand.len() < input.len() && min_savings_percent > 0.0 && !input.is_empty() {
            let saved = (input.len() as f64 - cand.len() as f64) / input.len() as f64 * 100.0;
            if saved < min_savings_percent {
                return (input.to_vec(), OptimizeStatus::AlreadyOptimal);
            }
        }
        return (cand, OptimizeStatus::Optimized);
    }
    (input.to_vec(), OptimizeStatus::AlreadyOptimal)
}

/// Read pixel dimensions from the image header without a full decode (best
/// effort; returns `None` for formats whose header we cannot cheaply parse).
///
/// The `image` crate handles most raster formats, but it cannot always read a
/// WebP header (notably extended/animated WebP), which would leave the
/// decompression-bomb guard blind before the codec performs a full decode. We
/// fall back to a direct WebP header probe so `max_pixels` is enforced there
/// too.
fn pixel_dimensions(input: &[u8]) -> Option<(u32, u32)> {
    if let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(input)).with_guessed_format() {
        if let Ok(dims) = reader.into_dimensions() {
            return Some(dims);
        }
    }
    webp_dimensions(input)
}

/// Parse the canvas dimensions of a RIFF/WebP file from its header, covering the
/// simple lossy (`VP8 `), simple lossless (`VP8L`), and extended (`VP8X`)
/// layouts. Returns `None` if the bytes are not a WebP we can measure.
fn webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 16 || &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return None;
    }
    match &b[12..16] {
        b"VP8X" => {
            // Flags (1) + reserved (3) then 24-bit width-1 and height-1 (LE).
            if b.len() < 30 {
                return None;
            }
            let w = u24_le(&b[24..27]) + 1;
            let h = u24_le(&b[27..30]) + 1;
            Some((w, h))
        }
        b"VP8L" => {
            // Data starts at 20; first byte is the 0x2f signature, then a
            // packed 14-bit width-1 and 14-bit height-1.
            if b.len() < 25 || b[20] != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            Some((w, h))
        }
        b"VP8 " => {
            // Data starts at 20; 3-byte frame tag, then the start code
            // 0x9d 0x01 0x2a, then 14-bit width and height (LE).
            if b.len() < 30 || b[23] != 0x9d || b[24] != 0x01 || b[25] != 0x2a {
                return None;
            }
            let w = u16::from_le_bytes([b[26], b[27]]) as u32 & 0x3FFF;
            let h = u16::from_le_bytes([b[28], b[29]]) as u32 & 0x3FFF;
            Some((w, h))
        }
        _ => None,
    }
}

fn u24_le(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16
}

/// Optimize a single file according to `sink`. Never panics.
pub fn optimize_file(path: &Path, opts: &OptimizeOptions, sink: &OutputSink) -> OptimizeResult {
    let start = Instant::now();

    let input = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return result(
                path,
                ImageFormat::Unknown,
                0,
                0,
                OptimizeStatus::Failed {
                    error: format!("read failed: {e}"),
                },
                start,
            );
        }
    };
    let original_size = input.len() as u64;
    let detected = detect_format(&input);

    let out = match optimize_bytes(&input, opts) {
        Ok(o) => o,
        Err(e) => {
            return result(
                path,
                detected,
                original_size,
                original_size,
                OptimizeStatus::Failed {
                    error: e.to_string(),
                },
                start,
            );
        }
    };

    let format = out.format;
    let fail = |error: String| {
        result(
            path,
            format,
            original_size,
            original_size,
            OptimizeStatus::Failed { error },
            start,
        )
    };

    match sink {
        OutputSink::DryRun => {}
        // In-place: persist only when we produced a smaller, validated output.
        OutputSink::InPlace { backup } => {
            if matches!(out.status, OptimizeStatus::Optimized) {
                if *backup {
                    if let Err(e) = backup_original(path) {
                        return fail(format!("backup failed: {e}"));
                    }
                }
                if let Err(e) = atomic_write(path, &out.bytes) {
                    return fail(format!("write failed: {e}"));
                }
            }
        }
        // Directory: write every non-failed file so the destination is a
        // complete copy (optimized bytes where possible, else the original).
        OutputSink::Directory { root } => {
            if !matches!(out.status, OptimizeStatus::Failed { .. }) {
                let file_name = match path.file_name() {
                    Some(n) => n,
                    None => return fail("source path has no file name".to_string()),
                };
                let dst = root.join(file_name);
                if let Err(e) = fs::create_dir_all(root) {
                    return fail(format!("create output dir failed: {e}"));
                }
                if let Err(e) = atomic_write(&dst, &out.bytes) {
                    return fail(format!("write failed: {e}"));
                }
            }
        }
    }

    result(
        path,
        format,
        original_size,
        out.optimized_size,
        out.status,
        start,
    )
}

/// A counting semaphore over a byte budget. Workers acquire an amount before
/// processing a file and release it afterwards, so the total size of files in
/// flight stays under the budget. An acquire request is clamped to the whole
/// budget, so a single oversized file is always eventually admitted (alone)
/// rather than deadlocking.
struct ByteSemaphore {
    budget: u64,
    available: Mutex<u64>,
    ready: Condvar,
}

impl ByteSemaphore {
    fn new(budget: u64) -> Self {
        ByteSemaphore {
            budget,
            available: Mutex::new(budget),
            ready: Condvar::new(),
        }
    }

    /// Reserve `want` bytes (clamped to at least 1 and at most the whole
    /// budget), blocking until they are available. Returns the amount reserved,
    /// which must be passed back to [`ByteSemaphore::release`].
    fn acquire(&self, want: u64) -> u64 {
        let want = want.clamp(1, self.budget);
        let mut available = self.available.lock().unwrap();
        while *available < want {
            available = self.ready.wait(available).unwrap();
        }
        *available -= want;
        want
    }

    fn release(&self, amount: u64) {
        let mut available = self.available.lock().unwrap();
        *available += amount;
        self.ready.notify_all();
    }
}

/// Optimize many paths in parallel (via rayon), preserving input order in the
/// returned vector. `progress` is invoked as work starts and finishes.
///
/// When [`OptimizeOptions::max_in_flight_bytes`] is set, the combined size of
/// files being processed concurrently is throttled to that budget so a batch of
/// large images does not exhaust memory.
pub fn optimize_paths<F>(
    paths: &[PathBuf],
    opts: &OptimizeOptions,
    sink: &OutputSink,
    progress: F,
) -> Vec<OptimizeResult>
where
    F: Fn(ProgressEvent) + Sync + Send,
{
    let total = paths.len();
    let budget = opts
        .max_in_flight_bytes
        .filter(|b| *b > 0)
        .map(ByteSemaphore::new);
    let budget = budget.as_ref();

    let mut indexed: Vec<(usize, OptimizeResult)> = paths
        .par_iter()
        .enumerate()
        .map(|(index, path)| {
            progress(ProgressEvent::Started {
                index,
                total,
                name: path.display().to_string(),
            });
            // Throttle on the file's on-disk size (a proxy for its memory cost)
            // when a budget is configured.
            let permit = budget.map(|sem| {
                let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                sem.acquire(size)
            });
            let res = optimize_file(path, opts, sink);
            if let (Some(sem), Some(amount)) = (budget, permit) {
                sem.release(amount);
            }
            progress(ProgressEvent::Finished { index, total });
            (index, res)
        })
        .collect();
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, r)| r).collect()
}

// --- helpers ---------------------------------------------------------------

fn skip_reason(format: ImageFormat) -> String {
    match format {
        ImageFormat::Unknown => "unrecognized or unsupported file format".to_string(),
        other => format!(
            "{} optimization is not enabled in this build",
            other.as_str()
        ),
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".orig");
    PathBuf::from(s)
}

fn backup_original(path: &Path) -> std::io::Result<()> {
    let dst = backup_path(path);
    // Never clobber an existing pristine backup: a later `--backup` run must not
    // overwrite the first one with already-optimized contents. `create_new`
    // fails atomically if it exists, which we treat as "keep the existing one".
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dst)
    {
        Ok(mut out) => {
            // If copy/sync fails after create_new, remove the partial .orig so a
            // later run isn't blocked by a truncated/empty backup.
            let written = (|| {
                let mut src = fs::File::open(path)?;
                std::io::copy(&mut src, &mut out)?;
                out.sync_all()
            })();
            if written.is_err() {
                let _ = fs::remove_file(&dst);
            }
            written
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write `bytes` to `path` atomically: temp file in the same directory →
/// preserve permissions → fsync file → rename over the original → fsync the
/// directory (POSIX) so the replacement survives a crash/power-loss.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));

    let mut tmp = tempfile::Builder::new()
        .prefix(".imageopt-")
        .suffix(".tmp")
        .tempfile_in(dir)?;

    // Match the original file's permissions so we don't silently relax/lock it.
    // Surface a failure rather than silently changing accessibility.
    if let Ok(meta) = fs::metadata(path) {
        tmp.as_file().set_permissions(meta.permissions())?;
    }

    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;

    // Best-effort: fsync the containing directory so the rename itself is
    // durable. Directory fsync is a POSIX concept; skip elsewhere.
    #[cfg(unix)]
    if let Ok(dir_file) = fs::File::open(dir) {
        let _ = dir_file.sync_all();
    }
    Ok(())
}

fn result(
    source: &Path,
    format: ImageFormat,
    original_size: u64,
    optimized_size: u64,
    status: OptimizeStatus,
    start: Instant,
) -> OptimizeResult {
    OptimizeResult {
        source: Some(source.to_path_buf()),
        format,
        original_size,
        optimized_size,
        status,
        elapsed: start.elapsed().max(Duration::ZERO),
    }
}
