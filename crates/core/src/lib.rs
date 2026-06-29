//! `imageopt-core` — the image optimization engine behind the `imageopt` CLI.
//!
//! The engine is deliberately frontend-agnostic (no async, no CLI, no HTTP) so
//! it can later be reused by a server or desktop GUI without changes.
//!
//! # Design guarantees
//!
//! * **Never enlarges a file.** Each codec only ever *proposes* candidate
//!   encodings; [`engine::optimize_bytes`] keeps the smallest one and, unless
//!   [`OptimizeOptions::keep_larger`] is set, discards anything that is not
//!   smaller than the input — returning the original bytes untouched.
//! * **Never writes a corrupt file.** A chosen candidate is re-decoded for
//!   validation before it is accepted; if it does not decode, the next-smallest
//!   candidate is tried, else the original is kept.
//! * **Crash-safe writes.** In-place optimization writes to a temporary file in
//!   the same directory, fsyncs, then atomically renames over the original.
//! * **Panic-safe.** Codec calls (which cross into C libraries) are wrapped in
//!   `catch_unwind`; a panic becomes a [`OptimizeStatus::Failed`] for that file
//!   and the original is left untouched — it never takes the process down.

pub mod codecs;
pub mod engine;
pub mod error;
pub mod format;
pub mod metadata;
pub mod options;
pub mod result;

pub use engine::{optimize_bytes, optimize_file, optimize_paths, OutputSink, ProgressEvent};
pub use error::Error;
pub use format::{detect_format, ImageFormat};
pub use options::{MetadataPolicy, OptimizeOptions};
pub use result::{OptimizeResult, OptimizeStatus, OptimizedImage};

/// Formats this build can optimize, given the enabled cargo features.
pub fn supported_formats() -> &'static [ImageFormat] {
    &[
        #[cfg(feature = "jpeg")]
        ImageFormat::Jpeg,
        #[cfg(feature = "png")]
        ImageFormat::Png,
        #[cfg(feature = "gif")]
        ImageFormat::Gif,
        #[cfg(feature = "webp")]
        ImageFormat::WebP,
        #[cfg(feature = "svg")]
        ImageFormat::Svg,
    ]
}
