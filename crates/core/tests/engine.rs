//! Integration tests for the engine's correctness guarantees.

use std::io::Cursor;

use imageopt_core::{
    optimize_bytes, optimize_file, Error, MetadataPolicy, OptimizeOptions, OptimizeStatus,
    OutputSink,
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
