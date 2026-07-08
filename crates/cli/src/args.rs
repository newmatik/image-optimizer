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

    /// Only rewrite a file if it shrinks by at least this percentage. Defaults to
    /// 0 (keep any improvement) for lossless, and 10 for --lossy so repeated
    /// runs converge instead of slowly degrading. Pass `--min-savings 0` to
    /// squeeze every byte (not recommended in a commit-back loop).
    #[arg(long, value_name = "PERCENT", value_parser = parse_percent)]
    pub min_savings: Option<f64>,

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

/// Parse and validate a 0–100 percentage for `--min-savings`.
fn parse_percent(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if (0.0..=100.0).contains(&v) {
        Ok(v)
    } else {
        Err("must be between 0 and 100".to_string())
    }
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
            // Lossy re-encoders can shave a sliver on every run; require a
            // meaningful gain (10%) by default so repeated runs converge after
            // the first pass instead of slowly degrading. Lossless is already
            // idempotent, so it keeps any improvement (0).
            min_savings_percent: self.min_savings.unwrap_or(if lossy { 10.0 } else { 0.0 }),
            ..Default::default()
        }
    }

    /// Whether files may be modified on disk.
    pub fn writes_files(&self) -> bool {
        !self.check && !self.dry_run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_with(lossy: bool, quality: Option<u8>, min_savings: Option<f64>) -> Cli {
        Cli {
            paths: vec!["assets".to_string()],
            recursive: false,
            lossy,
            quality,
            png_level: 3,
            min_savings,
            strip: None,
            dry_run: false,
            backup: false,
            check: false,
            json: false,
            jobs: None,
            keep_larger: false,
            quiet: false,
        }
    }

    #[test]
    fn lossy_defaults_to_threshold_that_converges_in_ci() {
        let opts = cli_with(true, None, None).to_options();

        assert!(opts.lossy);
        assert_eq!(opts.min_savings_percent, 10.0);
        assert_eq!(opts.metadata, MetadataPolicy::StripAll);
    }

    #[test]
    fn quality_implies_lossy_and_uses_same_convergence_threshold() {
        let opts = cli_with(false, Some(80), None).to_options();

        assert!(opts.lossy);
        assert_eq!(opts.quality, Some(80));
        assert_eq!(opts.min_savings_percent, 10.0);
    }

    #[test]
    fn explicit_min_savings_overrides_lossy_default() {
        let opts = cli_with(true, None, Some(0.0)).to_options();

        assert_eq!(opts.min_savings_percent, 0.0);
    }

    #[test]
    fn lossless_keeps_any_improvement_by_default() {
        let opts = cli_with(false, None, None).to_options();

        assert!(!opts.lossy);
        assert_eq!(opts.min_savings_percent, 0.0);
        assert_eq!(opts.metadata, MetadataPolicy::KeepColorProfile);
    }
}
