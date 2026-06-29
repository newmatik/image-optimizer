//! The optimization engine: dispatch, candidate selection, validation, and
//! crash-safe writing.

use std::fs;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::codecs;
use crate::error::Error;
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
    /// A file finished processing.
    Finished {
        index: usize,
        total: usize,
        result: Box<OptimizeResult>,
    },
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

    // Codec calls cross into C libraries (mozjpeg, libwebp, …) which report
    // errors by unwinding. Catch any unwind so one bad file can never abort the
    // process; the original is preserved.
    let candidates = catch_unwind(AssertUnwindSafe(|| codec.candidates(input, opts)))
        .map_err(|p| Error::Panicked(panic_message(p)))??;

    let (bytes, status) = pick_best(input, candidates, format, opts.keep_larger);
    Ok(OptimizedImage {
        optimized_size: bytes.len() as u64,
        bytes,
        format,
        original_size,
        status,
    })
}

/// Pick the smallest valid candidate. Returns the original bytes with
/// [`OptimizeStatus::AlreadyOptimal`] if nothing smaller (and valid) is found.
fn pick_best(
    input: &[u8],
    candidates: Vec<Vec<u8>>,
    format: ImageFormat,
    keep_larger: bool,
) -> (Vec<u8>, OptimizeStatus) {
    let mut sorted = candidates;
    sorted.sort_by_key(Vec::len);
    for cand in sorted {
        let smaller = cand.len() < input.len();
        if !smaller && !keep_larger {
            continue;
        }
        if cand.is_empty() {
            continue;
        }
        if validate(&cand, format) {
            return (cand, OptimizeStatus::Optimized);
        }
    }
    (input.to_vec(), OptimizeStatus::AlreadyOptimal)
}

/// Re-decode a candidate to ensure it is a valid image before we ever write it
/// over a good original.
fn validate(bytes: &[u8], format: ImageFormat) -> bool {
    match format {
        ImageFormat::Svg => {
            #[cfg(feature = "svg")]
            {
                usvg::Tree::from_data(bytes, &usvg::Options::default()).is_ok()
            }
            #[cfg(not(feature = "svg"))]
            {
                !bytes.is_empty()
            }
        }
        ImageFormat::WebP => {
            #[cfg(feature = "webp")]
            {
                webp::Decoder::new(bytes).decode().is_some()
            }
            #[cfg(not(feature = "webp"))]
            {
                image::load_from_memory(bytes).is_ok()
            }
        }
        _ => image::load_from_memory(bytes).is_ok(),
    }
}

/// Optimize a single file according to `sink`. Never panics.
pub fn optimize_file(path: &Path, opts: &OptimizeOptions, sink: &OutputSink) -> OptimizeResult {
    let start = Instant::now();

    let input = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return result(
                Some(path),
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
                Some(path),
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

    // Persist only when we actually have a smaller, validated output.
    if out.is_optimized_status() {
        if let OutputSink::InPlace { backup } = sink {
            if *backup {
                if let Err(e) = backup_original(path) {
                    return result(
                        Some(path),
                        out.format,
                        original_size,
                        original_size,
                        OptimizeStatus::Failed {
                            error: format!("backup failed: {e}"),
                        },
                        start,
                    );
                }
            }
            if let Err(e) = atomic_write(path, &out.bytes) {
                return result(
                    Some(path),
                    out.format,
                    original_size,
                    original_size,
                    OptimizeStatus::Failed {
                        error: format!("write failed: {e}"),
                    },
                    start,
                );
            }
        }
    }

    result(
        Some(path),
        out.format,
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
            progress(ProgressEvent::Finished {
                index,
                total,
                result: Box::new(res.clone()),
            });
            (index, res)
        })
        .collect();
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, r)| r).collect()
}

// --- helpers ---------------------------------------------------------------

impl OptimizedImage {
    fn is_optimized_status(&self) -> bool {
        matches!(self.status, OptimizeStatus::Optimized)
    }
}

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
    fs::copy(path, dst)?;
    Ok(())
}

/// Write `bytes` to `path` atomically: temp file in the same directory →
/// preserve permissions → fsync → rename over the original.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));

    let mut tmp = tempfile::Builder::new()
        .prefix(".imageopt-")
        .suffix(".tmp")
        .tempfile_in(dir)?;

    // Match the original file's permissions so we don't silently relax/lock it.
    if let Ok(meta) = fs::metadata(path) {
        let _ = tmp.as_file().set_permissions(meta.permissions());
    }

    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn result(
    source: Option<&Path>,
    format: ImageFormat,
    original_size: u64,
    optimized_size: u64,
    status: OptimizeStatus,
    start: Instant,
) -> OptimizeResult {
    OptimizeResult {
        source: source.map(Path::to_path_buf),
        format,
        original_size,
        optimized_size,
        status,
        elapsed: start.elapsed().max(Duration::ZERO),
    }
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}
