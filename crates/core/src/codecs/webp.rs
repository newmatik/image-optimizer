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
use crate::metadata::{keep_all, keep_color_profile};
use crate::options::OptimizeOptions;

pub struct WebpOptimizer;

/// VP8X flags byte (LSB). See the WebP container spec / libwebp
/// `format_constants.h`.
const ANIMATION_FLAG: u8 = 0x02;
const XMP_FLAG: u8 = 0x04;
const EXIF_FLAG: u8 = 0x08;
const ICCP_FLAG: u8 = 0x20;

impl Optimizer for WebpOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        // Re-encoding an animated WebP through the still decoder keeps only the
        // first frame; skip it so the original animation is preserved.
        if is_animated_webp(input) {
            return Ok(CandidateSet::Skipped {
                reason: "animated WebP is left untouched".to_string(),
            });
        }

        // `Encoder::from_image` rebuilds from decoded pixels and cannot copy
        // ICC/EXIF/XMP. Skip rather than emit a smaller candidate that would
        // violate KeepColorProfile / KeepAll (lossy rebuilds are already gated
        // by `allow_lossy_rebuild`).
        if pixel_rebuild_drops_kept_metadata(input, opts) {
            return Ok(CandidateSet::Skipped {
                reason: "WebP re-encode would drop ICC/EXIF/XMP under the current metadata policy"
                    .to_string(),
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
/// chunk. Chunk fourccs are parsed as RIFF chunks so the letters `ANIM` inside
/// a still-image payload do not produce a false skip.
pub(crate) fn is_animated_webp(input: &[u8]) -> bool {
    let Some(chunks) = webp_chunks(input) else {
        return false;
    };
    for (fourcc, payload) in chunks {
        if fourcc == b"VP8X" && payload.first().is_some_and(|f| f & ANIMATION_FLAG != 0) {
            return true;
        }
        if fourcc == b"ANIM" {
            return true;
        }
    }
    false
}

/// True when a pixel-rebuild candidate would drop metadata the current policy
/// requires us to keep. Simple (non-VP8X) WebP has no ICC/EXIF/XMP chunks.
fn pixel_rebuild_drops_kept_metadata(input: &[u8], opts: &OptimizeOptions) -> bool {
    if !keep_color_profile(opts.metadata) && !keep_all(opts.metadata) {
        return false;
    }
    let Some(chunks) = webp_chunks(input) else {
        return false;
    };
    let mut has_icc = false;
    let mut has_exif_or_xmp = false;
    for (fourcc, payload) in chunks {
        if fourcc == b"VP8X" {
            if let Some(&flags) = payload.first() {
                has_icc |= flags & ICCP_FLAG != 0;
                has_exif_or_xmp |= flags & (EXIF_FLAG | XMP_FLAG) != 0;
            }
        }
        has_icc |= fourcc == b"ICCP";
        has_exif_or_xmp |= fourcc == b"EXIF" || fourcc == b"XMP ";
    }
    if keep_all(opts.metadata) {
        has_icc || has_exif_or_xmp
    } else {
        has_icc
    }
}

/// Iterate RIFF chunks after the 12-byte `RIFF....WEBP` header.
fn webp_chunks(input: &[u8]) -> Option<WebpChunkIter<'_>> {
    if input.len() < 12 || &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
        return None;
    }
    Some(WebpChunkIter { rest: &input[12..] })
}

struct WebpChunkIter<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for WebpChunkIter<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < 8 {
            return None;
        }
        let fourcc = &self.rest[..4];
        let size = u32::from_le_bytes(self.rest[4..8].try_into().ok()?) as usize;
        let payload_end = 8usize.checked_add(size)?;
        if payload_end > self.rest.len() {
            return None;
        }
        let payload = &self.rest[8..payload_end];
        // RIFF chunks are padded to even length.
        let next = if size % 2 == 1 {
            payload_end.saturating_add(1)
        } else {
            payload_end
        };
        self.rest = self.rest.get(next.min(self.rest.len())..).unwrap_or(&[]);
        Some((fourcc, payload))
    }
}
