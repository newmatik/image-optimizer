//! GIF optimization (pure-Rust, thread-safe).
//!
//! Scope for v1: **static (single-frame) GIFs** are re-encoded losslessly
//! (decoded as palette indices and re-written, which re-runs LZW compression
//! and can drop slack). **Animated GIFs are left untouched** — robust animated
//! optimization needs gifsicle, whose crate only exposes a non-reentrant CLI
//! entry point that is unsafe to call from our parallel engine. Skipping is the
//! reliable choice; the engine reports the file as skipped with a reason.

use std::io::Cursor;

use gif::{ColorOutput, DecodeOptions, Encoder};

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::options::OptimizeOptions;

pub struct GifOptimizer;

impl Optimizer for GifOptimizer {
    fn candidates(&self, input: &[u8], _opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        let mut decode = DecodeOptions::new();
        // Indexed output preserves the exact palette + pixels (lossless).
        decode.set_color_output(ColorOutput::Indexed);
        let mut decoder = decode
            .read_info(Cursor::new(input))
            .map_err(|e| Error::Decode(format!("gif: {e}")))?;

        let width = decoder.width();
        let height = decoder.height();
        let global_palette = decoder.global_palette().map(<[u8]>::to_vec);

        let mut frames = Vec::new();
        while let Some(frame) = decoder
            .read_next_frame()
            .map_err(|e| Error::Decode(format!("gif frame: {e}")))?
        {
            frames.push(frame.clone());
            if frames.len() > 1 {
                break; // animated -> out of scope for v1
            }
        }

        if frames.len() != 1 {
            // Animated or empty: leave untouched.
            return Ok(CandidateSet::Skipped {
                reason: "animated or empty GIFs are left untouched".to_string(),
            });
        }

        let palette = global_palette.unwrap_or_default();
        let mut buf = Vec::new();
        {
            let mut encoder = Encoder::new(&mut buf, width, height, &palette)
                .map_err(|e| Error::Encode(format!("gif encode: {e}")))?;
            encoder
                .write_frame(&frames[0])
                .map_err(|e| Error::Encode(format!("gif write: {e}")))?;
        }

        Ok(CandidateSet::Candidates(vec![buf]))
    }
}
