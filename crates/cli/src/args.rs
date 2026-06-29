//! Command-line argument definitions (clap).

use clap::{Parser, ValueEnum};
use imageopt_core::{MetadataPolicy, OptimizeOptions};

/// Cross-platform, all-in-one image optimizer (ImageOptim-style).
///
/// Optimizes JPEG, PNG, GIF, WebP and SVG files in place, reporting how much
/// space was saved. Lossless by default; pass --lossy for smaller files.
#[derive(Parser, Debug)]
#[command(name = "imageopt", version, about, long_about = None)]
pub struct Cli {
    /// Files, directories, or glob patterns to optimize.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<String>,

    /// Recurse into subdirectories when a directory is given.
    #[arg(short, long)]
    pub recursive: bool,

    /// Allow lossy recompression (smaller files, some quality loss).
    #[arg(long)]
    pub lossy: bool,

    /// Quality (1-100) for lossy encoders. Implies --lossy.
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub quality: Option<u8>,

    /// PNG optimization effort (0-6; 6 enables Zopfli and is slowest).
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(0..=6))]
    pub png_level: u8,

    /// Metadata handling: strip all, keep only the color profile, or keep all.
    /// Default: keep the color profile — except with --lossy, which rebuilds the
    /// image and cannot preserve metadata, so the default there is "all".
    #[arg(long, value_enum)]
    pub strip: Option<StripArg>,

    /// Show what would change without modifying any files.
    #[arg(long)]
    pub dry_run: bool,

    /// Before overwriting, copy each original to <name>.orig.
    #[arg(long)]
    pub backup: bool,

    /// CI gate: write nothing and exit non-zero if any file could be optimized.
    #[arg(long)]
    pub check: bool,

    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,

    /// Number of parallel workers (default: number of CPU cores).
    #[arg(short, long)]
    pub jobs: Option<usize>,

    /// Keep a re-encoded file even if it is larger than the original.
    #[arg(long)]
    pub keep_larger: bool,

    /// Only print the final summary (no per-file table).
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StripArg {
    /// Remove all metadata, including the ICC color profile.
    All,
    /// Strip metadata but keep the ICC color profile (default).
    Color,
    /// Keep all metadata.
    None,
}

impl Cli {
    /// Build the engine options from the parsed flags.
    pub fn to_options(&self) -> OptimizeOptions {
        // A quality value implies lossy.
        let lossy = self.lossy || self.quality.is_some();
        let metadata = match self.strip {
            Some(StripArg::All) => MetadataPolicy::StripAll,
            Some(StripArg::Color) => MetadataPolicy::KeepColorProfile,
            Some(StripArg::None) => MetadataPolicy::KeepAll,
            // Unspecified: keep the color profile by default, but lossy
            // re-encoders rebuild the image and drop metadata, so default to
            // stripping all when lossy (otherwise lossy candidates are skipped
            // to honor the policy — see OptimizeOptions::allow_lossy_rebuild).
            None if lossy => MetadataPolicy::StripAll,
            None => MetadataPolicy::KeepColorProfile,
        };
        OptimizeOptions {
            lossy,
            quality: self.quality,
            png_level: self.png_level,
            metadata,
            keep_larger: self.keep_larger,
            ..Default::default()
        }
    }

    /// Whether files may be modified on disk.
    pub fn writes_files(&self) -> bool {
        !self.check && !self.dry_run
    }
}
