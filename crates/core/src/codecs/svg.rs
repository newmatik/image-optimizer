//! SVG optimization via `usvg` (the resvg simplifier).
//!
//! `usvg` parses the SVG into a normalized tree (resolving `<use>`, collapsing
//! transforms, dropping editor cruft and unused defs) and serializes it back
//! compactly. Text is preserved as text (`preserve_text`), and in lossless mode
//! full coordinate precision is kept; `--lossy` reduces precision for smaller
//! output.
//!
//! Because `usvg` does not understand SMIL animation, scripting, or
//! `foreignObject`, any SVG using those is left untouched (we never silently
//! drop animation/interactivity).

use usvg::{Options, Tree, WriteOptions};

use super::Optimizer;
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
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<Vec<Vec<u8>>, Error> {
        let text = match std::str::from_utf8(input) {
            Ok(t) => t,
            // Not UTF-8 (e.g. gzipped .svgz): leave untouched.
            Err(_) => return Ok(Vec::new()),
        };

        let lower = text.to_ascii_lowercase();
        if UNSAFE_MARKERS.iter().any(|m| lower.contains(m)) {
            return Ok(Vec::new());
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
        Ok(vec![optimized.into_bytes()])
    }
}
