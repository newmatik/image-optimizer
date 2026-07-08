//! JPEG optimization.
//!
//! * **Lossless (default).** A jpegtran-style coefficient transform via
//!   `mozjpeg-sys`: the DCT coefficients are read and re-written with optimized
//!   Huffman tables and a progressive scan script, and markers are kept/stripped
//!   per the metadata policy. Pixel data is **bit-for-bit unchanged**.
//! * **Lossy (`--lossy`).** Decode + re-encode through the safe `mozjpeg` crate
//!   at the requested quality (progressive, optimized Huffman).
//!
//! libjpeg reports errors by calling `error_exit`, which we wire to
//! `resume_unwind`; every entry point is wrapped in `catch_unwind` so a
//! malformed JPEG becomes a recoverable error instead of taking down the
//! process. The engine additionally validates and never enlarges the output.

use std::os::raw::c_int;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::metadata::{keep_all, keep_color_profile};
use crate::options::OptimizeOptions;

pub struct JpegOptimizer;

impl Optimizer for JpegOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        let mut out = Vec::new();

        // A lossy re-encode discards ICC/EXIF/COM markers, so it may only be
        // offered when the policy is to strip everything (otherwise a smaller
        // profile-less candidate could win and violate the metadata policy).
        let allow_lossy = opts.allow_lossy_rebuild();

        match lossless_transform(input, opts) {
            Ok(v) => out.push(v),
            Err(e) => {
                // If lossless failed and we won't try a lossy re-encode, this is
                // a genuine failure (engine -> Failed, original untouched).
                if !allow_lossy {
                    return Err(e);
                }
            }
        }

        if allow_lossy && should_offer_lossy(input, opts) {
            match lossy_reencode(input, opts) {
                Ok(v) => out.push(v),
                Err(e) => {
                    if out.is_empty() {
                        return Err(e);
                    }
                }
            }
        }

        Ok(CandidateSet::Candidates(out))
    }
}

// --- lossless (jpegtran-style coefficient transform) -----------------------

fn lossless_transform(input: &[u8], opts: &OptimizeOptions) -> Result<Vec<u8>, Error> {
    let keep_icc = keep_color_profile(opts.metadata);
    let keep_everything = keep_all(opts.metadata);
    flatten(catch_unwind(AssertUnwindSafe(|| unsafe {
        jpegtran(input, keep_icc, keep_everything)
    })))
}

/// Guards that always tear down the libjpeg objects, including on unwind.
struct DecompressGuard(*mut mozjpeg_sys::jpeg_decompress_struct);
impl Drop for DecompressGuard {
    fn drop(&mut self) {
        unsafe { mozjpeg_sys::jpeg_destroy_decompress(&mut *self.0) }
    }
}
struct CompressGuard(*mut mozjpeg_sys::jpeg_compress_struct);
impl Drop for CompressGuard {
    fn drop(&mut self) {
        unsafe { mozjpeg_sys::jpeg_destroy_compress(&mut *self.0) }
    }
}

/// Frees the `jpeg_mem_dest` output buffer (malloc'd by libjpeg) even if a later
/// libjpeg call unwinds via `error_exit` before we copy it out. Holds the
/// address of the buffer pointer so it reads the final (possibly realloc'd)
/// value at drop time.
struct MemDestBuf(*mut *mut u8);
impl Drop for MemDestBuf {
    fn drop(&mut self) {
        unsafe {
            let p = *self.0;
            if !p.is_null() {
                libc::free(p as *mut libc::c_void);
            }
        }
    }
}

unsafe extern "C-unwind" fn error_exit(_cinfo: &mut mozjpeg_sys::jpeg_common_struct) {
    // libjpeg has hit a fatal error. Unwind back to the catch_unwind in
    // `lossless_transform`. (Kept free of Rust destructors on the C side.)
    std::panic::resume_unwind(Box::new("libjpeg fatal error"));
}

unsafe extern "C-unwind" fn silence_message(_cinfo: &mut mozjpeg_sys::jpeg_common_struct) {}

const JPEG_APP0: c_int = 0xE0;
const JPEG_COM: c_int = 0xFE;
const TRUE: c_int = 1;

// Explicit `&mut *boxed` derefs make the FFI ownership obvious; keep them.
#[allow(clippy::explicit_auto_deref)]
unsafe fn jpegtran(input: &[u8], keep_icc: bool, keep_all_markers: bool) -> Result<Vec<u8>, Error> {
    use mozjpeg_sys::*;

    // Shared error manager. Must outlive both structs (declared first → dropped
    // last). error_exit unwinds; output_message is silenced.
    let mut jerr: jpeg_error_mgr = std::mem::zeroed();
    jpeg_std_error(&mut jerr);
    jerr.error_exit = Some(error_exit);
    jerr.output_message = Some(silence_message);

    let mut dinfo: Box<jpeg_decompress_struct> = Box::new(std::mem::zeroed());
    dinfo.common.err = &mut jerr;
    jpeg_create_decompress(&mut *dinfo);
    let _dguard = DecompressGuard(&mut *dinfo);

    let mut cinfo: Box<jpeg_compress_struct> = Box::new(std::mem::zeroed());
    cinfo.common.err = &mut jerr;
    jpeg_create_compress(&mut *cinfo);
    let _cguard = CompressGuard(&mut *cinfo);

    jpeg_mem_src(&mut *dinfo, input.as_ptr(), input.len() as c_ulong);

    // Choose which markers to preserve (must precede jpeg_read_header). libjpeg
    // regenerates JFIF APP0 and Adobe APP14 itself, so we never copy those.
    if keep_all_markers {
        for app in 1..=15 {
            if app == 14 {
                continue;
            }
            jpeg_save_markers(&mut *dinfo, JPEG_APP0 + app, 0xFFFF);
        }
        jpeg_save_markers(&mut *dinfo, JPEG_COM, 0xFFFF);
    } else if keep_icc {
        // ICC profiles are carried in APP2 markers.
        jpeg_save_markers(&mut *dinfo, JPEG_APP0 + 2, 0xFFFF);
    }

    jpeg_read_header(&mut *dinfo, TRUE);

    let coefs = jpeg_read_coefficients(&mut *dinfo);
    if coefs.is_null() {
        return Err(Error::Encode("jpeg_read_coefficients returned null".into()));
    }

    let mut outbuf: *mut u8 = ptr::null_mut();
    let mut outsize: c_ulong = 0;
    jpeg_mem_dest(&mut *cinfo, &mut outbuf, &mut outsize);
    // RAII so the buffer is freed even if a later libjpeg call unwinds.
    let _outbuf_guard = MemDestBuf(&mut outbuf);

    jpeg_copy_critical_parameters(&*dinfo, &mut *cinfo);
    cinfo.optimize_coding = TRUE; // optimized Huffman tables
    jpeg_simple_progression(&mut *cinfo); // progressive scan script

    jpeg_write_coefficients(&mut *cinfo, coefs);

    // Copy preserved markers verbatim into the output.
    if keep_all_markers || keep_icc {
        let mut marker = dinfo.marker_list;
        while !marker.is_null() {
            let m = &*marker;
            jpeg_write_marker(
                &mut *cinfo,
                m.marker as c_int,
                m.data,
                m.data_length as c_uint,
            );
            marker = m.next;
        }
    }

    jpeg_finish_compress(&mut *cinfo);
    jpeg_finish_decompress(&mut *dinfo);

    if outbuf.is_null() || outsize == 0 {
        Err(Error::Encode("libjpeg produced an empty output".into()))
    } else {
        Ok(std::slice::from_raw_parts(outbuf, outsize as usize).to_vec())
    }
    // _outbuf_guard frees the buffer; _cguard, _dguard, jerr drop after.
}

// --- lossy (decode + re-encode) --------------------------------------------

fn lossy_reencode(input: &[u8], opts: &OptimizeOptions) -> Result<Vec<u8>, Error> {
    let quality = opts.quality_or(80) as f32;
    flatten(catch_unwind(AssertUnwindSafe(
        || -> Result<Vec<u8>, Error> {
            use mozjpeg::{ColorSpace, Compress, Decompress};

            let dinfo = Decompress::new_mem(input).map_err(|e| Error::Decode(e.to_string()))?;
            let width = dinfo.width();
            let height = dinfo.height();
            let mut started = dinfo.rgb().map_err(|e| Error::Decode(e.to_string()))?;
            let pixels: Vec<u8> = started
                .read_scanlines::<u8>()
                .map_err(|e| Error::Decode(e.to_string()))?;
            let _ = started.finish();

            let mut comp = Compress::new(ColorSpace::JCS_RGB);
            comp.set_size(width, height);
            comp.set_quality(quality);
            comp.set_progressive_mode();
            comp.set_optimize_coding(true);

            let mut writer = comp
                .start_compress(Vec::new())
                .map_err(|e| Error::Encode(e.to_string()))?;
            writer
                .write_scanlines(&pixels)
                .map_err(|e| Error::Encode(e.to_string()))?;
            writer.finish().map_err(|e| Error::Encode(e.to_string()))
        },
    )))
}

// --- helpers ---------------------------------------------------------------

fn should_offer_lossy(input: &[u8], opts: &OptimizeOptions) -> bool {
    let target = opts.quality_or(80);
    match estimate_jpeg_quality(input) {
        Some(source) => source > target.saturating_add(2),
        None => true,
    }
}

/// Estimate JPEG quality from the first luminance quantization value.
///
/// This is intentionally conservative and only used to avoid repeated
/// destructive recompression in CI loops. If the source already appears to be at
/// or below the target quality, offering a lossy candidate would likely degrade
/// it for marginal savings.
fn estimate_jpeg_quality(input: &[u8]) -> Option<u8> {
    const STD_LUMA_Q00: f64 = 16.0;

    let q00 = first_luma_quant_value(input)? as f64;
    if q00 <= 0.0 {
        return None;
    }

    let scale = (q00 * 100.0) / STD_LUMA_Q00;
    let quality = if scale <= 100.0 {
        (200.0 - scale) / 2.0
    } else {
        5000.0 / scale
    };
    Some(quality.round().clamp(1.0, 100.0) as u8)
}

fn first_luma_quant_value(input: &[u8]) -> Option<u16> {
    if input.len() < 4 || input[0] != 0xFF || input[1] != 0xD8 {
        return None;
    }

    let mut i = 2;
    while i + 4 <= input.len() {
        while i < input.len() && input[i] == 0xFF {
            i += 1;
        }
        if i >= input.len() {
            return None;
        }
        let marker = input[i];
        i += 1;

        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        if i + 2 > input.len() {
            return None;
        }

        let len = u16::from_be_bytes([input[i], input[i + 1]]) as usize;
        if len < 2 || i + len > input.len() {
            return None;
        }
        let segment = &input[i + 2..i + len];
        if marker == 0xDB {
            return parse_dqt_q00(segment);
        }
        i += len;
    }
    None
}

fn parse_dqt_q00(mut segment: &[u8]) -> Option<u16> {
    while !segment.is_empty() {
        let info = *segment.first()?;
        let precision = info >> 4;
        let table_id = info & 0x0F;
        segment = &segment[1..];

        let bytes_per_value = match precision {
            0 => 1,
            1 => 2,
            _ => return None,
        };
        let table_len = 64 * bytes_per_value;
        if segment.len() < table_len {
            return None;
        }

        if table_id == 0 {
            return if bytes_per_value == 1 {
                Some(segment[0] as u16)
            } else {
                Some(u16::from_be_bytes([segment[0], segment[1]]))
            };
        }
        segment = &segment[table_len..];
    }
    None
}

fn flatten(r: std::thread::Result<Result<Vec<u8>, Error>>) -> Result<Vec<u8>, Error> {
    match r {
        Ok(inner) => inner,
        Err(p) => Err(Error::Panicked(crate::error::panic_message(p))),
    }
}
