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
