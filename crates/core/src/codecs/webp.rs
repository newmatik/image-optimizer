//! WebP optimization via libwebp.
//!
//! WebP files are already well-compressed, so the realistic wins are:
//! re-encoding a sub-optimally stored *lossless* WebP, or (with `--lossy`)
//! re-encoding at a target quality. The engine keeps the result only if it is
//! actually smaller, so this never enlarges a file.
//!
//! Scope for v1: **still (single-frame) WebP only**. The `webp` crate decodes a
//! single still image, so an animated WebP would be flattened to one frame on
//! re-encode. To avoid silently destroying animation, animated WebP is detected
//! and left untouched (reported as `Skipped`), mirroring the animated-GIF path.

use webp::Encoder;

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::options::OptimizeOptions;

pub struct WebpOptimizer;

impl Optimizer for WebpOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        // Re-encoding an animated WebP through the still decoder keeps only the
        // first frame; skip it so the original animation is preserved.
        if is_animated_webp(input) {
            return Ok(CandidateSet::Skipped {
                reason: "animated WebP is left untouched".to_string(),
            });
        }

        let decoded = webp::Decoder::new(input)
            .decode()
            .ok_or_else(|| Error::Decode("libwebp could not decode input".into()))?;
        let image = decoded.to_image();

        let encoder =
            Encoder::from_image(&image).map_err(|e| Error::Encode(format!("webp: {e}")))?;

        let mut out = Vec::new();
        // Lossless re-encode (useful when the source is lossless but inefficient).
        out.push(encoder.encode_lossless().to_vec());

        // Lossy WebP re-encodes from decoded pixels, dropping metadata, so only
        // offer it when the policy permits stripping everything.
        if opts.allow_lossy_rebuild() {
            let quality = opts.quality_or(80) as f32;
            out.push(encoder.encode(quality).to_vec());
        }

        Ok(CandidateSet::Candidates(out))
    }

    fn validate(&self, bytes: &[u8]) -> bool {
        // The `image` crate's pure-Rust WebP decoder is lossless-only; use
        // libwebp so lossy candidates validate too.
        webp::Decoder::new(bytes).decode().is_some()
    }
}

/// Whether a RIFF/WebP container advertises animation.
///
/// Animation only exists in the extended (`VP8X`) format: the VP8X flags byte
/// carries an animation bit, and animated files additionally contain an `ANIM`
/// chunk. We check both signals so a malformed flags byte does not let an
/// animation slip through.
pub(crate) fn is_animated_webp(input: &[u8]) -> bool {
    // Minimal container: "RIFF" <size> "WEBP" <fourcc> ...
    if input.len() < 16 || &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
        return false;
    }
    if &input[12..16] == b"VP8X" {
        // Layout: "VP8X" <u32 chunk size> <flags byte> ...; the animation flag
        // is bit 0x02 of the flags byte at offset 20.
        if input.len() > 20 && input[20] & 0x02 != 0 {
            return true;
        }
    }
    // Fallback: scan a bounded prefix for an ANIM chunk fourcc.
    let scan = &input[..input.len().min(4096)];
    scan.windows(4).any(|w| w == b"ANIM")
}
