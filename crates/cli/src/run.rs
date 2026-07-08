//! Path expansion and the parallel optimization run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::Glob;
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
/// Files with a known image extension qualify, and so do *extensionless* files
/// (the engine detects format by content, so a misnamed/extensionless image is
/// still optimized). Files with a known non-image extension are skipped to avoid
/// reading every unrelated file in a tree. Explicitly named files and globs are
/// never filtered this way.
fn should_consider(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        None => true,
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
    let glob = match Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            eprintln!("imageopt: invalid glob `{pattern}`: {e}");
            return;
        }
    };
    let root = glob_root(pattern);
    for entry in WalkDir::new(&root) {
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
