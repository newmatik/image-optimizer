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

fn looks_like_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| ImageFormat::from_extension(e) != ImageFormat::Unknown)
        .unwrap_or(false)
}

fn collect_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let max_depth = if recursive { usize::MAX } else { 1 };
    for entry in WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() && looks_like_image(entry.path()) {
            push_unique(entry.into_path(), out, seen);
        }
    }
}

/// Split a glob into a literal root directory to walk and the pattern matcher.
fn collect_glob(pattern: &str, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let glob = match Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(_) => return,
    };
    let root = glob_root(pattern);
    for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file() && glob.is_match(entry.path()) {
            push_unique(entry.into_path(), out, seen);
        }
    }
}

/// The longest leading run of path components that contain no glob metacharacters.
fn glob_root(pattern: &str) -> PathBuf {
    let mut root = PathBuf::new();
    for component in pattern.split('/') {
        if has_glob(component) {
            break;
        }
        root.push(component);
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else if root.is_dir() {
        root
    } else {
        // Leading component was a file or doesn't exist; walk its parent.
        root.parent()
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
