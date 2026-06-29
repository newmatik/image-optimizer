//! WebP optimization via libwebp.
//!
//! WebP files are already well-compressed, so the realistic wins are:
//! re-encoding a sub-optimally stored *lossless* WebP, or (with `--lossy`)
//! re-encoding at a target quality. The engine keeps the result only if it is
//! actually smaller, so this never enlarges a file.

use webp::Encoder;

use super::Optimizer;
use crate::error::Error;
use crate::options::OptimizeOptions;

pub struct WebpOptimizer;

impl Optimizer for WebpOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<Vec<Vec<u8>>, Error> {
        let decoded = webp::Decoder::new(input)
            .decode()
            .ok_or_else(|| Error::Decode("libwebp could not decode input".into()))?;
        let image = decoded.to_image();

        let encoder =
            Encoder::from_image(&image).map_err(|e| Error::Encode(format!("webp: {e}")))?;

        let mut out = Vec::new();
        // Lossless re-encode (useful when the source is lossless but inefficient).
        out.push(encoder.encode_lossless().to_vec());

        if opts.lossy {
            let quality = opts.quality_or(80) as f32;
            out.push(encoder.encode(quality).to_vec());
        }

        Ok(out)
    }
}
