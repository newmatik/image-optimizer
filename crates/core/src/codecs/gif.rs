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
use crate::metadata::keep_all;
use crate::options::OptimizeOptions;

pub struct GifOptimizer;

impl Optimizer for GifOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        // The re-encode writes the frame only. Comments and application data
        // (XMP, for example) cannot be carried over, so keep-all leaves a file
        // that has them alone.
        if keep_all(opts.metadata) && has_comment_or_application_data(input) {
            return Ok(CandidateSet::Skipped {
                reason: "GIF re-encode would drop comments or application data (keep-all policy)"
                    .to_string(),
            });
        }

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

/// Whether a GIF carries a comment extension or an application extension.
/// Walks the block structure, so the same bytes inside image data do not
/// count. A file that cannot be walked to its end counts as having them.
fn has_comment_or_application_data(input: &[u8]) -> bool {
    const EXTENSION: u8 = 0x21;
    const IMAGE: u8 = 0x2C;
    const TRAILER: u8 = 0x3B;
    const COMMENT: u8 = 0xFE;
    const APPLICATION: u8 = 0xFF;

    // A color table has 3 * 2^(n+1) bytes when the flag in `packed` is set.
    let table_len = |packed: u8| -> usize {
        if packed & 0x80 != 0 {
            3 << ((packed & 0x07) + 1)
        } else {
            0
        }
    };
    // Index just past a chain of sub-blocks (length byte, data, ..., 0).
    let skip_sub_blocks = |mut i: usize| -> Option<usize> {
        loop {
            let len = *input.get(i)? as usize;
            i += 1 + len;
            if len == 0 {
                return Some(i);
            }
        }
    };

    let walk = || -> Option<bool> {
        // Header (6), logical screen descriptor (7), global color table.
        let mut i = 13 + table_len(*input.get(10)?);
        loop {
            match *input.get(i)? {
                TRAILER => return Some(false),
                EXTENSION => {
                    if matches!(*input.get(i + 1)?, COMMENT | APPLICATION) {
                        return Some(true);
                    }
                    i = skip_sub_blocks(i + 2)?;
                }
                IMAGE => {
                    // Descriptor (10), local color table, LZW code size (1).
                    let after_table = i + 10 + table_len(*input.get(i + 9)?);
                    i = skip_sub_blocks(after_table + 1)?;
                }
                _ => return None,
            }
        }
    };
    walk().unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif_with(extension: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut out, 2, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
            let frame = gif::Frame {
                width: 2,
                height: 1,
                buffer: vec![0, 1].into(),
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
        // Insert the extension after the header, screen descriptor and the
        // 2-color global table.
        let at = 13 + 6;
        out.splice(at..at, extension.iter().copied());
        out
    }

    #[test]
    fn comment_and_application_extensions_are_found_by_block_walk() {
        assert!(!has_comment_or_application_data(&gif_with(&[])));
        // Comment extension "hi".
        assert!(has_comment_or_application_data(&gif_with(&[
            0x21, 0xFE, 2, b'h', b'i', 0
        ])));
        // Application extension with an 11-byte identifier.
        let mut app = vec![0x21, 0xFF, 11];
        app.extend_from_slice(b"XMP DataXMP");
        app.extend_from_slice(&[1, 0, 0]);
        assert!(has_comment_or_application_data(&gif_with(&app)));
        // A plain-text extension whose data contains the marker bytes.
        assert!(!has_comment_or_application_data(&gif_with(&[
            0x21, 0x01, 2, 0x21, 0xFE, 0
        ])));
        // Truncated: cannot be walked, so nothing is assumed safe.
        let cut = gif_with(&[]);
        assert!(has_comment_or_application_data(&cut[..cut.len() - 3]));
    }
}
