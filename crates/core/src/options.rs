//! Optimization options shared by every codec.

/// What to do with image metadata (EXIF, XMP, comments, color profiles, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MetadataPolicy {
    /// Remove all non-essential metadata, including the ICC color profile.
    /// Smallest output, but colors may shift on color-managed displays.
    StripAll,
    /// Remove EXIF/XMP/thumbnails/comments but keep the ICC color profile so
    /// colors are preserved. This is the default.
    #[default]
    KeepColorProfile,
    /// Keep all metadata; only recompress pixel/coefficient data.
    KeepAll,
}

/// Knobs controlling how aggressively an image is optimized.
///
/// The scope for v1 is **in-place, same-format optimization**: there are no
/// resize or format-conversion options here on purpose.
#[derive(Clone, Debug)]
pub struct OptimizeOptions {
    /// Allow lossy recompression. When `false` (default) only lossless
    /// transformations are applied, matching ImageOptim's default behavior.
    pub lossy: bool,
    /// Target quality (1–100) for lossy encoders. `None` uses a sensible
    /// per-codec default. Ignored when `lossy` is `false`.
    pub quality: Option<u8>,
    /// oxipng optimization level (0–6). Higher is slower but smaller; 6 enables
    /// Zopfli. Only affects PNG.
    pub png_level: u8,
    /// Metadata handling policy.
    pub metadata: MetadataPolicy,
    /// If `true`, keep a re-encoded file even when it is larger than the
    /// original. Off by default — the engine never enlarges a file otherwise.
    pub keep_larger: bool,
    /// Reject images whose pixel count exceeds this limit (decompression-bomb
    /// guard). Default ~268 megapixels (16384×16384).
    pub max_pixels: u64,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        OptimizeOptions {
            lossy: false,
            quality: None,
            png_level: 3,
            metadata: MetadataPolicy::default(),
            keep_larger: false,
            max_pixels: 16_384 * 16_384,
        }
    }
}

impl OptimizeOptions {
    /// Effective quality, clamped to 1–100, falling back to `default` when unset.
    pub fn quality_or(&self, default: u8) -> u8 {
        self.quality.unwrap_or(default).clamp(1, 100)
    }

    /// Whether a lossy raster candidate that *rebuilds the image from pixels*
    /// may be emitted. Such re-encoders (lossy JPEG/PNG/WebP) cannot preserve an
    /// embedded ICC profile or other metadata, so they only run when the policy
    /// is to strip everything — otherwise a smaller-but-profile-less candidate
    /// could win and violate `KeepColorProfile`/`KeepAll`.
    pub fn allow_lossy_rebuild(&self) -> bool {
        self.lossy && matches!(self.metadata, MetadataPolicy::StripAll)
    }
}
