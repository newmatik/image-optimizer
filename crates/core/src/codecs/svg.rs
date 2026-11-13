//! SVG optimization via `usvg` (the resvg simplifier).
//!
//! `usvg` parses the SVG into a normalized tree (resolving `<use>`, collapsing
//! transforms, dropping editor cruft and unused defs) and serializes it back
//! compactly. That is a *rendering* normalization: anything usvg does not model
//! is dropped or resolved to a fixed value. An image optimizer must not do that
//! silently, so this codec only rewrites documents that consist of constructs
//! usvg reproduces, and reports everything else as `Skipped`:
//!
//! * The document is parsed and every element is checked against an allowlist
//!   (shapes, groups, gradients, patterns, clips, masks, markers, filters,
//!   static `<style>`). Text, links, `<switch>`, scripting, animation,
//!   `<foreignObject>` and anything unknown are skipped.
//! * References must be internal (`#id`) or, for images, embedded (`data:`).
//! * Context-dependent paint and CSS (`currentColor`, `var()`, at-rules,
//!   pseudo-classes) is skipped: usvg would freeze it to one value.
//! * `<title>`/`<desc>` and accessibility attributes are only dropped under the
//!   strip-all policy. Under keep-all no SVG is rewritten, because comments,
//!   editor data and unknown attributes never survive normalization.
//!
//! usvg also flattens the root `viewBox` into a transform and always writes a
//! fixed `width`/`height`. The codec restores an equivalent `viewBox` (and
//! removes a size the source did not have) so the image keeps scaling.
//!
//! The gate is conservative but not a proof of equivalence: usvg has no list of
//! everything it ignores. The output is visually lossless at the configured
//! coordinate precision (8 decimals by default, 3 with `--lossy`), not
//! byte-identical.

use usvg::{Options, Tree, WriteOptions};

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::metadata::keep_all;
use crate::options::{MetadataPolicy, OptimizeOptions};

pub struct SvgOptimizer;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Element/marker substrings whose presence makes a normalize-and-reserialize
/// unsafe. Kept in addition to the parsed-document gate as a second net for
/// markup inside CSS or entities. Event handlers (`on...=`) and CSS animation
/// are handled separately.
const UNSAFE_MARKERS: &[&str] = &["<animate", "<set", "<script", "<foreignobject"];

/// Substrings (lowercase) for paint, CSS or layout that depends on the context
/// the SVG is used in, or that usvg does not implement. usvg would freeze or
/// drop them.
const CONTEXT_MARKERS: &[&str] = &[
    "currentcolor",
    "context-fill",
    "context-stroke",
    "var(",
    "vector-effect",
    "transform-box",
];

/// SVG elements usvg reproduces. Filter primitives (`fe*`) are allowed by
/// prefix; `title`, `desc` and `metadata` are handled by the metadata policy.
const SAFE_ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "symbol",
    "use",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "image",
    "linearGradient",
    "radialGradient",
    "stop",
    "pattern",
    "clipPath",
    "mask",
    "marker",
    "filter",
    "style",
];

/// CSS properties usvg applies from a `<style>` rule or a `style` attribute.
/// Anything else (geometry such as `width` or `d`, `transform`, `color`,
/// `overflow`, shorthands) is ignored or handled differently by usvg, so it is
/// not static.
const STATIC_CSS_PROPERTIES: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-opacity",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "opacity",
    "clip-rule",
    "clip-path",
    "mask",
    "filter",
    "stop-color",
    "stop-opacity",
    "flood-color",
    "flood-opacity",
    "display",
    "visibility",
];

/// Length units that depend on a font or on the viewport of the embedding
/// page. usvg resolves them once with its own defaults.
const CONTEXT_UNITS: &[&str] = &["rem", "em", "ex", "ch", "vw", "vh", "vmin", "vmax"];

/// Attributes whose value is never a length list (and may be long).
const NON_LENGTH_ATTRIBUTES: &[&str] = &["d", "id", "class", "href", "style"];

/// Embedded raster images usvg carries over unchanged. An embedded SVG would
/// be normalized by usvg without any of the checks in this module.
const EMBEDDED_IMAGE_PREFIXES: &[&str] = &[
    "data:image/png",
    "data:image/jpeg",
    "data:image/jpg",
    "data:image/gif",
    "data:image/webp",
];

/// Filter inputs usvg replaces with `SourceGraphic`.
const UNSUPPORTED_FILTER_INPUTS: &[&str] = &[
    "backgroundimage",
    "backgroundalpha",
    "fillpaint",
    "strokepaint",
];

/// Conditional-processing attributes: usvg evaluates them once, for one
/// language and feature set.
const CONDITIONAL_ATTRIBUTES: &[&str] =
    &["systemLanguage", "requiredFeatures", "requiredExtensions"];

impl Optimizer for SvgOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        let text = match std::str::from_utf8(input) {
            Ok(t) => t,
            // Not UTF-8 (e.g. gzipped .svgz): leave untouched.
            Err(_) => return Ok(skipped("SVG input is not UTF-8 and was left untouched")),
        };

        // Normalizing drops comments, editor data, unknown attributes and
        // processing instructions. Nothing of that can be kept, so keep-all
        // means: leave SVG alone.
        if keep_all(opts.metadata) {
            return Ok(skipped(
                "SVG normalization cannot keep all metadata (keep-all policy)",
            ));
        }

        let lower = text.to_ascii_lowercase();
        if UNSAFE_MARKERS.iter().any(|m| lower.contains(m))
            || has_event_handler(&lower)
            || has_css_animation(&lower)
            || has_external_use(&lower)
        {
            return Ok(skipped(
                "SVG contains scripting, event handlers, animation, or external references",
            ));
        }
        if CONTEXT_MARKERS.iter().any(|m| lower.contains(m)) {
            return Ok(skipped(
                "SVG uses context-dependent paint or CSS (currentColor, var(), vector-effect)",
            ));
        }
        if has_external_url(&lower) {
            return Ok(skipped("SVG references an external resource via url()"));
        }

        let parse_opts = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let doc = roxmltree::Document::parse_with_options(text, parse_opts)
            .map_err(|e| Error::Decode(format!("svg: {e}")))?;
        if let Err(reason) = check_document(&doc, opts.metadata) {
            return Ok(CandidateSet::Skipped { reason });
        }
        let geometry = match root_geometry(doc.root_element()) {
            Ok(g) => g,
            Err(reason) => return Ok(CandidateSet::Skipped { reason }),
        };

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

        let normalized = tree.to_string(&write_opts);
        match restore_root(&normalized, &geometry) {
            Some(optimized) => Ok(CandidateSet::Candidates(vec![optimized.into_bytes()])),
            None => Ok(skipped(
                "SVG root size could not be carried over to the optimized output",
            )),
        }
    }

    fn validate(&self, bytes: &[u8]) -> bool {
        usvg::Tree::from_data(bytes, &Options::default()).is_ok()
    }
}

fn skipped(reason: &str) -> CandidateSet {
    CandidateSet::Skipped {
        reason: reason.to_string(),
    }
}

// --- parsed-document gate ----------------------------------------------------

/// Check every node of the document. `Err` carries the skip reason.
fn check_document(doc: &roxmltree::Document<'_>, policy: MetadataPolicy) -> Result<(), String> {
    for node in doc.root().children() {
        if node.is_pi() {
            // `<?xml-stylesheet?>` pulls in external CSS that usvg never sees.
            if node.pi().is_some_and(|pi| pi.target == "xml-stylesheet") {
                return Err("SVG uses an external stylesheet".to_string());
            }
        }
    }
    // Only foreign *descendants* are editor data. A root that is not an
    // SVG-namespace `<svg>` (for example one without `xmlns`) must not take
    // that shortcut, or nothing below it would be checked.
    let root = doc.root_element();
    if root.tag_name().namespace() != Some(SVG_NS) || root.tag_name().name() != "svg" {
        return Err("SVG root element is not an <svg> in the SVG namespace".to_string());
    }
    check_element(root, policy)
}

fn check_element(node: roxmltree::Node<'_, '_>, policy: MetadataPolicy) -> Result<(), String> {
    let name = node.tag_name().name();

    if node.tag_name().namespace() != Some(SVG_NS) {
        // Editor data (Inkscape, Illustrator, RDF): not rendered, dropped by
        // usvg. That is metadata (keep-all never gets here).
        return Ok(());
    }

    match name {
        "metadata" => return Ok(()),
        // Accessible name and description: content, not editor cruft.
        "title" | "desc" => {
            return if matches!(policy, MetadataPolicy::StripAll) {
                Ok(())
            } else {
                Err(format!(
                    "SVG has a <{name}> element, which is only removed with strip-all metadata"
                ))
            };
        }
        "style" => check_style(node)?,
        _ if SAFE_ELEMENTS.contains(&name) => {}
        _ if name.starts_with("fe") => {}
        _ => {
            return Err(format!(
                "SVG contains <{name}>, which cannot be optimized without changing it"
            ))
        }
    }

    for attr in node.attributes() {
        check_attribute(name, &attr, policy)?;
    }

    for child in node.children() {
        if child.is_element() {
            check_element(child, policy)?;
        }
    }
    Ok(())
}

fn check_attribute(
    element: &str,
    attr: &roxmltree::Attribute<'_, '_>,
    policy: MetadataPolicy,
) -> Result<(), String> {
    let name = attr.name();
    match attr.namespace() {
        None => {}
        Some(XLINK_NS) | Some(XML_NS) => {}
        // Editor attributes such as `inkscape:label`.
        Some(_) => return Ok(()),
    }

    // `xml:base` would make `#id` references resolve against another document.
    if attr.namespace() == Some(XML_NS) && name == "base" {
        return Err("SVG references an external resource".to_string());
    }

    // Same checks as on the raw markup, but on the decoded value: character
    // references and entities are expanded here, as they are for usvg.
    let decoded = attr.value().to_ascii_lowercase();
    check_decoded_value(&decoded)?;
    // `inherit` takes the value from the embedding context once it reaches the
    // root; usvg resolves it against its own defaults.
    if decoded.trim() == "inherit" {
        return Err("SVG inherits a presentation value from its context".to_string());
    }
    if name == "style" {
        if !declarations_are_static(&decoded) {
            return Err(dynamic_css_reason());
        }
    } else if !NON_LENGTH_ATTRIBUTES.contains(&name) && has_context_unit(&decoded) {
        return Err("SVG uses font- or viewport-relative lengths".to_string());
    }
    if matches!(name, "in" | "in2") && UNSUPPORTED_FILTER_INPUTS.contains(&decoded.trim()) {
        return Err("SVG filter uses an input that cannot be reproduced".to_string());
    }

    if name == "href" {
        let value = attr.value().trim_start();
        let embedded = matches!(element, "image" | "feImage")
            && EMBEDDED_IMAGE_PREFIXES
                .iter()
                .any(|prefix| decoded.trim_start().starts_with(prefix));
        if !value.starts_with('#') && !embedded {
            return Err("SVG references an external resource".to_string());
        }
    }
    if CONDITIONAL_ATTRIBUTES.contains(&name) {
        return Err("SVG uses conditional processing attributes".to_string());
    }
    let accessibility = name == "role" || name == "tabindex" || name.starts_with("aria-");
    if accessibility && !matches!(policy, MetadataPolicy::StripAll) {
        return Err(
            "SVG has accessibility attributes, which are only removed with strip-all metadata"
                .to_string(),
        );
    }
    Ok(())
}

/// Context-dependent paint and external `url()` references in a decoded,
/// lowercased attribute value or stylesheet.
fn check_decoded_value(lower: &str) -> Result<(), String> {
    if CONTEXT_MARKERS.iter().any(|m| lower.contains(m)) {
        return Err(
            "SVG uses context-dependent paint or CSS (currentColor, var(), vector-effect)"
                .to_string(),
        );
    }
    if has_external_url(lower) {
        return Err("SVG references an external resource via url()".to_string());
    }
    Ok(())
}

fn dynamic_css_reason() -> String {
    "SVG has CSS that depends on context or is not supported (at-rules, pseudo-classes, complex selectors, non-presentation properties)".to_string()
}

/// Whether a value contains a length in a font- or viewport-relative unit
/// (`2em`, `50vw`). `lower` must already be lowercased.
fn has_context_unit(lower: &str) -> bool {
    lower
        .split(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | '(' | ')' | ';' | ':'))
        .any(|token| {
            CONTEXT_UNITS.iter().any(|unit| {
                token
                    .strip_suffix(unit)
                    .is_some_and(|number| !number.is_empty() && plain_number(number).is_some())
            })
        })
}

/// Whether every `property: value` declaration is one usvg applies as written.
/// `lower` must already be lowercased.
fn declarations_are_static(lower: &str) -> bool {
    lower
        .split(';')
        .filter(|d| !d.trim().is_empty())
        .all(|declaration| match declaration.split_once(':') {
            Some((property, value)) => {
                STATIC_CSS_PROPERTIES.contains(&property.trim())
                    && value.trim() != "inherit"
                    && !has_context_unit(value)
            }
            None => false,
        })
}

/// A `<style>` element is only safe when usvg resolves all of it: plain
/// type/class/id selectors with fixed presentation declarations, for every
/// medium. usvg reads only the element's first child and only if it is text,
/// so a comment or a second text node would hide rules from it.
fn check_style(node: roxmltree::Node<'_, '_>) -> Result<(), String> {
    if node.attribute("media").is_some() {
        return Err("SVG has a media-specific <style>".to_string());
    }
    // usvg applies a typed stylesheet only for this exact spelling.
    if node.attribute("type").is_some_and(|t| t != "text/css") {
        return Err("SVG has a <style> with an unsupported type".to_string());
    }
    let mut children = node.children();
    let css = match (children.next(), children.next()) {
        (None, _) => return Ok(()),
        (Some(only), None) if only.is_text() => only.text().unwrap_or(""),
        _ => return Err("SVG has a <style> element with comments or mixed content".to_string()),
    };
    let lower = css.to_ascii_lowercase();
    check_decoded_value(&lower)?;
    if css_is_static(&lower) {
        Ok(())
    } else {
        Err(dynamic_css_reason())
    }
}

/// Whether a stylesheet contains only rules usvg applies: no at-rules, no
/// `!important`, selectors built from type, class and id selectors with
/// descendant/child combinators and grouping, and presentation declarations.
/// `css` must already be lowercased.
fn css_is_static(css: &str) -> bool {
    let css = strip_css_comments(css);
    if css.contains('@') || css.contains("!important") {
        return false;
    }
    let mut rest = css.as_str();
    loop {
        let Some(open) = rest.find('{') else {
            return rest.trim().is_empty();
        };
        let selector = &rest[..open];
        let simple = selector.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c.is_ascii_whitespace()
                || matches!(c, '_' | '-' | '.' | '#' | ',' | '>' | '*')
        });
        if !simple || selector.trim().is_empty() {
            return false;
        }
        let body = &rest[open + 1..];
        let Some(close) = body.find('}') else {
            return false;
        };
        if body[..close].contains('{') || !declarations_are_static(&body[..close]) {
            return false;
        }
        rest = &body[close + 1..];
    }
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            // Unterminated comment: keep the marker so the caller rejects it.
            None => {
                out.push('@');
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

// --- root geometry -----------------------------------------------------------

/// How the source root element sizes itself, which decides what the optimized
/// root must look like.
#[derive(Debug, PartialEq)]
enum RootGeometry {
    /// `width` and `height`, no `viewBox`: usvg's output is already equivalent.
    Fixed,
    /// `viewBox` plus `width` and `height` with the same aspect ratio.
    ViewBoxSized,
    /// `viewBox` only; the source has no intrinsic size.
    ViewBoxOnly { width: f64, height: f64 },
}

fn root_geometry(root: roxmltree::Node<'_, '_>) -> Result<RootGeometry, String> {
    // usvg does not write `overflow` back; without it the root clips again.
    if root.attribute("overflow").is_some() {
        return Err("SVG root sets overflow".to_string());
    }
    if let Some(par) = root.attribute("preserveAspectRatio") {
        let par = par.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
        if par != "xMidYMid" && par != "xMidYMid meet" {
            return Err("SVG root uses a non-default preserveAspectRatio".to_string());
        }
    }

    let size = |name: &str| -> Result<Option<f64>, String> {
        match root.attribute(name) {
            None => Ok(None),
            Some(v) => plain_length(v)
                .map(Some)
                .ok_or_else(|| format!("SVG root {name} is not a plain pixel length")),
        }
    };
    let (width, height) = (size("width")?, size("height")?);
    let view_box = match root.attribute("viewBox") {
        None => None,
        Some(v) => Some(parse_view_box(v).ok_or("SVG root viewBox is not valid")?),
    };

    match (view_box, width, height) {
        (None, Some(_), Some(_)) => Ok(RootGeometry::Fixed),
        (None, _, _) => Err("SVG root has neither a viewBox nor a width and height".to_string()),
        (Some((vw, vh)), Some(w), Some(h)) => {
            // usvg letterboxes a mismatched viewBox into the fixed size; a
            // restored viewBox would then scale differently from the source.
            if ((w / h) - (vw / vh)).abs() > (vw / vh) * 1e-6 {
                return Err("SVG root size and viewBox have different aspect ratios".to_string());
            }
            Ok(RootGeometry::ViewBoxSized)
        }
        (Some((vw, vh)), None, None) => Ok(RootGeometry::ViewBoxOnly {
            width: vw,
            height: vh,
        }),
        (Some(_), _, _) => Err("SVG root has only one of width and height".to_string()),
    }
}

/// A positive number, optionally with a `px` suffix. Percentages and other
/// units depend on the embedding context, which usvg resolves once.
fn plain_length(value: &str) -> Option<f64> {
    let v = value.trim();
    let v = v.strip_suffix("px").unwrap_or(v);
    // `f64::from_str` also accepts `inf`, `nan` and `infinity`.
    if !v
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    v.parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.0)
}

/// Width and height of a `viewBox="min-x min-y width height"` attribute.
fn parse_view_box(value: &str) -> Option<(f64, f64)> {
    let parts: Vec<f64> = value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(plain_number)
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [_, _, w, h] if *w > 0.0 && *h > 0.0 => Some((*w, *h)),
        _ => None,
    }
}

fn plain_number(value: &str) -> Option<f64> {
    if !value
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    value.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Rewrite the root element usvg produced (`<svg width="W" height="H" ...`) so
/// it sizes and scales like the source. usvg has already mapped the content
/// into a `W`×`H` user space, so `viewBox="0 0 W H"` is the equivalent box.
/// Returns `None` when the output does not have the expected shape.
fn restore_root(normalized: &str, geometry: &RootGeometry) -> Option<String> {
    let rest = normalized.strip_prefix("<svg width=\"")?;
    let (width, rest) = rest.split_once("\" height=\"")?;
    let (height, rest) = rest.split_once('"')?;
    let (w, h) = (plain_number(width)?, plain_number(height)?);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }

    match geometry {
        RootGeometry::Fixed => Some(normalized.to_string()),
        RootGeometry::ViewBoxSized => Some(format!(
            "<svg width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"{rest}"
        )),
        RootGeometry::ViewBoxOnly {
            width: vw,
            height: vh,
        } => {
            // Without a source size usvg must have used the viewBox size.
            let close = |a: f64, b: f64| (a - b).abs() <= b.abs() * 1e-6;
            if !close(w, *vw) || !close(h, *vh) {
                return None;
            }
            Some(format!("<svg viewBox=\"0 0 {width} {height}\"{rest}"))
        }
    }
}

// --- raw-markup checks -------------------------------------------------------

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

/// Detect a `url(...)` paint/filter/CSS reference that is not a same-document
/// `#id` (for example `fill="url(sprites.svg#g)"` or a CSS `url(data:...)`).
/// `lower` must already be lowercased.
fn has_external_url(lower: &str) -> bool {
    lower.match_indices("url(").any(|(idx, _)| {
        let target = lower[idx + 4..].trim_start_matches([' ', '\t', '\r', '\n', '"', '\'']);
        !target.starts_with('#')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_css_is_accepted_and_dynamic_css_rejected() {
        assert!(css_is_static(".a{fill:#fff} g > .b, #c { stroke: red }"));
        assert!(css_is_static("/* note */ rect{fill:red}"));
        assert!(css_is_static("  "));

        // Geometry and other non-presentation properties are ignored by usvg.
        assert!(!css_is_static("rect{width:10px}"));
        assert!(!css_is_static(".a{transform:scale(2)}"));
        assert!(!css_is_static(".a{stroke-width:2em}"));
        assert!(!css_is_static(".a{fill}"));
        assert!(!css_is_static(".a{fill:inherit}"));
        assert!(!css_is_static(
            "@media (prefers-color-scheme: dark){.a{fill:#000}}"
        ));
        assert!(!css_is_static(".a:hover{fill:red}"));
        assert!(!css_is_static("rect:nth-child(2){fill:red}"));
        assert!(!css_is_static("[fill]{stroke:red}"));
        assert!(!css_is_static("a + b{fill:red}"));
        assert!(!css_is_static(".a{fill:red !important}"));
        assert!(!css_is_static(".a{fill:red"));
        assert!(!css_is_static("/* open .a{fill:red}"));
    }

    #[test]
    fn context_units_are_detected_in_lengths_only() {
        assert!(has_context_unit("2em"));
        assert!(has_context_unit("0 0 1.5rem 3"));
        assert!(has_context_unit("translate(50vw,0)"));
        assert!(!has_context_unit("24"));
        assert!(!has_context_unit("12px 50%"));
        assert!(!has_context_unit("item rem them"));
    }

    #[test]
    fn lengths_and_view_boxes_parse_strictly() {
        assert_eq!(plain_length("24"), Some(24.0));
        assert_eq!(plain_length(" 24.5px "), Some(24.5));
        assert_eq!(plain_length("100%"), None);
        assert_eq!(plain_length("2em"), None);
        assert_eq!(plain_length("0"), None);
        assert_eq!(plain_length("inf"), None);

        assert_eq!(parse_view_box("0 0 24 24"), Some((24.0, 24.0)));
        assert_eq!(parse_view_box("-12,-12,24,48"), Some((24.0, 48.0)));
        assert_eq!(parse_view_box("0 0 24"), None);
        assert_eq!(parse_view_box("0 0 0 24"), None);
        assert_eq!(parse_view_box("0 0 a 24"), None);
    }

    #[test]
    fn external_url_references_are_detected() {
        assert!(!has_external_url(r#"fill="url(#g)""#));
        assert!(!has_external_url(r#"fill="url( '#g' )""#));
        assert!(has_external_url(r#"fill="url(sprites.svg#g)""#));
        assert!(has_external_url("src:url(data:font/woff2;base64,aa)"));
    }

    #[test]
    fn restore_root_rewrites_only_the_expected_shape() {
        let out = r#"<svg width="24" height="12" xmlns="http://www.w3.org/2000/svg"><path/></svg>"#;

        assert_eq!(
            restore_root(out, &RootGeometry::Fixed).as_deref(),
            Some(out)
        );
        assert_eq!(
            restore_root(out, &RootGeometry::ViewBoxSized).unwrap(),
            r#"<svg width="24" height="12" viewBox="0 0 24 12" xmlns="http://www.w3.org/2000/svg"><path/></svg>"#
        );
        let only = RootGeometry::ViewBoxOnly {
            width: 24.0,
            height: 12.0,
        };
        assert_eq!(
            restore_root(out, &only).unwrap(),
            r#"<svg viewBox="0 0 24 12" xmlns="http://www.w3.org/2000/svg"><path/></svg>"#
        );
        // usvg picked a size other than the viewBox: do not guess.
        let other = RootGeometry::ViewBoxOnly {
            width: 48.0,
            height: 24.0,
        };
        assert_eq!(restore_root(out, &other), None);
        assert_eq!(
            restore_root("<svg xmlns=\"x\"/>", &RootGeometry::Fixed),
            None
        );
    }
}
