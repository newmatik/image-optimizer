//! The optimization engine: dispatch, candidate selection, validation, and
//! crash-safe writing.

use std::fs;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
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
fn pixel_dimensions(input: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(input))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
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

    // Persist only when we actually have a smaller, validated output.
    if matches!(out.status, OptimizeStatus::Optimized) {
        if let OutputSink::InPlace { backup } = sink {
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

    result(
        path,
        format,
        original_size,
        out.optimized_size,
        out.status,
        start,
    )
}

/// Optimize many paths in parallel (via rayon), preserving input order in the
/// returned vector. `progress` is invoked as work starts and finishes.
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
    let mut indexed: Vec<(usize, OptimizeResult)> = paths
        .par_iter()
        .enumerate()
        .map(|(index, path)| {
            progress(ProgressEvent::Started {
                index,
                total,
                name: path.display().to_string(),
            });
            let res = optimize_file(path, opts, sink);
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
