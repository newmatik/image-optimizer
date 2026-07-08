//! Per-format optimizers.
//!
//! Each codec implements [`Optimizer`] and only ever *proposes* candidate
//! encodings — it never decides whether to keep them and never writes files.
//! The engine ([`crate::engine`]) selects the smallest valid candidate.

use crate::error::Error;
use crate::format::ImageFormat;
use crate::options::OptimizeOptions;

#[cfg(feature = "gif")]
mod gif;
#[cfg(feature = "jpeg")]
mod jpeg;
#[cfg(feature = "png")]
mod png;
#[cfg(feature = "svg")]
mod svg;
#[cfg(feature = "webp")]
mod webp;

/// Result of asking a codec to process an input.
pub enum CandidateSet {
    /// Candidate encodings for the engine to validate and compare.
    Candidates(Vec<Vec<u8>>),
    /// The codec intentionally left the file untouched for a non-fatal reason.
    Skipped { reason: String },
}

/// Produces candidate re-encodings of an input image.
pub trait Optimizer {
    /// Return candidate encodings or an intentional non-fatal skip.
    ///
    /// Empty candidates mean "nothing better was produced"; the engine will then
    /// keep the original as already optimal. Unsupported format features should
    /// return [`CandidateSet::Skipped`] so CLI and JSON output can explain why.
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error>;

    /// Validate that produced bytes decode as a valid image, so the engine never
    /// writes a corrupt candidate over a good original. The default re-decodes
    /// via the `image` crate; codecs whose format `image` cannot (re)decode
    /// reliably (WebP, SVG) override this.
    fn validate(&self, bytes: &[u8]) -> bool {
        image::load_from_memory(bytes).is_ok()
    }
}

/// Resolve the optimizer for a detected format, if this build supports it.
pub fn codec_for(format: ImageFormat) -> Option<Box<dyn Optimizer>> {
    match format {
        #[cfg(feature = "jpeg")]
        ImageFormat::Jpeg => Some(Box::new(jpeg::JpegOptimizer)),
        #[cfg(feature = "png")]
        ImageFormat::Png => Some(Box::new(png::PngOptimizer)),
        #[cfg(feature = "gif")]
        ImageFormat::Gif => Some(Box::new(gif::GifOptimizer)),
        #[cfg(feature = "webp")]
        ImageFormat::WebP => Some(Box::new(webp::WebpOptimizer)),
        #[cfg(feature = "svg")]
        ImageFormat::Svg => Some(Box::new(svg::SvgOptimizer)),
        _ => None,
    }
}
