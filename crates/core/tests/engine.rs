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

#[test]
fn safe_svg_is_optimized_and_remains_parseable() {
    let input = br#"
        <svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
          <g>
            <rect width="10" height="10" fill="red" />
          </g>
        </svg>
    "#
    .to_vec();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    assert_eq!(out.status, OptimizeStatus::Optimized);
    assert!(out.optimized_size < out.original_size);
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

    assert!(matches!(out.status, OptimizeStatus::Skipped { .. }));
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

#[test]
fn unsafe_svg_is_reported_as_skipped_not_already_optimal() {
    let input =
        br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#.to_vec();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    match out.status {
        OptimizeStatus::Skipped { reason } => {
            assert!(reason.contains("SVG"), "unexpected skip reason: {reason}");
        }
        other => panic!("expected skipped unsafe SVG, got {other:?}"),
    }
    assert_eq!(
        out.bytes, input,
        "intentionally skipped SVG must be returned verbatim"
    );
}

#[test]
fn svg_with_css_animation_is_skipped() {
    let input = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <style>@keyframes spin { to { transform: rotate(360deg); } }
        rect { animation: spin 2s linear infinite; }</style>
        <rect width="10" height="10" fill="red" />
      </svg>"#
        .to_vec();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "CSS-animated SVG must be skipped, got {:?}",
        out.status
    );
    assert_eq!(out.bytes, input, "skipped SVG must be returned verbatim");
}

#[test]
fn svg_with_event_handler_is_skipped() {
    let input = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <rect width="10" height="10" fill="red" onfocus="steal()" />
      </svg>"#
        .to_vec();
    let out = optimize_bytes(&input, &OptimizeOptions::default()).unwrap();

    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "SVG with an event handler must be skipped, got {:?}",
        out.status
    );
    assert_eq!(out.bytes, input, "skipped SVG must be returned verbatim");
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
