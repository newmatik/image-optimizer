//! Result types returned by the engine.

use std::path::PathBuf;
use std::time::Duration;

use crate::format::ImageFormat;

/// Outcome of attempting to optimize one image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptimizeStatus {
    /// A smaller, valid output was produced (and written, unless dry-run).
    Optimized,
    /// Nothing smaller could be produced; the original was kept as-is.
    AlreadyOptimal,
    /// The format is not handled by this build; the file was left untouched.
    Skipped { reason: String },
    /// Optimization failed; the original file was left untouched.
    Failed { error: String },
}

impl OptimizeStatus {
    pub fn label(&self) -> &'static str {
        match self {
            OptimizeStatus::Optimized => "optimized",
            OptimizeStatus::AlreadyOptimal => "already optimal",
            OptimizeStatus::Skipped { .. } => "skipped",
            OptimizeStatus::Failed { .. } => "failed",
        }
    }
}

/// Per-file optimization result.
#[derive(Clone, Debug)]
pub struct OptimizeResult {
    /// Source path, if the result came from a file (vs. raw bytes).
    pub source: Option<PathBuf>,
    /// Detected input format.
    pub format: ImageFormat,
    /// Size of the original input, in bytes.
    pub original_size: u64,
    /// Size of the optimized output, in bytes (equals `original_size` when not
    /// optimized). For a dry run this is the size the file *would* become.
    pub optimized_size: u64,
    /// Outcome.
    pub status: OptimizeStatus,
    /// Wall-clock time spent on this file.
    pub elapsed: Duration,
}

impl OptimizeResult {
    /// Bytes saved (positive means smaller). Can be negative only if
    /// `keep_larger` was set.
    pub fn saved_bytes(&self) -> i64 {
        self.original_size as i64 - self.optimized_size as i64
    }

    /// Percentage of the original size that was saved.
    pub fn saved_percent(&self) -> f64 {
        if self.original_size == 0 {
            0.0
        } else {
            self.saved_bytes() as f64 / self.original_size as f64 * 100.0
        }
    }

    pub fn is_optimized(&self) -> bool {
        matches!(self.status, OptimizeStatus::Optimized)
    }
}

/// In-memory optimization result (no filesystem involved).
#[derive(Clone, Debug)]
pub struct OptimizedImage {
    /// Best output bytes. Equal to the input when [`OptimizeStatus::AlreadyOptimal`]
    /// or non-`Optimized`.
    pub bytes: Vec<u8>,
    /// Detected input format.
    pub format: ImageFormat,
    /// Original input size in bytes.
    pub original_size: u64,
    /// Output size in bytes.
    pub optimized_size: u64,
    /// Outcome.
    pub status: OptimizeStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(original: u64, optimized: u64, status: OptimizeStatus) -> OptimizeResult {
        OptimizeResult {
            source: None,
            format: ImageFormat::Png,
            original_size: original,
            optimized_size: optimized,
            status,
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn saved_bytes_and_percent() {
        let r = result(200, 150, OptimizeStatus::Optimized);

        assert_eq!(r.saved_bytes(), 50);
        assert!((r.saved_percent() - 25.0).abs() < 1e-9);
        assert!(r.is_optimized());
    }

    #[test]
    fn saved_is_negative_when_output_grew() {
        let r = result(100, 130, OptimizeStatus::Optimized);

        assert_eq!(r.saved_bytes(), -30);
        assert!(r.saved_percent() < 0.0);
    }

    #[test]
    fn zero_sized_original_does_not_divide_by_zero() {
        let r = result(0, 0, OptimizeStatus::AlreadyOptimal);

        assert_eq!(r.saved_percent(), 0.0);
        assert!(!r.is_optimized());
    }

    #[test]
    fn status_labels() {
        assert_eq!(OptimizeStatus::Optimized.label(), "optimized");
        assert_eq!(OptimizeStatus::AlreadyOptimal.label(), "already optimal");
        assert_eq!(
            OptimizeStatus::Skipped {
                reason: String::new()
            }
            .label(),
            "skipped"
        );
        assert_eq!(
            OptimizeStatus::Failed {
                error: String::new()
            }
            .label(),
            "failed"
        );
    }
}
