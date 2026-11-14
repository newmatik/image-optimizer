//! Integration tests for the engine's correctness guarantees.

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use imageopt_core::{
    optimize_bytes, optimize_file, optimize_paths, Error, MetadataPolicy, OptimizeOptions,
    OptimizeStatus, OutputSink, ProgressEvent,
};

/// A deliberately weakly-compressed PNG so there is room to optimize.
fn make_png() -> Vec<u8> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};

    let (w, h) = (128u32, 128u32);
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        // A handful of flat color regions — highly compressible.
        let c = (((x / 16) + (y / 16)) % 4) as u8;
        *px = image::Rgba([c * 60, 255 - c * 50, (c as u32 * 40 % 255) as u8, 255]);
    }
    let mut buf = Vec::new();
    PngEncoder::new_with_quality(
        Cursor::new(&mut buf),
        CompressionType::Fast,
        FilterType::NoFilter,
    )
    .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
    .unwrap();
    buf
}

fn make_jpeg() -> Vec<u8> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageEncoder;

    let (w, h) = (96u32, 96u32);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = (((x / 12) + (y / 12)) % 4) as u8;
            rgb.extend_from_slice(&[c * 60, 240 - c * 40, c * 30]);
        }
    }

    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(Cursor::new(&mut buf), 95)
        .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    buf
}

fn make_webp() -> Vec<u8> {
    let (w, h) = (96u32, 96u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = (((x / 12) + (y / 12)) % 4) as u8;
            rgba.extend_from_slice(&[c * 60, 240 - c * 40, c * 30, 255]);
        }
    }

    webp::Encoder::from_rgba(&rgba, w, h).encode(95.0).to_vec()
}

/// A minimal animated WebP: a RIFF/WEBP container whose VP8X flags byte sets the
/// animation bit and which carries an ANIM chunk. It need not hold real frame
/// data — the engine must refuse to touch it before decoding.
fn make_animated_webp() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&[0, 0, 0, 0]); // RIFF size placeholder (patched below)
    b.extend_from_slice(b"WEBP");
    // VP8X chunk (10-byte payload): flags byte with the animation bit, 3 reserved
    // bytes, then 24-bit canvas width-1 and height-1 (little-endian).
    b.extend_from_slice(b"VP8X");
    b.extend_from_slice(&10u32.to_le_bytes());
    b.push(0x02); // animation flag
    b.extend_from_slice(&[0, 0, 0]); // reserved
    b.extend_from_slice(&[0x0f, 0, 0]); // width - 1 = 15 -> 16px
    b.extend_from_slice(&[0x0f, 0, 0]); // height - 1 = 15 -> 16px
                                        // ANIM chunk (6-byte payload): background color + loop count.
    b.extend_from_slice(b"ANIM");
    b.extend_from_slice(&6u32.to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let size = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&size.to_le_bytes());
    b
}

/// A still (non-animated) VP8X WebP header advertising a huge canvas, used to
/// exercise the decompression-bomb guard's WebP fallback probe.
fn make_huge_webp_header() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(b"WEBP");
    b.extend_from_slice(b"VP8X");
    b.extend_from_slice(&10u32.to_le_bytes());
    b.push(0x00); // no animation flag
    b.extend_from_slice(&[0, 0, 0]); // reserved
    b.extend_from_slice(&[0xff, 0xff, 0xff]); // width - 1 = ~16.7M
    b.extend_from_slice(&[0xff, 0xff, 0xff]); // height - 1 = ~16.7M
    let size = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&size.to_le_bytes());
    b
}

fn make_static_gif() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut buf, 2, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
        let frame = gif::Frame {
            width: 2,
            height: 1,
            buffer: vec![0, 1].into(),
            ..Default::default()
        };
        encoder.write_frame(&frame).unwrap();
    }
    buf
}

fn make_animated_gif() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut buf, 1, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
        let frame_a = gif::Frame {
            width: 1,
            height: 1,
            buffer: vec![0].into(),
            ..Default::default()
        };
        let frame_b = gif::Frame {
            width: 1,
            height: 1,
            buffer: vec![1].into(),
            ..Default::default()
        };
        encoder.write_frame(&frame_a).unwrap();
        encoder.write_frame(&frame_b).unwrap();
    }
    buf
}

fn decode_rgb(bytes: &[u8]) -> Vec<u8> {
    image::load_from_memory(bytes).unwrap().to_rgb8().into_raw()
}

#[test]
fn jpeg_lossy_then_repeated_runs_do_not_change_decoded_pixels() {
    let input = make_jpeg();
    let opts = OptimizeOptions {
        lossy: true,
        metadata: MetadataPolicy::StripAll,
        min_savings_percent: 10.0,
        ..Default::default()
    };
    let first = optimize_bytes(&input, &opts).unwrap();

    assert_eq!(first.status, OptimizeStatus::Optimized);
    assert!(
        image::load_from_memory(&first.bytes).is_ok(),
        "optimized JPEG must decode"
    );

    let second = optimize_bytes(&first.bytes, &opts).unwrap();

    assert!(
        matches!(
            second.status,
            OptimizeStatus::AlreadyOptimal | OptimizeStatus::Optimized
        ),
        "unexpected repeated-run JPEG status: {:?}",
        second.status
    );
    assert_eq!(
        decode_rgb(&second.bytes),
        decode_rgb(&first.bytes),
        "repeated lossy-mode runs may do safe lossless rewrites, but must not apply another destructive JPEG recompression"
    );
}

#[test]
fn webp_lossy_path_produces_valid_output_or_keeps_original() {
    let input = make_webp();
    let out = optimize_bytes(
        &input,
        &OptimizeOptions {
            lossy: true,
            metadata: MetadataPolicy::StripAll,
            min_savings_percent: 0.0,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(!matches!(out.status, OptimizeStatus::Failed { .. }));
    assert!(
        webp::Decoder::new(&out.bytes).decode().is_some(),
        "WebP result must decode through libwebp"
    );
}

#[test]
fn animated_webp_is_reported_as_skipped_not_flattened() {
    let input = make_animated_webp();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    match out.status {
        OptimizeStatus::Skipped { reason } => {
            assert!(
                reason.to_lowercase().contains("webp"),
                "unexpected skip reason: {reason}"
            );
        }
        other => panic!("expected skipped animated WebP, got {other:?}"),
    }
    assert_eq!(
        out.bytes, input,
        "animated WebP must be returned verbatim, never flattened to one frame"
    );
}

/// Still VP8L whose payload happens to contain the letters "ANIM". Sliding-window
/// fourcc scans used to treat this as animated and skip a still image.
fn make_still_webp_with_anim_bytes_in_payload() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(b"WEBP");
    b.extend_from_slice(b"VP8L");
    let payload = b"xxxxANIMxxxx";
    b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    b.extend_from_slice(payload);
    let size = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&size.to_le_bytes());
    b
}

/// VP8X still WebP advertising an ICC profile (no animation).
fn make_webp_with_icc_flag() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(b"WEBP");
    b.extend_from_slice(b"VP8X");
    b.extend_from_slice(&10u32.to_le_bytes());
    b.push(0x20); // ICCP flag, not animation
    b.extend_from_slice(&[0, 0, 0]);
    b.extend_from_slice(&[0x0f, 0, 0]); // 16px
    b.extend_from_slice(&[0x0f, 0, 0]);
    b.extend_from_slice(b"ICCP");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0]);
    let size = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&size.to_le_bytes());
    b
}

#[test]
fn still_webp_is_not_skipped_because_anim_chunk_is_past_riff_size() {
    let mut input = make_still_webp_with_anim_bytes_in_payload();
    // Declared RIFF size already covers only the VP8L chunk. Append a real ANIM
    // chunk after the container; it must not count as animation.
    input.extend_from_slice(b"ANIM");
    input.extend_from_slice(&6u32.to_le_bytes());
    input.extend_from_slice(&[0, 0, 0, 0, 0, 0]);

    if let Ok(o) = optimize_bytes(&input, &OptimizeOptions::default()) {
        if let OptimizeStatus::Skipped { reason } = o.status {
            panic!(
                "trailing ANIM past RIFF size must not mark a still WebP animated, got {reason}"
            );
        }
    }
}

#[test]
fn still_webp_is_not_skipped_just_because_payload_contains_anim_bytes() {
    let input = make_still_webp_with_anim_bytes_in_payload();
    if let Ok(o) = optimize_bytes(&input, &OptimizeOptions::default()) {
        if let OptimizeStatus::Skipped { reason } = o.status {
            panic!(
                "still WebP must not be treated as animated because payload bytes spell ANIM, got {reason}"
            );
        }
    }
}

#[test]
fn webp_with_icc_is_not_rebuilt_under_keep_color_profile() {
    let input = make_webp_with_icc_flag();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    match out.status {
        OptimizeStatus::Skipped { reason } => {
            assert!(
                reason.to_lowercase().contains("icc") || reason.to_lowercase().contains("metadata"),
                "unexpected skip reason: {reason}"
            );
        }
        other => panic!("expected skipped WebP that would drop ICC, got {other:?}"),
    }
    assert_eq!(out.bytes, input, "ICC WebP must be returned verbatim");
}

#[test]
fn webp_with_icc_may_rebuild_when_stripping_all() {
    let input = make_webp_with_icc_flag();
    let out = optimize_bytes(
        &input,
        &OptimizeOptions {
            metadata: MetadataPolicy::StripAll,
            ..Default::default()
        },
    );
    // Header-only fixture cannot decode; the point is we no longer skip for
    // metadata, so the codec is allowed to attempt a rebuild.
    match out {
        Err(_) => {}
        Ok(o) => assert!(
            !matches!(o.status, OptimizeStatus::Skipped { .. }),
            "StripAll must not skip WebP for metadata preservation, got {:?}",
            o.status
        ),
    }
}

#[test]
fn static_gif_path_is_non_fatal_and_preserves_decodability() {
    let input = make_static_gif();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    assert!(!matches!(out.status, OptimizeStatus::Failed { .. }));
    assert!(
        image::load_from_memory(&out.bytes).is_ok(),
        "static GIF result must decode"
    );
}

#[test]
fn optimizes_and_is_idempotent_and_never_enlarges() {
    let input = make_png();
    let opts = OptimizeOptions::default();

    let first = optimize_bytes(&input, &opts).unwrap();
    assert_eq!(first.status, OptimizeStatus::Optimized, "should optimize");
    assert!(
        first.optimized_size <= first.original_size,
        "must never enlarge: {} -> {}",
        first.original_size,
        first.optimized_size
    );
    assert!(
        image::load_from_memory(&first.bytes).is_ok(),
        "output must be a valid image"
    );

    // Running again must not grow the file and should report already-optimal.
    let second = optimize_bytes(&first.bytes, &opts).unwrap();
    assert!(second.optimized_size <= first.optimized_size);
    assert_eq!(second.status, OptimizeStatus::AlreadyOptimal);
}

#[test]
fn unknown_format_is_skipped_unchanged() {
    let input = b"this is definitely not an image file".to_vec();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();
    assert!(matches!(out.status, OptimizeStatus::Skipped { .. }));
    assert_eq!(out.bytes, input, "skipped input must be returned verbatim");
}

#[test]
fn avif_is_detected_but_skipped_until_optimizer_exists() {
    let input = b"\x00\x00\x00\x18ftypmif1\x00\x00\x00\x00mif1avif".to_vec();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    match &out.status {
        OptimizeStatus::Skipped { reason } => {
            assert!(
                reason.contains("AVIF") && reason.contains("not yet supported"),
                "unexpected skip reason: {reason}"
            );
        }
        other => panic!("expected skipped AVIF, got {other:?}"),
    }
    assert_eq!(
        out.bytes, input,
        "unsupported AVIF must be returned verbatim"
    );
}

#[test]
fn animated_gif_is_reported_as_skipped_not_already_optimal() {
    let input = make_animated_gif();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    match out.status {
        OptimizeStatus::Skipped { reason } => {
            assert!(reason.contains("GIF"), "unexpected skip reason: {reason}");
        }
        other => panic!("expected skipped animated GIF, got {other:?}"),
    }
    assert_eq!(
        out.bytes, input,
        "intentionally skipped GIF must be returned verbatim"
    );
}

/// A two-frame APNG built by hand: `acTL`, then per frame `fcTL` plus data.
fn make_animated_png() -> Vec<u8> {
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        // CRC-32 as PNG defines it.
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in kind.iter().chain(data) {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&(!crc).to_be_bytes());
        out
    }
    fn frame_control(sequence: u32, w: u32, h: u32) -> Vec<u8> {
        let mut d = sequence.to_be_bytes().to_vec();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[0; 8]); // x and y offset
        d.extend_from_slice(&[0, 1, 0, 2]); // delay 1/2 s
        d.extend_from_slice(&[0, 0]); // dispose, blend
        d
    }

    // Take header and image data from an ordinary PNG of noisy pixels, so the
    // quantized still image would be clearly smaller.
    let (w, h) = (64u32, 64u32);
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([
            (x * 37 + y * 11) as u8,
            (x * 5 + y * 71) as u8,
            (x * y) as u8,
            255,
        ]);
    }
    let mut still = Vec::new();
    {
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(Cursor::new(&mut still))
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
    }
    let ihdr = &still[8..8 + 25];
    let idat_start = still.windows(4).position(|c| c == b"IDAT").unwrap() - 4;
    let idat_len = u32::from_be_bytes(still[idat_start..idat_start + 4].try_into().unwrap());
    let idat_data = &still[idat_start + 8..idat_start + 8 + idat_len as usize];
    assert!(
        still[idat_start + 12 + idat_len as usize + 4..].starts_with(b"IEND"),
        "fixture expects a single IDAT chunk"
    );

    let mut out = still[..8].to_vec();
    out.extend_from_slice(ihdr);
    out.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
    out.extend(chunk(b"fcTL", &frame_control(0, w, h)));
    out.extend(chunk(b"IDAT", idat_data));
    out.extend(chunk(b"fcTL", &frame_control(1, w, h)));
    let mut second = 2u32.to_be_bytes().to_vec();
    second.extend_from_slice(idat_data);
    out.extend(chunk(b"fdAT", &second));
    out.extend(chunk(b"IEND", &[]));
    out
}

fn png_chunk_count(bytes: &[u8], kind: &[u8; 4]) -> usize {
    bytes.windows(4).filter(|c| c == kind).count()
}

/// Neither "strip all" (which used to remove the animation chunks as if they
/// were metadata) nor the lossy palette pass (which decodes one image) may
/// replace an animation with its first frame.
#[test]
fn animated_png_keeps_its_frames_in_every_mode() {
    let input = make_animated_png();
    assert_eq!(png_chunk_count(&input, b"fdAT"), 1);

    for (lossy, metadata) in [
        (false, MetadataPolicy::KeepAll),
        (false, MetadataPolicy::KeepColorProfile),
        (false, MetadataPolicy::StripAll),
        (true, MetadataPolicy::StripAll),
    ] {
        let opts = OptimizeOptions {
            lossy,
            metadata,
            ..Default::default()
        };
        let out = optimize_bytes(&input, &opts).unwrap();
        assert!(
            !matches!(out.status, OptimizeStatus::Failed { .. }),
            "{:?}",
            out.status
        );
        let case = format!("lossy={lossy} {metadata:?}");
        assert_eq!(png_chunk_count(&out.bytes, b"acTL"), 1, "{case}");
        assert_eq!(png_chunk_count(&out.bytes, b"fcTL"), 2, "{case}");
        assert_eq!(png_chunk_count(&out.bytes, b"fdAT"), 1, "{case}");
    }
}

/// Keep-all means keep all: a GIF comment cannot be written back, so the file
/// stays as it is. Other policies may drop it.
#[test]
fn gif_with_a_comment_is_left_alone_under_keep_all() {
    let mut input = make_static_gif();
    let comment = [
        0x21, 0xFE, 9, b'c', b'o', b'p', b'y', b'r', b'i', b'g', b'h', b't', 0,
    ];
    // After header (6), screen descriptor (7) and the 2-color table (6).
    input.splice(19..19, comment);
    assert!(
        image::load_from_memory(&input).is_ok(),
        "fixture must decode"
    );

    let keep = OptimizeOptions {
        metadata: MetadataPolicy::KeepAll,
        ..Default::default()
    };
    let out = optimize_bytes(&input, &keep).unwrap();
    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "{:?}",
        out.status
    );
    assert_eq!(out.bytes, input);

    // Without a comment keep-all still optimizes or keeps, never skips.
    let plain = optimize_bytes(&make_static_gif(), &keep).unwrap();
    assert!(!matches!(plain.status, OptimizeStatus::Skipped { .. }));

    let default = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();
    assert!(!matches!(default.status, OptimizeStatus::Skipped { .. }));
}

/// EXIF data (TIFF) with a camera make and the given orientation.
fn exif_tiff(orientation: u8) -> Vec<u8> {
    let mut tiff = b"MM\0*\0\0\0\x08\0\x02".to_vec();
    tiff.extend_from_slice(&[0x01, 0x0F, 0, 2, 0, 0, 0, 4, b'C', b'a', b'm', 0]);
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation, 0, 0]);
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    tiff
}

fn jpeg_with_exif(orientation: u8) -> Vec<u8> {
    let mut jpeg = make_jpeg();
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&exif_tiff(orientation));
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
    segment.extend_from_slice(&payload);
    // After SOI and the JFIF APP0 segment.
    assert_eq!(&jpeg[2..4], &[0xFF, 0xE0]);
    let at = 4 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
    jpeg.splice(at..at, segment);
    jpeg
}

fn png_with_exif(orientation: u8) -> Vec<u8> {
    png_with_chunk(make_png(), b"eXIf", &exif_tiff(orientation))
}

/// `png` with a chunk inserted right after IHDR.
fn png_with_chunk(mut png: Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in kind.iter().chain(data) {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    chunk.extend_from_slice(&(!crc).to_be_bytes());
    // After the signature (8) and IHDR (25).
    png.splice(33..33, chunk);
    png
}

/// The orientation a viewer would apply to the optimized file.
fn orientation_of(bytes: &[u8]) -> Option<u8> {
    let find = |needle: &[u8]| bytes.windows(needle.len()).position(|w| w == needle);
    let tiff = if bytes.starts_with(&[0xFF, 0xD8]) {
        &bytes[find(b"Exif\0\0")? + 6..]
    } else {
        &bytes[find(b"eXIf")? + 4..]
    };
    imageopt_core::metadata::display_orientation(tiff)
}

/// Browsers turn JPEG and PNG pictures by their EXIF orientation. Stripping it
/// with the other metadata would show the picture rotated.
#[test]
fn exif_orientation_survives_every_mode() {
    let modes = [
        (false, MetadataPolicy::KeepColorProfile),
        (false, MetadataPolicy::StripAll),
        (false, MetadataPolicy::KeepAll),
        (true, MetadataPolicy::StripAll),
    ];
    for (name, input) in [("jpeg", jpeg_with_exif(6)), ("png", png_with_exif(6))] {
        assert_eq!(orientation_of(&input), Some(6), "{name} fixture");
        assert!(image::load_from_memory(&input).is_ok(), "{name} fixture");
        for (lossy, metadata) in modes {
            let opts = OptimizeOptions {
                lossy,
                metadata,
                min_savings_percent: 0.0,
                ..Default::default()
            };
            let out = optimize_bytes(&input, &opts).unwrap();
            let case = format!("{name} lossy={lossy} {metadata:?}: {:?}", out.status);
            assert!(
                !matches!(out.status, OptimizeStatus::Failed { .. }),
                "{case}"
            );
            assert_eq!(orientation_of(&out.bytes), Some(6), "{case}");
            assert!(image::load_from_memory(&out.bytes).is_ok(), "{case}");
            // Only keep-all keeps the rest of the EXIF data.
            let has_make = out.bytes.windows(4).any(|w| w == b"Cam\0");
            assert_eq!(has_make, metadata == MetadataPolicy::KeepAll, "{case}");
        }
    }

    // EXIF data split over two APP1 segments: viewers read only the first,
    // which holds no readable orientation, so the file is left alone.
    let whole = jpeg_with_exif(6);
    let at = whole.windows(6).position(|w| w == b"Exif\0\0").unwrap() - 4;
    let len = u16::from_be_bytes([whole[at + 2], whole[at + 3]]) as usize;
    let tiff = &whole[at + 10..at + 2 + len];
    let (first, second) = tiff.split_at(9);
    let mut split = whole[..at].to_vec();
    for part in [first, second] {
        split.extend_from_slice(&[0xFF, 0xE1]);
        split.extend_from_slice(&(part.len() as u16 + 8).to_be_bytes());
        split.extend_from_slice(b"Exif\0\0");
        split.extend_from_slice(part);
    }
    split.extend_from_slice(&whole[at + 2 + len..]);
    let out = optimize_bytes(&split, &OptimizeOptions::default()).unwrap();
    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "{:?}",
        out.status
    );
    assert_eq!(out.bytes, split);

    // A second EXIF segment after a readable first one does not count.
    let mut second = jpeg_with_exif(1);
    let at = 4 + u16::from_be_bytes([second[4], second[5]]) as usize;
    let first_len = u16::from_be_bytes([second[at + 2], second[at + 3]]) as usize;
    let extra = jpeg_with_exif(6)[at..at + 2 + first_len].to_vec();
    let after_first = at + 2 + first_len;
    second.splice(after_first..after_first, extra);
    let out = optimize_bytes(&second, &OptimizeOptions::default()).unwrap();
    assert_eq!(out.status, OptimizeStatus::Optimized);
    assert_eq!(orientation_of(&out.bytes), None);

    // A PNG with a second eXIf chunk keeps only the orientation.
    let doubled = png_with_chunk(png_with_exif(6), b"eXIf", &exif_tiff(6));
    for metadata in [MetadataPolicy::KeepColorProfile, MetadataPolicy::StripAll] {
        let opts = OptimizeOptions {
            metadata,
            min_savings_percent: 0.0,
            ..Default::default()
        };
        let out = optimize_bytes(&doubled, &opts).unwrap();
        assert!(
            !matches!(out.status, OptimizeStatus::Failed { .. }),
            "{:?}",
            out.status
        );
        assert_eq!(png_chunk_count(&out.bytes, b"eXIf"), 1, "{metadata:?}");
        assert!(!out.bytes.windows(4).any(|w| w == b"Cam\0"), "{metadata:?}");
        assert_eq!(orientation_of(&out.bytes), Some(6), "{metadata:?}");
    }

    // EXIF data that cannot be read is not stripped: the file stays as it is.
    let mut broken = jpeg_with_exif(6);
    let tiff_at = broken.windows(6).position(|w| w == b"Exif\0\0").unwrap() + 6;
    broken[tiff_at + 7] = 0xF0; // first IFD far outside the data
    let out = optimize_bytes(&broken, &OptimizeOptions::default()).unwrap();
    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "{:?}",
        out.status
    );
    assert_eq!(out.bytes, broken);
    let keep = OptimizeOptions {
        metadata: MetadataPolicy::KeepAll,
        ..Default::default()
    };
    assert_eq!(
        optimize_bytes(&broken, &keep).unwrap().status,
        OptimizeStatus::Optimized
    );

    // Bytes that look like an EXIF segment but have no marker prefix are not
    // one (decoders skip them): no orientation may be invented.
    let mut stray = make_jpeg();
    let at = 4 + u16::from_be_bytes([stray[4], stray[5]]) as usize;
    let mut fake = vec![0xE1, 0x00, 0x22];
    fake.extend_from_slice(b"Exif\0\0");
    fake.extend_from_slice(&imageopt_core::metadata::orientation_only_tiff(6));
    stray.splice(at..at, fake);
    if let Ok(out) = optimize_bytes(&stray, &OptimizeOptions::default()) {
        if out.status == OptimizeStatus::Optimized {
            assert!(!out.bytes.windows(4).any(|w| w == b"Exif"));
        }
    }

    // Stray bytes before a real EXIF segment: the decoder skips them and
    // reads the segment, so its orientation is kept as well.
    let mut stray_then_exif = jpeg_with_exif(6);
    let at = 4 + u16::from_be_bytes([stray_then_exif[4], stray_then_exif[5]]) as usize;
    stray_then_exif.splice(at..at, [0x12, 0x34, 0x00]);
    assert!(image::load_from_memory(&stray_then_exif).is_ok());
    let out = optimize_bytes(&stray_then_exif, &OptimizeOptions::default()).unwrap();
    assert!(
        !matches!(out.status, OptimizeStatus::Failed { .. }),
        "{:?}",
        out.status
    );
    assert_eq!(orientation_of(&out.bytes), Some(6), "{:?}", out.status);

    // An upright picture needs no EXIF: it is stripped as before.
    for (name, input) in [("jpeg", jpeg_with_exif(1)), ("png", png_with_exif(1))] {
        let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();
        assert_eq!(out.status, OptimizeStatus::Optimized, "{name}");
        assert!(!out.bytes.windows(4).any(|w| w == b"Cam\0"), "{name}");
        assert!(
            !out.bytes.windows(4).any(|w| w == b"Exif" || w == b"eXIf"),
            "{name}"
        );
    }
}

/// The chunk types of a PNG, in file order.
fn png_chunk_kinds(png: &[u8]) -> Vec<[u8; 4]> {
    let mut kinds = Vec::new();
    let mut at = 8;
    while let Some(head) = png.get(at..at + 8) {
        let len = u32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
        kinds.push(head[4..8].try_into().unwrap());
        at += len + 12;
    }
    kinds
}

/// Keeping the orientation of a PNG must not cost it any chunk that
/// `--strip color` keeps otherwise (the list it uses mirrors oxipng's).
#[test]
fn exif_orientation_keeps_the_safe_chunks() {
    let mut plain = make_png();
    for (kind, data) in [
        (b"gAMA", &[0, 0, 0xB1, 0x8F][..]),
        (
            b"cHRM",
            &[
                0, 0, 0x7A, 0x26, 0, 0, 0x80, 0x84, 0, 0, 0xFA, 0, 0, 0, 0x80, 0xE8, 0, 0, 0x75,
                0x30, 0, 0, 0xEA, 0x60, 0, 0, 0x3A, 0x98, 0, 0, 0x17, 0x70,
            ][..],
        ),
        (b"sRGB", &[0][..]),
        (b"pHYs", &[0, 0, 0x0B, 0x13, 0, 0, 0x0B, 0x13, 1][..]),
        (b"tEXt", &b"Comment\0hello"[..]),
    ] {
        plain = png_with_chunk(plain, kind, data);
    }
    let rotated = png_with_chunk(plain.clone(), b"eXIf", &exif_tiff(6));
    let opts = OptimizeOptions {
        metadata: MetadataPolicy::KeepColorProfile,
        min_savings_percent: 0.0,
        ..Default::default()
    };
    let kinds = |input: &[u8]| {
        let out = optimize_bytes(input, &opts).unwrap();
        assert_eq!(out.status, OptimizeStatus::Optimized);
        let mut kinds = png_chunk_kinds(&out.bytes);
        kinds.retain(|k| k != b"eXIf");
        kinds.sort();
        kinds
    };
    let plain_kinds = kinds(&plain);
    assert!(plain_kinds.contains(b"sRGB"), "{plain_kinds:?}");
    assert!(!plain_kinds.contains(b"tEXt"), "{plain_kinds:?}");
    assert_eq!(kinds(&rotated), plain_kinds);
}

#[test]
fn corrupt_jpeg_fails_without_panicking() {
    // Valid JPEG magic bytes followed by garbage.
    let mut input = vec![0xFF, 0xD8, 0xFF, 0xE0];
    input.extend_from_slice(&[0u8; 64]);
    let out = optimize_bytes(&input, &OptimizeOptions::default());
    // Either a clean Failed error or a Skipped/AlreadyOptimal — never a panic.
    if let Ok(o) = out {
        assert_ne!(o.status, OptimizeStatus::Optimized);
    }
}

#[test]
fn dry_run_leaves_file_untouched_inplace_writes_smaller() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("img.png");
    let input = make_png();
    std::fs::write(&path, &input).unwrap();
    let original_len = std::fs::metadata(&path).unwrap().len();

    // Dry run: nothing on disk changes.
    let r = optimize_file(&path, &OptimizeOptions::default(), &OutputSink::DryRun);
    assert_eq!(r.status, OptimizeStatus::Optimized);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), original_len);

    // In place: file shrinks and remains a valid PNG.
    let r = optimize_file(
        &path,
        &OptimizeOptions::default(),
        &OutputSink::InPlace { backup: false },
    );
    assert_eq!(r.status, OptimizeStatus::Optimized);
    let new_len = std::fs::metadata(&path).unwrap().len();
    assert!(new_len < original_len, "{new_len} !< {original_len}");
    assert!(image::load_from_memory(&std::fs::read(&path).unwrap()).is_ok());
}

/// Create a file symlink. `false` when the platform refuses: Windows needs
/// Developer Mode or an elevated process for symlinks. GitHub's Windows runners
/// are elevated, so there the tests must run rather than pass by skipping.
#[cfg(any(unix, windows))]
fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let created = std::os::windows::fs::symlink_file(target, link);
    match created {
        Ok(()) => true,
        Err(e) if cfg!(windows) && std::env::var_os("GITHUB_ACTIONS").is_none() => {
            eprintln!("skipped: cannot create a symlink here ({e})");
            false
        }
        Err(e) => panic!("cannot create a symlink: {e}"),
    }
}

/// A rename over a symlink would replace the link with a regular file and
/// leave the real file unoptimized.
#[cfg(any(unix, windows))]
#[test]
fn in_place_write_follows_a_symlink_and_keeps_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    let link_dir = dir.path().join("links");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::create_dir(&link_dir).unwrap();
    let real = real_dir.join("img.png");
    let link = link_dir.join("alias.png");
    std::fs::write(&real, make_png()).unwrap();
    if !symlink_file(&real, &link) {
        return;
    }
    let original_len = std::fs::metadata(&real).unwrap().len();

    let r = optimize_file(
        &link,
        &OptimizeOptions::default(),
        &OutputSink::InPlace { backup: false },
    );

    assert_eq!(r.status, OptimizeStatus::Optimized);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link must stay a link"
    );
    let new_len = std::fs::metadata(&real).unwrap().len();
    assert!(new_len < original_len, "target must be optimized");
    assert_eq!(std::fs::read(&link).unwrap(), std::fs::read(&real).unwrap());
    // No temp file left behind in either directory.
    assert_eq!(std::fs::read_dir(&real_dir).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(&link_dir).unwrap().count(), 1);
}

/// The directory sink replaces the entry in its root. It must not follow a
/// symlink that already sits there and overwrite a file outside the root.
#[cfg(any(unix, windows))]
#[test]
fn directory_sink_does_not_write_through_an_existing_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("img.png");
    std::fs::write(&src, make_png()).unwrap();
    let outside = dir.path().join("outside.png");
    std::fs::write(&outside, b"unrelated").unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let dst = out_dir.join("img.png");
    if !symlink_file(&outside, &dst) {
        return;
    }

    let r = optimize_file(
        &src,
        &OptimizeOptions::default(),
        &OutputSink::Directory {
            root: out_dir.clone(),
        },
    );

    assert_eq!(r.status, OptimizeStatus::Optimized);
    assert_eq!(std::fs::read(&outside).unwrap(), b"unrelated");
    assert!(!std::fs::symlink_metadata(&dst)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(image::load_from_memory(&std::fs::read(&dst).unwrap()).is_ok());
}

#[test]
fn directory_sink_writes_to_dest_and_leaves_source_untouched() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("img.png");
    let input = make_png();
    std::fs::write(&src, &input).unwrap();

    let dst = out_dir.path().join("img.png");
    let r = optimize_file(
        &src,
        &OptimizeOptions::default(),
        &OutputSink::Directory {
            root: out_dir.path().to_path_buf(),
        },
    );

    assert_eq!(r.status, OptimizeStatus::Optimized);
    // Source is untouched.
    assert_eq!(std::fs::read(&src).unwrap(), input);
    // Destination holds the optimized (smaller, valid) image.
    let written = std::fs::read(&dst).unwrap();
    assert!(written.len() < input.len(), "destination must be smaller");
    assert!(image::load_from_memory(&written).is_ok());
}

#[test]
fn backup_preserves_original_and_is_not_clobbered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("img.png");
    let backup = dir.path().join("img.png.orig");
    let input = make_png();
    std::fs::write(&path, &input).unwrap();

    let r = optimize_file(
        &path,
        &OptimizeOptions::default(),
        &OutputSink::InPlace { backup: true },
    );

    assert_eq!(r.status, OptimizeStatus::Optimized);
    assert_eq!(std::fs::read(&backup).unwrap(), input);

    // A later backup run must keep the first pristine original, not replace it
    // with already-optimized contents.
    let r = optimize_file(
        &path,
        &OptimizeOptions::default(),
        &OutputSink::InPlace { backup: true },
    );

    assert_eq!(r.status, OptimizeStatus::AlreadyOptimal);
    assert_eq!(std::fs::read(&backup).unwrap(), input);
}

#[test]
fn optimize_paths_preserves_input_order_and_reports_progress() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("img.png");
    let unknown = dir.path().join("unknown.bin");
    std::fs::write(&png, make_png()).unwrap();
    std::fs::write(&unknown, b"not an image").unwrap();
    let paths = vec![unknown.clone(), png.clone()];
    let events = AtomicUsize::new(0);

    let results = optimize_paths(
        &paths,
        &OptimizeOptions::default(),
        &OutputSink::DryRun,
        |event| {
            if matches!(
                event,
                ProgressEvent::Started { .. } | ProgressEvent::Finished { .. }
            ) {
                events.fetch_add(1, Ordering::SeqCst);
            }
        },
    );

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].source.as_ref(), Some(&unknown));
    assert!(matches!(results[0].status, OptimizeStatus::Skipped { .. }));
    assert_eq!(results[1].source.as_ref(), Some(&png));
    assert_eq!(results[1].status, OptimizeStatus::Optimized);
    assert_eq!(events.load(Ordering::SeqCst), 4);
}

#[test]
fn in_flight_byte_budget_still_processes_all_files_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..5 {
        let p = dir.path().join(format!("img{i}.png"));
        std::fs::write(&p, make_png()).unwrap();
        paths.push(p);
    }

    // A budget far smaller than any single file forces fully serialized admission
    // (each file is admitted alone). This must not deadlock and must still
    // optimize every file, preserving input order.
    let opts = OptimizeOptions {
        max_in_flight_bytes: Some(1),
        ..Default::default()
    };
    let results = optimize_paths(&paths, &opts, &OutputSink::DryRun, |_| {});

    assert_eq!(results.len(), paths.len());
    for (r, p) in results.iter().zip(&paths) {
        assert_eq!(r.source.as_ref(), Some(p), "order must be preserved");
        assert_eq!(r.status, OptimizeStatus::Optimized);
    }
}

/// The budget used to be awaited on pool workers. A worker that had picked up
/// the next file while a PNG candidate evaluation was suspended below it then
/// blocked for good, and every caller sharing the pool with it. It takes
/// concurrent callers to hit the timing, as in a test binary or a service;
/// with the old code this stalls within a few rounds. A stall fails the test
/// instead of hanging the suite.
#[test]
fn in_flight_byte_budget_never_stalls_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let png = make_png();
    let paths: Vec<_> = (0..16)
        .map(|i| {
            let p = dir.path().join(format!("img{i}.png"));
            std::fs::write(&p, &png).unwrap();
            p
        })
        .collect();

    let (done, finished) = std::sync::mpsc::channel();
    let workers = 6;
    for worker in 0..workers {
        let (paths, png, done) = (paths.clone(), png.clone(), done.clone());
        std::thread::spawn(move || {
            for _ in 0..12 {
                if worker < 3 {
                    // Budgeted batches, one file at a time.
                    let opts = OptimizeOptions {
                        max_in_flight_bytes: Some(1),
                        ..Default::default()
                    };
                    let results = optimize_paths(&paths, &opts, &OutputSink::DryRun, |_| {});
                    assert_eq!(results.len(), paths.len());
                    for (r, p) in results.iter().zip(&paths) {
                        assert_eq!(r.source.as_ref(), Some(p), "order must be preserved");
                        assert_eq!(r.status, OptimizeStatus::Optimized);
                    }
                } else {
                    // Other users of the same pool.
                    for _ in 0..8 {
                        optimize_bytes(&png, &OptimizeOptions::default()).unwrap();
                    }
                }
            }
            // The receiver is gone if the test already failed.
            let _ = done.send(());
        });
    }
    drop(done);
    for _ in 0..workers {
        if finished
            .recv_timeout(std::time::Duration::from_secs(180))
            .is_err()
        {
            stalled("a budgeted PNG batch");
        }
    }
}

/// A caller that is itself a pool worker (a service that runs batches on the
/// pool, or a caller-installed pool) must not stall either: with one worker
/// there is nobody else to run the files.
#[test]
fn in_flight_byte_budget_works_when_called_from_a_pool_worker() {
    let dir = tempfile::tempdir().unwrap();
    let png = make_png();
    let paths: Vec<_> = (0..6)
        .map(|i| {
            let p = dir.path().join(format!("img{i}.png"));
            std::fs::write(&p, &png).unwrap();
            p
        })
        .collect();

    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for threads in [1, 2] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let opts = OptimizeOptions {
                max_in_flight_bytes: Some(1),
                ..Default::default()
            };
            let results =
                pool.install(|| optimize_paths(&paths, &opts, &OutputSink::DryRun, |_| {}));
            for (r, p) in results.iter().zip(&paths) {
                assert_eq!(r.source.as_ref(), Some(p), "order must be preserved");
                assert_eq!(r.status, OptimizeStatus::Optimized);
            }
        }
        let _ = done.send(());
    });
    if finished
        .recv_timeout(std::time::Duration::from_secs(180))
        .is_err()
    {
        stalled("a budgeted batch started on a pool worker");
    }
}

/// A stalled pool takes every other test in this binary with it, and a panic
/// here would not free it: report and end the process.
fn stalled(what: &str) -> ! {
    eprintln!("FAILED: {what} stalled or panicked; ending the test process");
    std::process::exit(1);
}

#[test]
fn rejects_images_over_pixel_limit() {
    let input = make_png(); // 128x128 = 16384 px
    let opts = OptimizeOptions {
        max_pixels: 1024, // far below the image
        ..Default::default()
    };
    match optimize_bytes(&input, &opts) {
        Err(Error::TooLarge { pixels, limit }) => {
            assert_eq!(pixels, 128 * 128);
            assert_eq!(limit, 1024);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn rejects_webp_over_pixel_limit_via_header_probe() {
    // The `image` crate cannot always read a VP8X header; the engine's WebP
    // fallback probe must still enforce the pixel budget before any decode.
    let input = make_huge_webp_header();
    match optimize_bytes(&input, &OptimizeOptions::default()) {
        Err(Error::TooLarge { limit, .. }) => {
            assert_eq!(limit, OptimizeOptions::default().max_pixels);
        }
        other => panic!("expected TooLarge for oversized WebP, got {other:?}"),
    }
}

#[test]
fn min_savings_threshold_gates_small_wins() {
    let input = make_png();

    // With no threshold, the image optimizes.
    let r0 = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();
    assert_eq!(r0.status, OptimizeStatus::Optimized);
    let actual_saved =
        (input.len() - r0.optimized_size as usize) as f64 / input.len() as f64 * 100.0;

    // Requiring more savings than is achievable keeps the original untouched.
    let strict = OptimizeOptions {
        min_savings_percent: actual_saved + 1.0,
        ..Default::default()
    };
    let r1 = optimize_bytes(&input, &strict).unwrap();
    assert_eq!(r1.status, OptimizeStatus::AlreadyOptimal);
    assert_eq!(r1.bytes, input, "original must be kept when gated");

    // The threshold must hold for a *smaller* candidate even with keep_larger.
    let strict_keep_larger = OptimizeOptions {
        min_savings_percent: actual_saved + 1.0,
        keep_larger: true,
        ..Default::default()
    };
    assert_eq!(
        optimize_bytes(&input, &strict_keep_larger).unwrap().status,
        OptimizeStatus::AlreadyOptimal,
        "keep_larger must not bypass the min-savings gate for smaller candidates"
    );

    // A threshold below the achievable savings still optimizes.
    let loose = OptimizeOptions {
        min_savings_percent: actual_saved - 1.0,
        ..Default::default()
    };
    assert_eq!(
        optimize_bytes(&input, &loose).unwrap().status,
        OptimizeStatus::Optimized
    );
}

#[test]
fn lossy_rebuild_only_allowed_when_stripping_all() {
    // The metadata-policy gate: lossy raster candidates that drop metadata are
    // only emitted when the policy strips everything.
    let strip_all = OptimizeOptions {
        lossy: true,
        metadata: MetadataPolicy::StripAll,
        ..Default::default()
    };
    assert!(strip_all.allow_lossy_rebuild());

    let keep_profile = OptimizeOptions {
        lossy: true,
        metadata: MetadataPolicy::KeepColorProfile,
        ..Default::default()
    };
    assert!(!keep_profile.allow_lossy_rebuild());

    let lossless = OptimizeOptions {
        lossy: false,
        metadata: MetadataPolicy::StripAll,
        ..Default::default()
    };
    assert!(!lossless.allow_lossy_rebuild());
}
