//! SVG optimization via `usvg` (the resvg simplifier).
//!
//! `usvg` parses the SVG into a normalized tree (resolving `<use>`, collapsing
//! transforms, dropping editor cruft and unused defs) and serializes it back
//! compactly. Text is preserved as text (`preserve_text`). This is a
//! normalization rather than a byte-exact transform: by default it keeps high
//! coordinate precision (8 decimal places, usvg's maximum) so geometry is
//! visually preserved, and `--lossy` reduces precision further for smaller
//! output. (usvg does not offer unbounded precision, so this is "visually
//! lossless" rather than bit-for-bit.)
//!
//! Because `usvg` does not understand SMIL animation, scripting, or
//! `foreignObject`, any SVG using those is left untouched (we never silently
//! drop animation/interactivity).

use usvg::{Options, Tree, WriteOptions};

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::options::OptimizeOptions;

pub struct SvgOptimizer;

/// Markers whose presence makes a normalize-and-reserialize unsafe.
const UNSAFE_MARKERS: &[&str] = &[
    "<animate",
    "<set",
    "<script",
    "<foreignobject",
    "onload=",
    "onclick=",
    "onmouseover=",
];

impl Optimizer for SvgOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        let text = match std::str::from_utf8(input) {
            Ok(t) => t,
            // Not UTF-8 (e.g. gzipped .svgz): leave untouched.
            Err(_) => {
                return Ok(CandidateSet::Skipped {
                    reason: "SVG input is not UTF-8 and was left untouched".to_string(),
                });
            }
        };

        let lower = text.to_ascii_lowercase();
        if UNSAFE_MARKERS.iter().any(|m| lower.contains(m)) {
            return Ok(CandidateSet::Skipped {
                reason: "SVG contains animation, scripting, or foreignObject content".to_string(),
            });
        }

        let tree = Tree::from_data(input, &Options::default())
            .map_err(|e| Error::Decode(format!("svg: {e}")))?;

        let precision = if opts.lossy { 3 } else { 8 };
        let write_opts = WriteOptions {
            preserve_text: true,
            coordinates_precision: precision,
            transforms_precision: precision,
            indent: usvg::Indent::None,
            attributes_indent: usvg::Indent::None,
            ..Default::default()
        };

        let optimized = tree.to_string(&write_opts);
        Ok(CandidateSet::Candidates(vec![optimized.into_bytes()]))
    }

    fn validate(&self, bytes: &[u8]) -> bool {
        usvg::Tree::from_data(bytes, &Options::default()).is_ok()
    }
}
