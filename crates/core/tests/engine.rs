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

/// Optimize an SVG with the given metadata policy and return the result.
fn optimize_svg(input: &str, metadata: MetadataPolicy) -> imageopt_core::OptimizedImage {
    let opts = OptimizeOptions {
        metadata,
        ..Default::default()
    };
    optimize_bytes(input.as_bytes(), &opts).unwrap()
}

fn assert_svg_skipped(input: &str, metadata: MetadataPolicy) {
    let out = optimize_svg(input, metadata);
    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "expected a skip, got {:?} for {input}",
        out.status
    );
    assert_eq!(
        out.bytes,
        input.as_bytes(),
        "skipped SVG must be returned verbatim"
    );
}

const VERBOSE_PATH: &str =
    r#"<path d="M 3.000000 12.000000 L 12.000000 3.000000 L 21.000000 12.000000 Z" fill="red"/>"#;

#[test]
fn svg_constructs_usvg_cannot_reproduce_are_skipped() {
    let ns = r#"xmlns="http://www.w3.org/2000/svg" width="100" height="100""#;
    let cases = [
        // Text would be deleted: no fonts are loaded.
        format!(r#"<svg {ns}>{VERBOSE_PATH}<text x="10" y="50">Hello</text></svg>"#),
        // Links would be unwrapped.
        format!(r#"<svg {ns}><a href="https://example.com/">{VERBOSE_PATH}</a></svg>"#),
        // Context-dependent paint would be frozen to black.
        format!(r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="currentColor"/></svg>"#),
        format!(
            r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="var(--c, blue)"/></svg>"#
        ),
        // External content would be deleted.
        format!(
            r#"<svg {ns}>{VERBOSE_PATH}<image href="photo.png" width="10" height="10"/></svg>"#
        ),
        format!(r#"<svg {ns}>{VERBOSE_PATH}<use href="sprite.svg#a"/></svg>"#),
        format!(
            r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" fill="url(sprite.svg#g)"/></svg>"#
        ),
        // Conditional content would be resolved for one language.
        format!(r#"<svg {ns}><switch><g systemLanguage="de">{VERBOSE_PATH}</g></switch></svg>"#),
        format!(r#"<svg {ns}><g systemLanguage="de">{VERBOSE_PATH}</g></svg>"#),
        // Dynamic CSS would be resolved once.
        format!(
            r#"<svg {ns}><style>.t{{fill:#333}} @media (prefers-color-scheme: dark){{.t{{fill:#eee}}}}</style>{VERBOSE_PATH}</svg>"#
        ),
        format!(r#"<svg {ns}><style>path:hover{{fill:blue}}</style>{VERBOSE_PATH}</svg>"#),
        // A namespace prefix must not bypass the gate.
        r#"<s:svg xmlns:s="http://www.w3.org/2000/svg" width="100" height="100"><s:text x="1" y="9">Hi</s:text></s:svg>"#.to_string(),
        // Character references are decoded before usvg sees the value.
        format!(r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="current&#67;olor"/></svg>"#),
        format!(
            r#"<svg {ns}><style>.a{{fill:&#118;ar(--c, blue)}}</style><rect class="a" width="10.000" height="10.000"/></svg>"#
        ),
        // `inherit` on the way to the root takes its value from the page.
        format!(r#"<svg {ns} fill="inherit">{VERBOSE_PATH}</svg>"#),
        format!(r#"<svg {ns}><g style="stroke: inherit">{VERBOSE_PATH}</g></svg>"#),
        // usvg only applies a stylesheet typed exactly `text/css`.
        format!(
            r#"<svg {ns}><style type="TEXT/CSS">.a{{fill:#ff0000}}</style><rect class="a" width="20.000" height="10.000"/></svg>"#
        ),
        format!(r#"<svg {ns} fill="unset">{VERBOSE_PATH}</svg>"#),
        // A prefixed XHTML script runs in a browser; usvg would drop it.
        format!(
            r#"<svg {ns} xmlns:h="http://www.w3.org/1999/xhtml">{VERBOSE_PATH}<h:script>alert(1)</h:script></svg>"#
        ),
        format!(
            r#"<svg {ns} xmlns:x="urn:example:x">{VERBOSE_PATH}<x:wrap><x:Script>1</x:Script></x:wrap></svg>"#
        ),
        // Valid CSS colors usvg cannot parse would turn black.
        format!(r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="color(srgb 1 0 0)"/></svg>"#),
        // usvg ignores pathLength, which rescales the dash pattern.
        format!(
            r#"<svg {ns}><path pathLength="1" stroke="red" stroke-dasharray="0.5 0.5" d="M 0.000 50.000 L 100.000 50.000"/></svg>"#
        ),
        // A root transform sits outside the viewBox mapping.
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100" transform="translate(10 0)">{VERBOSE_PATH}</svg>"#
        ),
        // usvg applies a foreign <style> by local name before dropping it.
        format!(
            r#"<svg {ns} xmlns:x="urn:example:editor"><x:editor><x:style>path{{fill:blue}}</x:style></x:editor>{VERBOSE_PATH}</svg>"#
        ),
        format!(
            r#"<svg {ns} xmlns:x="urn:example:editor"><metadata><x:style>path{{fill:blue}}</x:style></metadata>{VERBOSE_PATH}</svg>"#
        ),
        // xml:base turns a local-looking reference into an external one.
        format!(r##"<svg {ns} xml:base="sprites.svg">{VERBOSE_PATH}<use href="#icon"/></svg>"##),
        // usvg reads only the first text child of <style>.
        format!(
            r#"<svg {ns}><style><!--x-->.a{{fill:#00ff00}}</style><rect class="a" width="20.000" height="10.000"/></svg>"#
        ),
        // CSS geometry is ignored by usvg.
        format!(r#"<svg {ns}><rect style="width:100px;height:100px;fill:red"/>{VERBOSE_PATH}</svg>"#),
        format!(r#"<svg {ns}><style>rect{{width:50px}}</style><rect height="10.000"/>{VERBOSE_PATH}</svg>"#),
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" style="width:200px;height:50px">{VERBOSE_PATH}</svg>"#
        ),
        // Font-relative lengths are frozen at usvg's default font size.
        format!(r#"<svg {ns}><rect width="2em" height="2.000em" fill="red"/>{VERBOSE_PATH}</svg>"#),
        // An embedded SVG would be normalized without any of these checks.
        format!(
            r#"<svg {ns}>{VERBOSE_PATH}<image width="64" height="16" href="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSI2NCIgaGVpZ2h0PSIxNiI+PHRleHQgeD0iMCIgeT0iMTIiPkhlbGxvPC90ZXh0Pjwvc3ZnPg=="/></svg>"#
        ),
        // Root overflow is not written back, so the root would clip again.
        format!(r#"<svg {ns} overflow="visible">{VERBOSE_PATH}</svg>"#),
        // Filter inputs usvg replaces with SourceGraphic.
        format!(
            r##"<svg {ns}><filter id="f"><feComposite in="FillPaint" in2="SourceGraphic" operator="in"/></filter><g filter="url(#f)">{VERBOSE_PATH}</g></svg>"##
        ),
        // A root without the SVG namespace must not skip the element checks.
        format!(r#"<svg width="100" height="100">{VERBOSE_PATH}<text x="1" y="9">Hi</text></svg>"#),
        // Sizes that depend on the embedding context.
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%">{VERBOSE_PATH}</svg>"#
        ),
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{VERBOSE_PATH}</svg>"#),
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 100 100">{VERBOSE_PATH}</svg>"#
        ),
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" preserveAspectRatio="none">{VERBOSE_PATH}</svg>"#
        ),
    ];
    for case in &cases {
        // Not even strip-all may rewrite these.
        assert_svg_skipped(case, MetadataPolicy::StripAll);
    }
}

/// usvg recurses along nesting and references; a stack overflow would abort the
/// whole process, so over-deep documents must be skipped before it runs.
#[test]
fn svg_with_excessive_nesting_or_reference_chains_is_skipped() {
    let open = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24">"#;
    let path = r#"<path d="M0.0000 0.0000L24.0000 24.0000"/>"#;

    // Far beyond anything a stack could take.
    let nested = format!(
        "{open}{}{path}{}</svg>",
        "<g>".repeat(100_000),
        "</g>".repeat(100_000)
    );
    assert_svg_skipped(&nested, MetadataPolicy::StripAll);

    // Flat markup, deep only through a chain of <use> references.
    let chain: String = (0..5_000)
        .map(|i| format!(r##"<g id="a{i}"><use href="#a{}"/></g>"##, i + 1))
        .collect();
    let chained = format!(r#"{open}{chain}<path id="a5000" d="M0 0L24 24"/></svg>"#);
    assert_svg_skipped(&chained, MetadataPolicy::StripAll);

    // Deep only through paint-server references.
    let patterns: String = (0..5_000)
        .map(|i| {
            format!(
                r##"<pattern id="p{i}" width="4" height="4"><rect width="4" height="4" fill="url(#p{})"/></pattern>"##,
                i + 1
            )
        })
        .collect();
    let painted = format!(
        r##"{open}<defs>{patterns}<pattern id="p5000" width="4" height="4"/></defs><rect width="24" height="24" fill="url(#p0)"/></svg>"##
    );
    assert_svg_skipped(&painted, MetadataPolicy::StripAll);

    // A reference cycle.
    let cyclic =
        format!(r##"{open}<g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g></svg>"##);
    assert_svg_skipped(&cyclic, MetadataPolicy::StripAll);
}

#[test]
fn svg_at_ordinary_depth_is_still_optimized() {
    let open = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24">"#;
    // 40 nested groups plus a short <use> chain: deeper than real artwork,
    // still inside the limit, and safe on a 2 MB test-thread stack.
    let chain: String = (0..5)
        .map(|i| format!(r##"<g id="a{i}"><use href="#a{}"/></g>"##, i + 1))
        .collect();
    let input = format!(
        r#"{open}{}{chain}<path id="a5" d="M 0.000000 0.000000 L 24.000000 24.000000" stroke="red"/>{}</svg>"#,
        "<g>".repeat(40),
        "</g>".repeat(40)
    );
    let out = optimize_svg(&input, MetadataPolicy::default());
    assert!(
        !matches!(out.status, OptimizeStatus::Skipped { .. }),
        "ordinary depth must not be skipped: {:?}",
        out.status
    );
}

#[test]
fn svg_view_box_survives_optimization() {
    // Icon style: viewBox only, no intrinsic size.
    let icon = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">{VERBOSE_PATH}</svg>"#
    );
    let out = optimize_svg(&icon, MetadataPolicy::default());
    assert_eq!(out.status, OptimizeStatus::Optimized);
    let text = String::from_utf8(out.bytes.clone()).unwrap();
    assert!(
        text.starts_with(r#"<svg viewBox="0 0 24 24" "#),
        "viewBox-only root must stay viewBox-only: {text}"
    );

    // Sized and scaled: 400×200 user units shown at 200×100.
    let sized = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="200.000" height="100.000" viewBox="0 0 400 200">{VERBOSE_PATH}{VERBOSE_PATH}{VERBOSE_PATH}</svg>"#
    );
    let out = optimize_svg(&sized, MetadataPolicy::default());
    assert_eq!(out.status, OptimizeStatus::Optimized);
    let text = String::from_utf8(out.bytes.clone()).unwrap();
    assert!(
        text.starts_with(r#"<svg width="200" height="100" viewBox="0 0 200 100" "#),
        "sized root must keep an equivalent viewBox: {text}"
    );

    // Idempotent: the restored root must not trigger another rewrite.
    let again = optimize_bytes(&out.bytes, &OptimizeOptions::default()).unwrap();
    assert_eq!(again.status, OptimizeStatus::AlreadyOptimal);
}

#[test]
fn svg_static_styles_and_internal_references_are_still_optimized() {
    let input = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">
  <style>.a{fill:#ff0000} g > .b { fill: #00ff00 }</style>
  <defs>
    <linearGradient id="g"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></linearGradient>
    <rect id="r" width="10.000000" height="10.000000"/>
  </defs>
  <rect class="a" x="0.000000" y="0.000000" width="50.000000" height="50.000000"/>
  <g><rect class="b" x="50.000000" y="50.000000" width="50.000000" height="50.000000"/></g>
  <use xlink:href="#r" x="20.000000" y="20.000000" fill="url(#g)"/>
  <g transform="translate(5.000000 5.000000) rotate(45.000000)"><rect fill="rgb(0, 0, 255)" width="5.000000" height="5.000000"/></g>
</svg>"##;
    let out = optimize_svg(input, MetadataPolicy::default());
    assert_eq!(out.status, OptimizeStatus::Optimized);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(
        text.contains("#ff0000") && text.contains("#00ff00"),
        "{text}"
    );
    assert!(text.contains("linearGradient"), "{text}");
}

#[test]
fn svg_inline_presentation_style_and_embedded_raster_are_still_optimized() {
    // A 1×1 PNG, embedded: usvg carries raster data over unchanged.
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let input = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100.000" height="100.000">
  <rect style="fill:#ff0000; stroke-width: 2.000" x="0.000000" y="0.000000" width="50.000000" height="50.000000"/>
  <rect style="fill:#00ff00" x="50.000000" y="50.000000" width="50.000000" height="50.000000"/>
  <image width="10.000000" height="10.000000" href="{png}"/>
  {VERBOSE_PATH}{VERBOSE_PATH}{VERBOSE_PATH}{VERBOSE_PATH}
</svg>"#
    );
    let out = optimize_svg(&input, MetadataPolicy::default());
    assert_eq!(out.status, OptimizeStatus::Optimized);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(
        text.contains("#ff0000") && text.contains("#00ff00"),
        "{text}"
    );
    assert!(text.contains("data:image/png"), "{text}");
}

#[test]
fn svg_descriptive_content_follows_the_metadata_policy() {
    let ns = r#"xmlns="http://www.w3.org/2000/svg" width="100" height="100""#;
    let titled = format!(r#"<svg {ns}><title>Home</title>{VERBOSE_PATH}</svg>"#);
    let labelled = format!(r#"<svg {ns} role="img" aria-label="Home">{VERBOSE_PATH}</svg>"#);
    for input in [&titled, &labelled] {
        // Accessible names are content: only strip-all removes them.
        assert_svg_skipped(input, MetadataPolicy::KeepColorProfile);
        assert_svg_skipped(input, MetadataPolicy::KeepAll);
        let out = optimize_svg(input, MetadataPolicy::StripAll);
        assert_eq!(out.status, OptimizeStatus::Optimized);
    }

    // Non-ASCII text in an attribute must never make the file fail.
    let unicode = format!(r#"<svg {ns} data-label="温度(°C)">{VERBOSE_PATH}{VERBOSE_PATH}</svg>"#);
    let out = optimize_svg(&unicode, MetadataPolicy::KeepColorProfile);
    assert!(
        !matches!(out.status, OptimizeStatus::Failed { .. }),
        "{:?}",
        out.status
    );

    // Editor data is metadata: removed by default.
    let editor = format!(
        r#"<svg {ns} xmlns:ink="http://www.inkscape.org/namespaces/inkscape" ink:version="1.0"><!-- made by hand --><metadata><ink:x/></metadata><ink:view/>{VERBOSE_PATH}</svg>"#
    );
    // Keep-all cannot be honoured by a normalizer, so nothing is rewritten:
    // not editor data, and not a file with a custom attribute either.
    assert_svg_skipped(&editor, MetadataPolicy::KeepAll);
    let plain = format!(r#"<svg {ns} custom="x" xml:lang="en">{VERBOSE_PATH}{VERBOSE_PATH}</svg>"#);
    assert_svg_skipped(&plain, MetadataPolicy::KeepAll);
    let out = optimize_svg(&editor, MetadataPolicy::KeepColorProfile);
    assert_eq!(out.status, OptimizeStatus::Optimized);
    assert!(!String::from_utf8(out.bytes).unwrap().contains("ink"));
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

/// A rename over a symlink would replace the link with a regular file and
/// leave the real file unoptimized.
#[cfg(unix)]
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
    std::os::unix::fs::symlink(&real, &link).unwrap();
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
#[cfg(unix)]
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
    std::os::unix::fs::symlink(&outside, &dst).unwrap();

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
