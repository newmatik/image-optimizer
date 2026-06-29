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

/// Produces candidate re-encodings of an input image.
pub trait Optimizer {
    /// Return zero or more candidate encodings. An empty result means "nothing
    /// better was produced"; the engine will then keep the original.
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<Vec<Vec<u8>>, Error>;
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
