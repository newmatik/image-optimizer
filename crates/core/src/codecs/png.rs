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

use super::Optimizer;
use crate::error::Error;
use crate::metadata::keep_color_profile;
use crate::options::{MetadataPolicy, OptimizeOptions};

pub struct PngOptimizer;

impl Optimizer for PngOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<Vec<Vec<u8>>, Error> {
        let mut out = Vec::new();

        // Lossless pass. If this fails the input is not a valid PNG we can
        // handle, so surface the error (engine -> Failed, original untouched).
        let lossless = oxipng::optimize_from_memory(input, &oxipng_options(opts))
            .map_err(|e| Error::Encode(format!("oxipng: {e}")))?;
        out.push(lossless);

        if opts.lossy {
            if let Ok(q) = quantize(input, opts) {
                out.push(q);
            }
        }

        Ok(out)
    }
}

fn oxipng_options(opts: &OptimizeOptions) -> Options {
    let level = opts.png_level.min(6);
    let mut o = Options::from_preset(level);
    o.strip = match opts.metadata {
        MetadataPolicy::StripAll => StripChunks::All,
        // `Safe` removes metadata chunks that don't affect rendering while
        // keeping color-affecting ones (iCCP/sRGB/gAMA/cHRM).
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

    // Rebuild an RGBA8 image from the palette so oxipng can store it as an
    // optimal indexed PNG (it detects the ≤256 colors and builds the palette).
    let mut rgba = image::RgbaImage::new(w, h);
    for (i, &idx) in indices.iter().enumerate() {
        let c = palette[idx as usize];
        let x = (i as u32) % w;
        let y = (i as u32) / w;
        rgba.put_pixel(x, y, image::Rgba([c.r, c.g, c.b, c.a]));
    }

    let mut buf = Vec::new();
    {
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf))
            .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| Error::Encode(format!("png reencode: {e}")))?;
    }

    let mut o = oxipng_options(opts);
    // The quantized image carries no source metadata; never keep a profile here.
    if !keep_color_profile(opts.metadata) {
        o.strip = StripChunks::All;
    }
    oxipng::optimize_from_memory(&buf, &o).map_err(|e| Error::Encode(format!("oxipng(lossy): {e}")))
}
