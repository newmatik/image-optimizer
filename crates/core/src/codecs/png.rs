//! PNG optimization.
//!
//! * Lossless (always): [`oxipng`] re-compresses IDAT, reduces bit depth/color
//!   type/palette, and strips metadata per policy.
//! * Lossy (`--lossy`): [`imagequant`] (the pngquant engine) reduces the image
//!   to an optimal ≤256-color palette; the quantized image is then run back
//!   through oxipng so it is stored as an indexed PNG.
//!
//! Both results are returned as candidates so the engine keeps whichever is
//! smaller (and never one larger than the input).

use oxipng::{Options, StripChunks};

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::metadata::{exif_orientation, keep_all, orientation_only_tiff, ExifOrientation};
use crate::options::{MetadataPolicy, OptimizeOptions};

pub struct PngOptimizer;

impl Optimizer for PngOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        let mut out = Vec::new();

        // Lossless pass. If this fails the input is not a valid PNG we can
        // handle, so surface the error (engine -> Failed, original untouched).
        let animated = is_animated_png(input);
        let mut options = oxipng_options(opts);
        // "Strip all" would remove the animation chunks with the metadata
        // and leave the first frame. They are image data: keep them.
        if animated && matches!(options.strip, StripChunks::All) {
            options.strip = StripChunks::Keep(oxipng::indexset! {*b"acTL", *b"fcTL", *b"fdAT"});
        }
        // Browsers rotate a PNG by its EXIF orientation. Keep the `eXIf`
        // chunk of a rotated image when metadata is stripped; without the
        // tag the picture would be shown turned.
        let orientation = match exif_chunk(input).map(|(_, data)| exif_orientation(data)) {
            Some(ExifOrientation::Turned(value)) => Some(value),
            // Not readable: stripping could drop an orientation.
            Some(ExifOrientation::Unreadable) if !keep_all(opts.metadata) => {
                return Ok(CandidateSet::Skipped {
                    reason: "PNG has EXIF data that could not be read; left untouched to keep its orientation"
                        .to_string(),
                });
            }
            _ => None,
        };
        let rotated = orientation.is_some();
        if rotated {
            options.strip = match options.strip {
                StripChunks::All => StripChunks::Keep(oxipng::indexset! {*b"eXIf"}),
                StripChunks::Keep(mut kept) => {
                    kept.insert(*b"eXIf");
                    StripChunks::Keep(kept)
                }
                // The chunks "safe" keeps, plus `eXIf`.
                StripChunks::Safe => {
                    let mut kept: oxipng::IndexSet<[u8; 4]> = SAFE_CHUNKS.into_iter().collect();
                    kept.insert(*b"eXIf");
                    StripChunks::Keep(kept)
                }
                other => other,
            };
        }
        let mut lossless = oxipng::optimize_from_memory(input, &options)
            .map_err(|e| Error::Encode(format!("oxipng: {e}")))?;
        // Unless everything is kept, the orientation is all that stays of the
        // EXIF data.
        if let (Some(orientation), false) = (orientation, keep_all(opts.metadata)) {
            lossless = with_exif(&lossless, &orientation_only_tiff(orientation));
        }
        out.push(lossless);

        // Lossy quantization rebuilds the PNG from RGBA pixels, discarding any
        // source metadata, so only offer it when the policy is to strip all.
        // It also decodes a single image: an animated PNG would come out as
        // its first frame only, so animation keeps the lossless pass alone.
        // Nor can it carry the orientation over.
        if opts.allow_lossy_rebuild() && !animated && !rotated {
            if let Ok(q) = quantize(input, opts) {
                out.push(q);
            }
        }

        Ok(CandidateSet::Candidates(out))
    }
}

/// Whether a PNG is animated (APNG): it has an `acTL` chunk, which must come
/// before the first `IDAT`.
pub(crate) fn is_animated_png(input: &[u8]) -> bool {
    let mut rest = input.get(8..).unwrap_or(&[]);
    while let (Some(len), Some(kind)) = (rest.get(..4), rest.get(4..8)) {
        match kind {
            b"acTL" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => {}
        }
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        // Length, type, data, CRC.
        let Some(next) = len.checked_add(12).and_then(|n| rest.get(n..)) else {
            return false;
        };
        rest = next;
    }
    false
}

/// The first `eXIf` chunk of a PNG: its byte range (length field to CRC) and
/// its data.
fn exif_chunk(input: &[u8]) -> Option<(std::ops::Range<usize>, &[u8])> {
    let mut at = 8usize;
    loop {
        let len = u32::from_be_bytes(input.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize;
        let kind = input.get(at + 4..at + 8)?;
        let end = at.checked_add(len)?.checked_add(12)?;
        let data = input.get(at + 8..end - 4)?;
        match kind {
            b"eXIf" => return Some((at..end, data)),
            b"IEND" => return None,
            _ => {}
        }
        at = end;
    }
}

/// `png` with the data of its first `eXIf` chunk replaced by `tiff`. Any
/// further `eXIf` chunk is removed: it would carry the stripped EXIF data.
fn with_exif(png: &[u8], tiff: &[u8]) -> Vec<u8> {
    let Some((range, _)) = exif_chunk(png) else {
        return png.to_vec();
    };
    let mut rest = Vec::with_capacity(png.len() - range.end);
    let mut at = range.end;
    while let Some(head) = png.get(at..at + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(end) = len.checked_add(12).and_then(|n| n.checked_add(at)) else {
            break;
        };
        let Some(chunk) = png.get(at..end) else {
            break;
        };
        if &head[4..8] != b"eXIf" {
            rest.extend_from_slice(chunk);
        }
        at = end;
    }
    rest.extend_from_slice(&png[at..]);
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in b"eXIf".iter().chain(tiff) {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    let mut out = Vec::with_capacity(png.len());
    out.extend_from_slice(&png[..range.start]);
    out.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
    out.extend_from_slice(b"eXIf");
    out.extend_from_slice(tiff);
    out.extend_from_slice(&(!crc).to_be_bytes());
    out.extend_from_slice(&rest);
    out
}

/// The chunks oxipng's `StripChunks::Safe` keeps (its `DISPLAY_CHUNKS`, which
/// it does not export). A rotated PNG is stripped with this list plus `eXIf`;
/// `exif_orientation_keeps_the_safe_chunks` checks that it still matches.
const SAFE_CHUNKS: [[u8; 4]; 7] = [
    *b"cICP", *b"iCCP", *b"sRGB", *b"pHYs", *b"acTL", *b"fcTL", *b"fdAT",
];

fn oxipng_options(opts: &OptimizeOptions) -> Options {
    let level = opts.png_level.min(6);
    let mut o = Options::from_preset(level);
    o.strip = match opts.metadata {
        MetadataPolicy::StripAll => StripChunks::All,
        // `Safe` keeps the chunks in `SAFE_CHUNKS` (color space, pixel
        // size, animation) and removes the rest.
        MetadataPolicy::KeepColorProfile => StripChunks::Safe,
        MetadataPolicy::KeepAll => StripChunks::None,
    };
    o
}

/// Lossy palette quantization via libimagequant, re-packed by oxipng.
fn quantize(input: &[u8], opts: &OptimizeOptions) -> Result<Vec<u8>, Error> {
    let img = image::load_from_memory(input)
        .map_err(|e| Error::Decode(format!("png decode: {e}")))?
        .to_rgba8();
    let (w, h) = img.dimensions();

    let pixels: Vec<imagequant::RGBA> = img
        .pixels()
        .map(|p| imagequant::RGBA::new(p[0], p[1], p[2], p[3]))
        .collect();

    let mut liq = imagequant::new();
    let target = opts.quality_or(75);
    // Allow a wide quality floor so quantization rarely fails outright; the
    // engine's size comparison decides whether to keep the result.
    liq.set_quality(0, target)
        .map_err(|e| Error::Encode(format!("imagequant quality: {e:?}")))?;
    liq.set_speed(4)
        .map_err(|e| Error::Encode(format!("imagequant speed: {e:?}")))?;

    let mut qimg = liq
        .new_image(pixels, w as usize, h as usize, 0.0)
        .map_err(|e| Error::Encode(format!("imagequant image: {e:?}")))?;

    let mut res = liq
        .quantize(&mut qimg)
        .map_err(|e| Error::Encode(format!("imagequant quantize: {e:?}")))?;
    res.set_dithering_level(1.0)
        .map_err(|e| Error::Encode(format!("imagequant dither: {e:?}")))?;

    let (palette, indices) = res
        .remapped(&mut qimg)
        .map_err(|e| Error::Encode(format!("imagequant remap: {e:?}")))?;

    // Rebuild RGBA bytes from the palette so oxipng can store an optimal indexed
    // PNG (it detects the ≤256 colors and builds the palette). `indices` is in
    // row order, so fill the buffer directly.
    let mut raw = Vec::with_capacity(indices.len() * 4);
    for &idx in &indices {
        let c = palette[idx as usize];
        raw.extend_from_slice(&[c.r, c.g, c.b, c.a]);
    }

    let mut buf = Vec::new();
    {
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf))
            .write_image(&raw, w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| Error::Encode(format!("png reencode: {e}")))?;
    }

    // The quantized image is rebuilt from pixels and carries no source metadata
    // (this path only runs under StripAll anyway), so strip everything.
    let mut o = oxipng_options(opts);
    o.strip = StripChunks::All;
    oxipng::optimize_from_memory(&buf, &o).map_err(|e| Error::Encode(format!("oxipng(lossy): {e}")))
}
