use std::io::Cursor;
use std::process::Command;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use serde_json::Value;

fn imageopt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_imageopt"))
}

fn make_png() -> Vec<u8> {
    let (w, h) = (64u32, 64u32);
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let c = (((x / 8) + (y / 8)) % 4) as u8;
        *px = image::Rgba([c * 50, 240 - c * 40, c * 30, 255]);
    }

    let mut buf = Vec::new();
    PngEncoder::new_with_quality(
        Cursor::new(&mut buf),
        CompressionType::Fast,
        FilterType::NoFilter,
    )
    .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
    .expect("fixture PNG should encode");
    buf
}

#[test]
fn json_output_includes_summary_for_ci_consumers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("img.png");
    std::fs::write(&path, make_png()).expect("write fixture");

    let output = imageopt()
        .arg("--json")
        .arg("--dry-run")
        .arg(&path)
        .output()
        .expect("run imageopt");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).expect("valid JSON output");

    assert_eq!(json["summary"]["total"], 1);
    assert_eq!(json["summary"]["optimized"], 1);
    assert_eq!(json["summary"]["formats"]["png"], 1);
    assert_eq!(json["results"][0]["status"], "optimized");
    assert_eq!(json["results"][0]["format"], "png");
}

#[test]
fn check_mode_exits_nonzero_when_file_can_be_optimized() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("img.png");
    let input = make_png();
    std::fs::write(&path, &input).expect("write fixture");

    let output = imageopt()
        .arg("--check")
        .arg("--json")
        .arg(&path)
        .output()
        .expect("run imageopt");

    assert_eq!(output.status.code(), Some(1), "stdout: {}", stdout(&output));
    assert_eq!(
        std::fs::read(&path).expect("read fixture"),
        input,
        "--check must not modify files"
    );
}

#[test]
fn skipped_files_do_not_fail_check_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("unknown.bin");
    std::fs::write(&path, b"not an image").expect("write fixture");

    let output = imageopt()
        .arg("--check")
        .arg("--json")
        .arg(&path)
        .output()
        .expect("run imageopt");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).expect("valid JSON output");

    assert_eq!(json["summary"]["skipped"], 1);
    assert_eq!(json["results"][0]["status"], "skipped");
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
