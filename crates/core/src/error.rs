//! Engine error type.

use thiserror::Error;

/// Errors produced while optimizing a single image in memory.
///
/// File-level problems (read/write failures) are reported through
/// [`crate::result::OptimizeStatus::Failed`] rather than this type.
#[derive(Debug, Error)]
pub enum Error {
    /// The input could not be decoded by the codec for its detected format.
    #[error("decode failed: {0}")]
    Decode(String),

    /// Re-encoding failed.
    #[error("encode failed: {0}")]
    Encode(String),

    /// The image exceeds the configured pixel limit (decompression-bomb guard).
    #[error("image too large: {pixels} pixels exceeds limit of {limit}")]
    TooLarge { pixels: u64, limit: u64 },

    /// A codec panicked (e.g. inside a C library) and the unwind was caught.
    #[error("codec aborted: {0}")]
    Panicked(String),

    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Anything else.
    #[error("{0}")]
    Other(String),
}
