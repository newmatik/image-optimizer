//! Path expansion and the parallel optimization run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use imageopt_core::{optimize_paths, ImageFormat, OptimizeResult, OutputSink, ProgressEvent};
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::WalkDir;

use crate::args::Cli;

/// Expand the user's path arguments into a concrete, de-duplicated list of files.
///
/// * A glob pattern (containing `*`, `?`, `[`) is matched against the filesystem.
/// * A directory is scanned for image files (recursively with `--recursive`).
/// * A file is included as-is.
pub fn expand_paths(inputs: &[String], recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for input in inputs {
        if has_glob(input) {
            collect_glob(input, &mut out, &mut seen);
        } else {
            let p = PathBuf::from(input);
            if p.is_dir() {
                collect_dir(&p, recursive, &mut out, &mut seen);
            } else {
                // Files (and non-existent paths, which surface as a read error).
                push_unique(p, &mut out, &mut seen);
            }
        }
    }
    out
}

fn has_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

fn push_unique(path: PathBuf, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if seen.insert(key) {
        out.push(path);
    }
}

/// Whether a file found during a directory walk should be handed to the engine.
///
/// Only files with a known image extension qualify. Extensionless names
/// (`LICENSE`, `Makefile`, `Dockerfile`) are skipped so `imageopt .` does not
/// read every non-image in the tree and report it as skipped. Explicitly named
/// files and glob matches are never filtered this way — pass a path or glob if
/// the image has no (or a non-standard) extension.
fn should_consider(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        None => false,
        Some(ext) => ImageFormat::from_extension(ext) != ImageFormat::Unknown,
    }
}

fn collect_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let max_depth = if recursive { usize::MAX } else { 1 };
    for entry in WalkDir::new(dir).max_depth(max_depth) {
        match entry {
            Ok(entry) => {
                if entry.file_type().is_file() && should_consider(entry.path()) {
                    push_unique(entry.into_path(), out, seen);
                }
            }
            Err(e) => eprintln!("imageopt: skipped walk entry: {e}"),
        }
    }
}

/// Split a glob into a literal root directory to walk and the pattern matcher.
fn collect_glob(pattern: &str, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    // `*`/`?` must not match path separators, matching shell globs and the
    // walk-depth cap (`**` is the recursive form). globset's default is the
    // opposite.
    let glob = match GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            eprintln!("imageopt: invalid glob `{pattern}`: {e}");
            return;
        }
    };
    let root = glob_root(pattern);
    let mut walker = WalkDir::new(&root);
    if let Some(depth) = glob_walk_max_depth(pattern) {
        walker = walker.max_depth(depth);
    }
    for entry in walker {
        match entry {
            Ok(entry) => {
                if entry.file_type().is_file() && glob.is_match(entry.path()) {
                    push_unique(entry.into_path(), out, seen);
                }
            }
            Err(e) => eprintln!("imageopt: skipped walk entry: {e}"),
        }
    }
}

/// The literal directory prefix of a glob: everything up to (but not including)
/// the first path segment containing a glob metacharacter.
///
/// Works on the raw string and splits on both `/` and `\\`, so absolute POSIX
/// roots (`/srv/...`) and Windows drive prefixes (`C:\...`) are preserved
/// verbatim instead of being dropped or collapsed to `.`.
fn glob_root(pattern: &str) -> PathBuf {
    let bytes = pattern.as_bytes();
    let mut seg_start = 0;
    let mut cut = pattern.len();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'/' | b'\\' => seg_start = i + 1,
            b'*' | b'?' | b'[' => {
                cut = seg_start; // the first glob segment starts here
                break;
            }
            _ => {}
        }
    }

    let prefix = &pattern[..cut];
    if prefix.is_empty() {
        return PathBuf::from(".");
    }
    let root = PathBuf::from(prefix);
    if root.is_dir() {
        root
    } else {
        // Leading literal segment is a file or doesn't exist; walk its parent.
        root.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// How deep [`WalkDir`] needs to go for `pattern`, relative to [`glob_root`].
///
/// Patterns without `**` cannot match below a fixed number of segments, so we
/// cap the walk. `*.png` in a large repo must not recurse into `target/` or
/// `.git`. `None` means unbounded (`**` is present).
fn glob_walk_max_depth(pattern: &str) -> Option<usize> {
    if pattern.contains("**") {
        return None;
    }
    let bytes = pattern.as_bytes();
    let mut seg_start = 0;
    let mut glob_seg = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'/' | b'\\' if glob_seg.is_none() => {
                seg_start = i + 1;
            }
            b'*' | b'?' | b'[' if glob_seg.is_none() => {
                glob_seg = Some(seg_start);
            }
            _ => {}
        }
    }
    let rest = &pattern[glob_seg.unwrap_or(0)..];
    let n = rest.split(['/', '\\']).filter(|s| !s.is_empty()).count();
    Some(n.max(1))
}

/// Run the optimization across `paths` with a live progress bar (unless quiet
/// or JSON output is requested).
pub fn run(
    paths: &[PathBuf],
    opts: &imageopt_core::OptimizeOptions,
    sink: &OutputSink,
    cli: &Cli,
) -> Vec<OptimizeResult> {
    let bar = if cli.json || cli.quiet {
        None
    } else {
        let b = ProgressBar::new(paths.len() as u64);
        b.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{bar:30.cyan/blue}] {pos}/{len} optimizing…",
            )
            .unwrap()
            .progress_chars("=> "),
        );
        Some(b)
    };

    let bar_ref = &bar;
    let results = optimize_paths(paths, opts, sink, |ev| {
        if let (Some(b), ProgressEvent::Finished { .. }) = (bar_ref, &ev) {
            b.inc(1);
        }
    });

    if let Some(b) = bar {
        b.finish_and_clear();
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn glob_star_does_not_match_path_separators() {
        let matcher = GlobBuilder::new("assets/*/*.png")
            .literal_separator(true)
            .build()
            .unwrap()
            .compile_matcher();
        assert!(matcher.is_match("assets/a/b.png"));
        assert!(
            !matcher.is_match("assets/a/b/c.png"),
            "* must not span directories"
        );
    }

    #[test]
    fn glob_walk_depth_is_bounded_unless_double_star() {
        assert_eq!(glob_walk_max_depth("*.png"), Some(1));
        assert_eq!(glob_walk_max_depth("assets/*.png"), Some(1));
        assert_eq!(glob_walk_max_depth("assets/*/*.png"), Some(2));
        assert_eq!(glob_walk_max_depth("foo/bar/img?.jpg"), Some(1));
        assert_eq!(glob_walk_max_depth("**/*.png"), None);
        assert_eq!(glob_walk_max_depth("assets/**/*.webp"), None);
        assert_eq!(glob_walk_max_depth(r"C:\photos\*.jpg"), Some(1));
    }

    #[test]
    fn directory_walk_skips_extensionless_and_non_image_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("LICENSE"), b"text").unwrap();
        fs::write(dir.path().join("notes.txt"), b"text").unwrap();
        fs::write(dir.path().join("logo.png"), make_png()).unwrap();

        let paths = expand_paths(&[dir.path().to_string_lossy().into_owned()], false);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "logo.png");
    }

    #[test]
    fn explicit_path_is_kept_even_without_an_image_extension() {
        let dir = tempfile::tempdir().unwrap();
        let license = dir.path().join("LICENSE");
        fs::write(&license, b"text").unwrap();

        let paths = expand_paths(&[license.to_string_lossy().into_owned()], false);
        assert_eq!(paths, vec![license]);
    }

    #[test]
    fn shallow_glob_does_not_pick_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("top.png"), make_png()).unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested").join("deep.png"), make_png()).unwrap();

        let pattern = dir.path().join("*.png").to_string_lossy().into_owned();
        let paths = expand_paths(&[pattern], false);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "top.png");
    }

    #[test]
    fn two_level_glob_does_not_let_star_match_slashes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a").join("b")).unwrap();
        fs::write(dir.path().join("a").join("mid.png"), make_png()).unwrap();
        fs::write(dir.path().join("a").join("b").join("deep.png"), make_png()).unwrap();

        let pattern = dir
            .path()
            .join("*")
            .join("*.png")
            .to_string_lossy()
            .into_owned();
        let paths = expand_paths(&[pattern], false);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "mid.png");
    }

    #[test]
    fn recursive_glob_picks_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("top.png"), make_png()).unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested").join("deep.png"), make_png()).unwrap();

        let pattern = dir
            .path()
            .join("**")
            .join("*.png")
            .to_string_lossy()
            .into_owned();
        let mut paths = expand_paths(&[pattern], false);
        paths.sort();
        assert_eq!(paths.len(), 2);
    }

    fn make_png() -> Vec<u8> {
        use std::io::Cursor;

        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        use image::{ExtendedColorType, ImageEncoder};

        let mut img = image::RgbaImage::new(8, 8);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 0, 0, 255]);
        }
        let mut buf = Vec::new();
        PngEncoder::new_with_quality(
            Cursor::new(&mut buf),
            CompressionType::Fast,
            FilterType::NoFilter,
        )
        .write_image(img.as_raw(), 8, 8, ExtendedColorType::Rgba8)
        .unwrap();
        buf
    }
}
