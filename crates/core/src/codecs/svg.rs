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
//! Because `usvg` normalizes the tree (dropping anything it does not model), any
//! SVG that relies on SMIL animation, scripting, event handlers, CSS
//! animations, or external/`foreignObject` content is left untouched so we
//! never silently drop animation or interactivity.
//!
//! Detection is a conservative denylist over the raw markup: it recognizes the
//! common unsafe constructs but is not a full SVG/CSS parser, so exotic ways of
//! expressing the same behavior may still slip through. Because the transform is
//! a normalize-and-reserialize (visually lossless, not byte-exact), we err on
//! the side of skipping when any of these markers are present.

use usvg::{Options, Tree, WriteOptions};

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::options::OptimizeOptions;

pub struct SvgOptimizer;

/// Element/marker substrings whose presence makes a normalize-and-reserialize
/// unsafe. Event handlers (`on...=`) and CSS animation are handled separately.
const UNSAFE_MARKERS: &[&str] = &["<animate", "<set", "<script", "<foreignobject"];

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
        if UNSAFE_MARKERS.iter().any(|m| lower.contains(m))
            || has_event_handler(&lower)
            || has_css_animation(&lower)
            || has_external_use(&lower)
        {
            return Ok(CandidateSet::Skipped {
                reason: "SVG contains scripting, event handlers, animation, or external references"
                    .to_string(),
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

/// Detect any `on<name>=` event-handler attribute (`onclick=`, `onload=`,
/// `onmouseenter=`, `onfocus=`, `onbegin=`, ...). `lower` must already be
/// lowercased. This is a superset of the handful of handlers the old denylist
/// spelled out explicitly.
fn has_event_handler(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("on") {
        let start = search_from + rel;
        search_from = start + 2;

        // Must look like the beginning of an attribute name: at the start of the
        // string or preceded by a tag/attribute boundary character.
        let boundary = start == 0
            || matches!(
                bytes[start - 1],
                b' ' | b'\t' | b'\r' | b'\n' | b'<' | b'"' | b'\'' | b'/' | b';'
            );
        if !boundary {
            continue;
        }

        // At least one ASCII-alphabetic character must follow `on`.
        let mut i = start + 2;
        let name_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        if i == name_start {
            continue;
        }

        // Optional whitespace, then `=` marks it as an attribute assignment.
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if bytes.get(i) == Some(&b'=') {
            return true;
        }
    }
    false
}

/// Detect CSS animations, which usvg cannot represent and would silently drop.
/// `lower` must already be lowercased.
fn has_css_animation(lower: &str) -> bool {
    lower.contains("@keyframes") || (lower.contains("<style") && lower.contains("animation"))
}

/// Detect a `<use>` element that pulls in an external or `data:` resource.
/// Internal references (`href="#id"`) are safe and handled by usvg. `lower` must
/// already be lowercased.
fn has_external_use(lower: &str) -> bool {
    lower.match_indices("<use").any(|(idx, _)| {
        let tag = &lower[idx..];
        let end = tag.find('>').unwrap_or(tag.len());
        let tag = &tag[..end];
        tag.contains("href") && (tag.contains("://") || tag.contains("data:"))
    })
}
