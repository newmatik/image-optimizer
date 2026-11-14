//! SVG optimization: a minifier that edits the source text.
//!
//! The codec never rebuilds the image from a rendering model. It parses the
//! document only to locate things, then removes or shortens pieces of the
//! original text:
//!
//! * whitespace between elements where SVG renders no text (inside `<svg>`,
//!   `<g>`, `<defs>`, shapes, gradients, ...; never inside `<text>`,
//!   `<style>`, `<foreignObject>` or under `xml:space`);
//! * whitespace between attributes;
//! * comments (kept: `<!--! ... -->` and everything under keep-all);
//! * `<metadata>` and editor data (Inkscape, Illustrator, Sketch, ...), unless
//!   keep-all is set or the file has a stylesheet whose selectors could depend
//!   on those elements;
//! * redundant digits in numbers (`1.500` → `1.5`, `0.5` → `.5`) in path data,
//!   `points` and plain numeric attributes. With `lossy`, coordinates are also
//!   rounded to three decimals, without accumulating error along a path.
//!
//! Everything else stays byte for byte: text, links, styles, animation,
//! references, the root size and `viewBox`, unknown elements and attributes.
//!
//! Each result is checked before it is offered: the output is parsed again and
//! compared, node by node, with the source minus exactly the removals above.
//! Attribute values must be identical, or numerically equal where a number was
//! rewritten. If the comparison fails the file is reported as skipped, so a
//! mistake in the editing code cannot change an image.
//!
//! Documents that script themselves are skipped: a script can observe
//! whitespace, comments and attribute text. Documents that declare entities
//! are skipped too, because their text does not map one to one to the source.
//!
//! The XML parser recurses along element nesting and a stack overflow cannot be
//! caught, so nesting is bounded on the raw markup before anything is parsed.

use std::collections::HashSet;
use std::ops::Range;

use roxmltree::{Document, Node};

use super::{CandidateSet, Optimizer};
use crate::error::Error;
use crate::metadata::keep_all;
use crate::options::OptimizeOptions;

pub struct SvgOptimizer;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Deepest element nesting handed to the XML parser. Real artwork stays far
/// below this; a debug build on a 2 MB thread stack overflows somewhere above
/// 96 nested groups.
const MAX_STRUCTURE_DEPTH: usize = 64;

/// SVG elements that render no character data of their own. Whitespace and
/// non-rendered elements between their children are not part of the image.
/// Filter primitives (`fe*`) are matched by prefix. `<switch>` is not in the
/// list: it renders its first usable child, so removing a child changes which.
const INERT_CONTAINERS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "symbol",
    "marker",
    "mask",
    "pattern",
    "clipPath",
    "linearGradient",
    "radialGradient",
    "filter",
    "a",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "image",
    "use",
    "stop",
];

/// Namespaces of drawing programs. Their elements and attributes are never
/// rendered.
const EDITOR_NAMESPACES: &[&str] = &[
    "http://www.inkscape.org/namespaces/inkscape",
    "http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd",
    "http://www.bohemiancoding.com/sketch/ns",
    "http://www.serif.com/",
    "http://www.figma.com/figma/ns",
    "http://schemas.microsoft.com/visio/2003/SVGExtensions/",
];
const EDITOR_NAMESPACE_PREFIXES: &[&str] = &["http://ns.adobe.com/"];

/// Vocabularies used inside `<metadata>`. Their declarations are dropped when
/// nothing uses them any more.
const METADATA_NAMESPACES: &[&str] = &[
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://purl.org/dc/elements/1.1/",
    "http://purl.org/dc/terms/",
    "http://creativecommons.org/ns#",
    "http://web.resource.org/cc/",
];

/// Attributes that hold one number, optionally with a unit.
const NUMERIC_ATTRIBUTES: &[&str] = &[
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "fx",
    "fy",
    "width",
    "height",
    "offset",
    "opacity",
    "fill-opacity",
    "stroke-opacity",
    "stop-opacity",
    "flood-opacity",
    "stroke-width",
    "stroke-miterlimit",
    "stroke-dashoffset",
];

/// Decimals kept by lossy rounding, and the largest distance a rounded
/// coordinate may end up from its exact position.
const LOSSY_DECIMALS: usize = 3;
const LOSSY_TOLERANCE: f64 = 0.0005 + 1e-7;

impl Optimizer for SvgOptimizer {
    fn candidates(&self, input: &[u8], opts: &OptimizeOptions) -> Result<CandidateSet, Error> {
        let text = match std::str::from_utf8(input) {
            Ok(t) => t,
            // Not UTF-8 (e.g. gzipped .svgz): leave untouched.
            Err(_) => return Ok(skipped("SVG input is not UTF-8 and was left untouched")),
        };

        // Must come before any XML parsing: the parser itself is recursive.
        if let Err(reason) = check_raw_nesting(text) {
            return Ok(CandidateSet::Skipped { reason });
        }
        // Entity text is expanded by the parser and has no position of its own
        // in the source, so edits could not be placed reliably.
        if text.contains("<!ENTITY") {
            return Ok(skipped("SVG declares entities and was left untouched"));
        }

        let doc = parse(text).map_err(|e| Error::Decode(format!("svg: {e}")))?;
        let root = doc.root_element();
        if root.tag_name().namespace() != Some(SVG_NS) || root.tag_name().name() != "svg" {
            return Ok(skipped(
                "SVG root element is not an <svg> in the SVG namespace",
            ));
        }
        if has_scripting(&doc) {
            return Ok(skipped(
                "SVG contains scripting or event handlers, which can observe the markup",
            ));
        }

        let rules = Rules::new(&doc, text, opts);
        let Some(minified) = minify(&doc, text, &rules) else {
            return Ok(skipped("SVG markup could not be minified safely"));
        };
        if minified == text {
            return Ok(CandidateSet::Candidates(Vec::new()));
        }
        match verify(&doc, &minified, &rules) {
            Ok(()) => Ok(CandidateSet::Candidates(vec![minified.into_bytes()])),
            Err(what) => Ok(CandidateSet::Skipped {
                reason: format!("SVG minification could not be verified ({what})"),
            }),
        }
    }

    fn validate(&self, bytes: &[u8]) -> bool {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return false;
        };
        if check_raw_nesting(text).is_err() {
            return false;
        }
        parse(text).is_ok_and(|doc| {
            let root = doc.root_element();
            root.tag_name().namespace() == Some(SVG_NS) && root.tag_name().name() == "svg"
        })
    }
}

fn skipped(reason: &str) -> CandidateSet {
    CandidateSet::Skipped {
        reason: reason.to_string(),
    }
}

fn parse(text: &str) -> Result<Document<'_>, roxmltree::Error> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    Document::parse_with_options(text, options)
}

fn is_xml_whitespace(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Elements that run script or load a document that can reach into this one.
const ACTIVE_ELEMENTS: &[&str] = &["script", "iframe", "frame", "object", "embed", "handler"];

/// Anything that can run script and so observe the markup: an active element
/// in any namespace, an `on*` attribute, or a `javascript:` URL in any
/// attribute value (URL parsers drop whitespace and control characters, so
/// those are ignored here).
fn has_scripting(doc: &Document<'_>) -> bool {
    doc.descendants().filter(|n| n.is_element()).any(|node| {
        let name = node.tag_name().name();
        ACTIVE_ELEMENTS.iter().any(|e| name.eq_ignore_ascii_case(e))
            || node.attributes().any(|attr| {
                let handler = attr.name().len() > 2
                    && attr
                        .name()
                        .get(..2)
                        .is_some_and(|on| on.eq_ignore_ascii_case("on"));
                handler || {
                    let compact: String = attr
                        .value()
                        .chars()
                        .filter(|c| *c > ' ')
                        .map(|c| c.to_ascii_lowercase())
                        .collect();
                    compact.contains("javascript:")
                }
            })
    })
}

// --- what may be removed or rewritten ----------------------------------------

/// The decisions for one document. The editing code and the verification both
/// ask these, so the verification checks the editing, not the decisions.
struct Rules<'a> {
    text: &'a str,
    /// Comments may go (not under keep-all).
    strip_comments: bool,
    /// `<metadata>`, editor elements and editor attributes may go: not under
    /// keep-all, only without a stylesheet, whose selectors (`:first-child`,
    /// `+`, `[attr]`) could depend on them, and only without CSS escapes,
    /// which could hide a reference to them.
    strip_editor_data: bool,
    /// Numbers in attributes may be rewritten: not when CSS could match or
    /// read attribute text (attribute selectors, `attr()`), or cannot be read
    /// here (imports, escapes, external sheets).
    rewrite_numbers: bool,
    /// Coordinates may be rounded.
    lossy: bool,
    /// The document has a stylesheet. `:empty` then makes whitespace in an
    /// element without child elements observable.
    has_stylesheet: bool,
    /// Namespaces still used by an element or attribute after the removals.
    used_namespaces: HashSet<String>,
    /// Every name an attribute value could use to point at an element.
    reference_tokens: HashSet<String>,
    /// Elements that have a child element (only filled with a stylesheet).
    parents_of_elements: HashSet<roxmltree::NodeId>,
    root: roxmltree::NodeId,
}

impl<'a> Rules<'a> {
    fn new(doc: &Document<'_>, text: &'a str, opts: &OptimizeOptions) -> Self {
        let mut has_stylesheet = doc
            .root()
            .children()
            .any(|n| n.pi().is_some_and(|pi| pi.target == "xml-stylesheet"));
        // A stylesheet this codec cannot read: assume it matches anything.
        let mut opaque_css = has_stylesheet;
        let mut attribute_css = false;
        let mut reference_tokens = HashSet::new();
        // A CSS escape can spell an id in a way the tokens do not show
        // (`url(#\\67)` names `g`), so with a backslash anywhere in a value
        // or sheet nothing that could be referenced is removed.
        let mut escapes = false;
        for node in doc.descendants().filter(|n| n.is_element()) {
            match node.tag_name().name() {
                "style" => {
                    // The sheet is the text of all children together: a comment
                    // in the middle does not split a rule for the CSS parser.
                    let css: String = node
                        .children()
                        .filter(|c| c.is_text())
                        .filter_map(|c| c.text())
                        .collect::<String>()
                        .to_ascii_lowercase();
                    has_stylesheet |= !is_xml_whitespace(&css);
                    // Escapes can spell anything, and an import is unseen.
                    escapes |= css.contains('\\');
                    opaque_css |= css.contains('\\') || css.contains("@import");
                    attribute_css |= css.contains('[') || css.contains("attr(");
                }
                "link" => {
                    has_stylesheet = true;
                    opaque_css = true;
                }
                _ => {}
            }
            for attr in node.attributes() {
                let value = attr.value();
                if attr.name() == "style" {
                    let css = value.to_ascii_lowercase();
                    attribute_css |= css.contains("attr(") || css.contains('\\');
                }
                if attr.name() != "id" {
                    escapes |= value.contains('\\');
                    collect_reference_tokens(value, &mut reference_tokens);
                }
            }
        }
        let parents_of_elements = if has_stylesheet {
            doc.descendants()
                .filter(|n| n.is_element())
                .filter_map(|n| n.parent())
                .map(|p| p.id())
                .collect()
        } else {
            HashSet::new()
        };

        let keep_everything = keep_all(opts.metadata);
        let mut rules = Rules {
            text,
            strip_comments: !keep_everything,
            strip_editor_data: !keep_everything && !has_stylesheet && !escapes,
            rewrite_numbers: !opaque_css && !attribute_css,
            lossy: opts.lossy,
            has_stylesheet,
            used_namespaces: HashSet::new(),
            reference_tokens,
            parents_of_elements,
            root: doc.root_element().id(),
        };

        let mut used = HashSet::new();
        let mut skip_end = 0;
        for node in doc.descendants().filter(|n| n.is_element()) {
            if node.range().start < skip_end {
                continue;
            }
            if rules.drops_element(node) {
                skip_end = node.range().end;
                continue;
            }
            if let Some(ns) = node.tag_name().namespace() {
                used.insert(ns.to_string());
            }
            for attr in node.attributes() {
                if let Some(ns) = attr.namespace() {
                    if !rules.drops_attribute(Some(ns)) {
                        used.insert(ns.to_string());
                    }
                }
            }
        }
        rules.used_namespaces = used;
        rules
    }

    /// Whether whitespace and non-rendered children directly inside `parent`
    /// are outside the image: every element from `parent` up to the root is an
    /// SVG container that renders no character data, and none sets `xml:space`.
    fn inert_context(&self, parent: Node<'_, '_>) -> bool {
        parent.is_element()
            && parent.ancestors().filter(|n| n.is_element()).all(|n| {
                let name = n.tag_name().name();
                n.tag_name().namespace() == Some(SVG_NS)
                    && (INERT_CONTAINERS.contains(&name) || name.starts_with("fe"))
                    && n.attribute((XML_NS, "space")).is_none()
            })
    }

    fn drops_text(&self, node: Node<'_, '_>) -> bool {
        let Some(value) = node.text() else {
            return false;
        };
        let Some(parent) = node.parent() else {
            return false;
        };
        // The source slice must be whitespace as well: a text node that
        // continues in a CDATA section reports only its first part as range.
        let raw = self.text.get(node.range()).unwrap_or("x");
        node.is_text()
            && !value.is_empty()
            && is_xml_whitespace(value)
            && !raw.is_empty()
            && is_xml_whitespace(raw)
            && self.inert_context(parent)
            && (!self.has_stylesheet || self.parents_of_elements.contains(&parent.id()))
    }

    fn drops_comment(&self, node: Node<'_, '_>) -> bool {
        // `<!--! ... -->` marks a comment to keep (licence notices).
        self.strip_comments && node.is_comment() && !node.text().unwrap_or("").starts_with('!')
    }

    fn drops_element(&self, node: Node<'_, '_>) -> bool {
        if !self.strip_editor_data || !node.is_element() {
            return false;
        }
        let name = node.tag_name();
        let non_rendered = match name.namespace() {
            Some(SVG_NS) => name.name() == "metadata",
            Some(ns) => is_editor_namespace(ns),
            None => false,
        };
        non_rendered
            && node.parent().is_some_and(|p| self.inert_context(p))
            && !self.subtree_is_referenced(node)
            && !subtree_animates(node)
    }

    /// Whether anything in the document could point at an id inside `node`.
    fn subtree_is_referenced(&self, node: Node<'_, '_>) -> bool {
        node.descendants()
            .filter(|n| n.is_element())
            .flat_map(|n| n.attributes())
            .filter(|a| a.name() == "id")
            .any(|a| self.reference_tokens.contains(a.value().trim()))
    }

    fn drops_attribute(&self, namespace: Option<&str>) -> bool {
        self.strip_editor_data && namespace.is_some_and(is_editor_namespace)
    }

    /// Whether the declaration `xmlns:prefix="uri"` may go: an editor or
    /// metadata vocabulary that nothing uses after the removals.
    fn drops_declaration(&self, prefix: Option<&str>, uri: &str) -> bool {
        self.strip_editor_data
            && prefix.is_some()
            && (is_editor_namespace(uri) || METADATA_NAMESPACES.contains(&uri))
            && !self.used_namespaces.contains(uri)
    }

    /// How the value of `attribute` on `element` is a number, if it may be
    /// rewritten, and whether it may also be rounded. The root element is
    /// never rounded: its size and `viewBox` set the scale of everything else.
    fn number_rule(
        &self,
        element: Node<'_, '_>,
        attr_ns: Option<&str>,
        attr: &str,
    ) -> Option<NumberRule> {
        if !self.rewrite_numbers
            || attr_ns.is_some()
            || element.tag_name().namespace() != Some(SVG_NS)
        {
            return None;
        }
        let kind = match (element.tag_name().name(), attr) {
            ("path", "d") => NumberKind::Path,
            ("polygon" | "polyline", "points") => NumberKind::List,
            (_, name) if NUMERIC_ATTRIBUTES.contains(&name) => NumberKind::Single,
            _ => return None,
        };
        Some(NumberRule {
            kind,
            round: self.lossy && element.id() != self.root,
        })
    }
}

/// SVG animation elements. They act on their target wherever they are, also
/// inside `<metadata>` (`<set href="#r" attributeName="fill" to="red"/>`).
const ANIMATION_ELEMENTS: &[&str] = &[
    "animate",
    "animateColor",
    "animateMotion",
    "animateTransform",
    "discard",
    "set",
];

/// Whether `node` or an element inside it is an SVG animation element.
fn subtree_animates(node: Node<'_, '_>) -> bool {
    node.descendants().any(|n| {
        n.is_element()
            && n.tag_name().namespace() == Some(SVG_NS)
            && ANIMATION_ELEMENTS.contains(&n.tag_name().name())
    })
}

/// Collect what a decoded attribute value could name: each word, each word up
/// to a `.` (`begin="id.click"`), and everything after a `#` up to a closing
/// delimiter (`#id`, `url(#id)`, `other.svg#id`). Generous on purpose: a
/// token that is no reference only keeps an element that could have gone.
fn collect_reference_tokens(value: &str, tokens: &mut HashSet<String>) {
    // Fragments of the whole value: an id may contain `,` or `;`
    // (`href="#a,b"`). Each piece is looked at once, so this stays linear.
    for piece in value.split('#').skip(1) {
        collect_fragment(piece, &[' ', '\t', '\r', '\n'], tokens);
    }
    for word in value.split(|c: char| c.is_whitespace() || matches!(c, ';' | ',')) {
        if word.is_empty() {
            continue;
        }
        for piece in word.split('#').skip(1) {
            collect_fragment(piece, &[], tokens);
        }
        tokens.insert(word.to_string());
        if let Some((before_dot, _)) = word.split_once('.') {
            tokens.insert(before_dot.to_string());
        }
    }
}

/// The fragment at the start of `piece` (the text after a `#`), up to a
/// closing delimiter or one of `extra_ends`, as written and decoded.
fn collect_fragment(piece: &str, extra_ends: &[char], tokens: &mut HashSet<String>) {
    let end = piece
        .find(|c: char| matches!(c, ')' | '"' | '\'') || extra_ends.contains(&c))
        .unwrap_or(piece.len());
    let fragment = &piece[..end];
    // Browsers decode a URL fragment: `#%61` names `a`.
    if fragment.contains('%') {
        tokens.insert(percent_decoded(fragment));
    }
    tokens.insert(fragment.to_string());
}

/// `text` with every `%XX` escape decoded (as UTF-8, invalid bytes replaced).
fn percent_decoded(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_editor_namespace(uri: &str) -> bool {
    EDITOR_NAMESPACES.contains(&uri) || EDITOR_NAMESPACE_PREFIXES.iter().any(|p| uri.starts_with(p))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NumberRule {
    kind: NumberKind,
    /// Coordinates may be rounded to [`LOSSY_DECIMALS`].
    round: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum NumberKind {
    /// Path data.
    Path,
    /// A list of numbers (`points`).
    List,
    /// One number with an optional unit.
    Single,
}

// --- editing -----------------------------------------------------------------

/// Apply the rules to the source text. `None` when the markup does not have the
/// shape the editing code expects.
fn minify(doc: &Document<'_>, text: &str, rules: &Rules<'_>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    // Replace `range` by `with`. Nodes come in document order and every edit
    // lies at the start of its node, so ranges only move forward.
    let mut replace = |out: &mut String, range: Range<usize>, with: &str| -> Option<()> {
        if range.start < copied || range.end < range.start {
            return None;
        }
        out.push_str(text.get(copied..range.start)?);
        out.push_str(with);
        copied = range.end;
        Some(())
    };

    let mut skip_end = 0;
    for node in doc.descendants() {
        let range = node.range();
        if range.start < skip_end {
            continue;
        }
        if node.is_element() {
            if rules.drops_element(node) {
                skip_end = range.end;
                replace(&mut out, range, "")?;
                continue;
            }
            let tag = lex_start_tag(text, range.start)?;
            let rebuilt = rebuild_start_tag(node, text, &tag, rules)?;
            if rebuilt != text[range.start..tag.end] {
                replace(&mut out, range.start..tag.end, &rebuilt)?;
            }
        } else if node.is_text() {
            if rules.drops_text(node) {
                replace(&mut out, range, "")?;
            }
        } else if node.is_comment() && rules.drops_comment(node) {
            let raw = text.get(range.clone())?;
            if !raw.starts_with("<!--") || !raw.ends_with("-->") {
                return None;
            }
            replace(&mut out, range, "")?;
        }
    }
    out.push_str(text.get(copied..)?);
    Some(out)
}

struct StartTag {
    name: Range<usize>,
    attributes: Vec<RawAttribute>,
    self_closing: bool,
    /// Index just past the closing `>`.
    end: usize,
}

struct RawAttribute {
    name: Range<usize>,
    /// The value between the quotes, as written.
    value: Range<usize>,
    quote: u8,
}

/// Split the start tag at `start` into its name and attributes. The document
/// has already been parsed, so the tag is well-formed; `None` on anything
/// unexpected.
fn lex_start_tag(text: &str, start: usize) -> Option<StartTag> {
    let b = text.as_bytes();
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\r' | b'\n');
    if b.get(start) != Some(&b'<') {
        return None;
    }
    let mut i = start + 1;
    let name_start = i;
    while !matches!(*b.get(i)?, b'>' | b'/') && !is_space(b[i]) {
        i += 1;
    }
    let name = name_start..i;
    if name.is_empty() {
        return None;
    }

    let mut attributes = Vec::new();
    loop {
        while is_space(*b.get(i)?) {
            i += 1;
        }
        match b[i] {
            b'>' => {
                return Some(StartTag {
                    name,
                    attributes,
                    self_closing: false,
                    end: i + 1,
                })
            }
            b'/' => {
                if b.get(i + 1) != Some(&b'>') {
                    return None;
                }
                return Some(StartTag {
                    name,
                    attributes,
                    self_closing: true,
                    end: i + 2,
                });
            }
            _ => {}
        }
        let attr_start = i;
        while *b.get(i)? != b'=' && !is_space(b[i]) {
            if matches!(b[i], b'>' | b'/' | b'"' | b'\'' | b'<') {
                return None;
            }
            i += 1;
        }
        let attr_name = attr_start..i;
        while is_space(*b.get(i)?) {
            i += 1;
        }
        if b[i] != b'=' {
            return None;
        }
        i += 1;
        while is_space(*b.get(i)?) {
            i += 1;
        }
        let quote = b[i];
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value_start = i + 1;
        let value_len = b.get(value_start..)?.iter().position(|&c| c == quote)?;
        let value = value_start..value_start + value_len;
        i = value.end + 1;
        attributes.push(RawAttribute {
            name: attr_name,
            value,
            quote,
        });
    }
}

/// Write the start tag again from its source parts: one space between
/// attributes, dropped attributes left out, numbers shortened.
fn rebuild_start_tag(
    node: Node<'_, '_>,
    text: &str,
    tag: &StartTag,
    rules: &Rules<'_>,
) -> Option<String> {
    let mut out = String::with_capacity(tag.end - tag.name.start + 1);
    out.push('<');
    out.push_str(text.get(tag.name.clone())?);
    for attr in &tag.attributes {
        let qname = text.get(attr.name.clone())?;
        let raw = text.get(attr.value.clone())?;
        let mut value = None;

        if qname == "xmlns" || qname.starts_with("xmlns:") {
            let prefix = qname.strip_prefix("xmlns:");
            let uri = node.lookup_namespace_uri(prefix)?;
            if rules.drops_declaration(prefix, uri) {
                continue;
            }
        } else {
            let (prefix, local) = match qname.split_once(':') {
                Some((prefix, local)) => (Some(prefix), local),
                None => (None, qname),
            };
            // `xml:` is bound implicitly; every other prefix must resolve.
            let namespace = match prefix {
                Some("xml") => Some(XML_NS),
                Some(prefix) => Some(node.lookup_namespace_uri(Some(prefix))?),
                None => None,
            };
            if rules.drops_attribute(namespace) {
                continue;
            }
            // A reference in the value would not survive a rewrite as written.
            if !raw.contains('&') {
                if let Some(rule) = rules.number_rule(node, namespace, local) {
                    value = minify_number_value(rule.kind, raw, rule.round);
                }
            }
        }

        out.push(' ');
        out.push_str(qname);
        out.push('=');
        out.push(attr.quote as char);
        out.push_str(value.as_deref().unwrap_or(raw));
        out.push(attr.quote as char);
    }
    out.push_str(if tag.self_closing { "/>" } else { ">" });
    Some(out)
}

// --- numbers -----------------------------------------------------------------

/// End of the number that starts at `i`: sign, digits with an optional
/// fraction, optional exponent. `1.` and a lone sign are not numbers here, and
/// an `e` that is not followed by digits (`1em`) is left to the caller.
fn number_end(b: &[u8], mut i: usize) -> Option<usize> {
    let digits = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    };
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let int_end = digits(i);
    let has_int = int_end > i;
    i = int_end;
    if b.get(i) == Some(&b'.') {
        let frac_end = digits(i + 1);
        if frac_end == i + 1 {
            return None;
        }
        i = frac_end;
    } else if !has_int {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut k = i + 1;
        if matches!(b.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let exp_end = digits(k);
        if exp_end > k {
            i = exp_end;
        }
    }
    Some(i)
}

/// Longest run of integer or fraction digits in a number that is rewritten.
/// Up to here every reader computes the same value with and without padding
/// zeros; a parser that scales digit by digit can overflow on hundreds of
/// them, and then the padded spelling is a different (invalid) number.
const MAX_DIGITS: usize = 12;

/// Whether `number` is spelled with at most [`MAX_DIGITS`] integer and
/// fraction digits. Only such numbers are respelled.
fn is_ordinary(number: &str) -> bool {
    let digits = number.trim_start_matches(['+', '-']);
    let mantissa = digits.split(['e', 'E']).next().unwrap_or(digits);
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    int.len() <= MAX_DIGITS && frac.len() <= MAX_DIGITS
}

/// The shortest spelling of the same decimal number: no `+`, no leading or
/// trailing zeros. Numbers with an exponent are returned unchanged.
fn shorten(number: &str) -> String {
    if number.contains(['e', 'E']) {
        return number.to_string();
    }
    let (negative, digits) = match number.as_bytes().first() {
        Some(b'-') => (true, &number[1..]),
        Some(b'+') => (false, &number[1..]),
        _ => (false, number),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    if int.is_empty() && frac.is_empty() {
        return "0".to_string();
    }
    let mut out = String::with_capacity(number.len());
    if negative {
        out.push('-');
    }
    out.push_str(int);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out
}

fn parse_finite(number: &str) -> Option<f64> {
    number
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() < 1e12)
}

/// `value` rounded to [`LOSSY_DECIMALS`], as text and as the number a reader
/// gets from that text.
fn rounded(value: f64) -> Option<(String, f64)> {
    let text = shorten(&format!("{value:.LOSSY_DECIMALS$}"));
    let read = parse_finite(&text)?;
    Some((text, read))
}

/// Append `number`, separated from a preceding number where needed.
fn push_number(out: &mut String, number: &str, after_number: bool) {
    if after_number && !number.starts_with('-') {
        out.push(' ');
    }
    out.push_str(number);
}

/// The rewritten value of a numeric attribute, or `None` to keep it as it is.
fn minify_number_value(kind: NumberKind, raw: &str, lossy: bool) -> Option<String> {
    match kind {
        NumberKind::Path => {
            let segments = parse_path(raw)?;
            if !segments
                .iter()
                .all(|s| s.args.iter().all(|a| is_ordinary(a)))
            {
                return None;
            }
            let lossless = || segments.iter().map(|s| s.shortened()).collect::<Vec<_>>();
            let args = if lossy {
                round_path(&segments).unwrap_or_else(lossless)
            } else {
                lossless()
            };
            Some(write_path(&segments, &args))
        }
        NumberKind::List => {
            let numbers = parse_number_list(raw)?;
            if !numbers.iter().all(|n| is_ordinary(n)) {
                return None;
            }
            // Always a space between numbers: a sign alone may separate the
            // two coordinates of a point, but Firefox rejects it between two
            // points (`0 0-10 10`).
            let numbers: Vec<String> = numbers.iter().map(|n| round_or_shorten(n, lossy)).collect();
            Some(numbers.join(" "))
        }
        NumberKind::Single => {
            let (number, unit) = split_unit(raw)?;
            if !is_ordinary(number) {
                return None;
            }
            // Rounding in another unit would not be a rounding in user units.
            let lossy = lossy && matches!(unit, "" | "px");
            Some(format!("{}{unit}", round_or_shorten(number, lossy)))
        }
    }
}

fn round_or_shorten(number: &str, lossy: bool) -> String {
    if lossy && !number.contains(['e', 'E']) {
        if let Some((text, _)) = parse_finite(number).and_then(rounded) {
            return text;
        }
    }
    shorten(number)
}

/// A number followed by nothing or by a unit (`12.50`, `50%`, `1.0em`).
fn split_unit(value: &str) -> Option<(&str, &str)> {
    let end = number_end(value.as_bytes(), 0)?;
    let (number, unit) = value.split_at(end);
    let is_unit = unit == "%" || unit.bytes().all(|b| b.is_ascii_alphabetic());
    is_unit.then_some((number, unit))
}

/// Skip whitespace and at most one comma.
fn skip_separator(b: &[u8], mut i: usize) -> usize {
    let space = |i: usize| {
        b.get(i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
    };
    while space(i) {
        i += 1;
    }
    if b.get(i) == Some(&b',') {
        i += 1;
        while space(i) {
            i += 1;
        }
    }
    i
}

fn skip_spaces(b: &[u8], mut i: usize) -> usize {
    while b
        .get(i)
        .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
    {
        i += 1;
    }
    i
}

/// The numbers of a point list, separated by whitespace and single commas.
/// Without a separator, only a `-` may start the second number of a point
/// (`10-30`), which is all Firefox accepts. `None` for anything else,
/// including a comma after the last number: browsers reject such a list, and
/// the rewrite must not turn it into a valid one.
fn parse_number_list(value: &str) -> Option<Vec<&str>> {
    let b = value.as_bytes();
    let mut numbers = Vec::new();
    let mut i = skip_spaces(b, 0);
    let mut separated = true;
    while i < b.len() {
        let y_of_point = numbers.len() % 2 == 1 && b[i] == b'-';
        if !(separated || y_of_point) {
            return None;
        }
        let end = number_end(b, i)?;
        numbers.push(&value[i..end]);
        i = skip_separator(b, end);
        separated = i > end;
        if i >= b.len() && b[end..].contains(&b',') {
            return None;
        }
    }
    (!numbers.is_empty()).then_some(numbers)
}

/// One path command with the arguments of one repetition.
#[derive(Debug, PartialEq)]
struct Segment<'a> {
    /// The command letter in effect. Coordinates after a moveto without a new
    /// letter are linetos and are stored as such.
    command: u8,
    /// Whether the letter is written in the source.
    explicit: bool,
    /// Numbers as written. Arc flags are `0` or `1`.
    args: Vec<&'a str>,
}

impl Segment<'_> {
    fn shortened(&self) -> Vec<String> {
        self.args.iter().map(|a| shorten(a)).collect()
    }
}

/// Number of arguments per repetition of a command.
fn arity(command: u8) -> Option<usize> {
    Some(match command.to_ascii_uppercase() {
        b'Z' => 0,
        b'H' | b'V' => 1,
        b'M' | b'L' | b'T' => 2,
        b'S' | b'Q' => 4,
        b'C' => 6,
        b'A' => 7,
        _ => return None,
    })
}

/// Parse path data by the SVG grammar. `None` for anything the grammar does not
/// allow: a renderer draws such a path up to the error, so it stays as it is.
fn parse_path(data: &str) -> Option<Vec<Segment<'_>>> {
    let b = data.as_bytes();
    let mut segments = Vec::new();
    let mut i = skip_spaces(b, 0);
    // The command a bare argument group belongs to.
    let mut implicit: Option<u8> = None;
    while i < b.len() {
        let (command, explicit) = if b[i].is_ascii_alphabetic() {
            let letter = b[i];
            arity(letter)?;
            i = skip_spaces(b, i + 1);
            (letter, true)
        } else {
            (implicit?, false)
        };
        if segments.is_empty() && !command.eq_ignore_ascii_case(&b'M') {
            return None;
        }
        let count = arity(command)?;
        let arc = command.eq_ignore_ascii_case(&b'A');
        let mut args = Vec::with_capacity(count);
        for k in 0..count {
            if k > 0 {
                i = skip_separator(b, i);
            }
            if arc && (k == 3 || k == 4) {
                if !matches!(b.get(i), Some(b'0' | b'1')) {
                    return None;
                }
                args.push(&data[i..i + 1]);
                i += 1;
            } else {
                let end = number_end(b, i)?;
                args.push(&data[i..end]);
                i = end;
            }
        }
        segments.push(Segment {
            command,
            explicit,
            args,
        });
        implicit = match command {
            b'Z' | b'z' => None,
            b'M' => Some(b'L'),
            b'm' => Some(b'l'),
            other => Some(other),
        };
        // A comma may separate argument groups, not commands.
        let next = skip_separator(b, i);
        let had_comma = data[i..next].contains(',');
        i = next;
        if had_comma && (count == 0 || i >= b.len() || b[i].is_ascii_alphabetic()) {
            return None;
        }
    }
    (!segments.is_empty()).then_some(segments)
}

fn write_path(segments: &[Segment<'_>], args: &[Vec<String>]) -> String {
    let mut out = String::new();
    let mut after_number = false;
    for (segment, args) in segments.iter().zip(args) {
        if segment.explicit {
            // The stored letter of an implicit lineto is never written, so an
            // explicit segment always carries its own source letter.
            out.push(segment.command as char);
            after_number = false;
        }
        for arg in args {
            push_number(&mut out, arg, after_number);
            after_number = true;
        }
    }
    out
}

/// Walk a path and call `point(segment index, argument index, axis, exact
/// absolute coordinate, relative?)` for every coordinate; the return value is
/// the absolute coordinate as it will be read back. Tracks the current point
/// both exactly and as read back. `None` for arcs, smooth curves or
/// unreadable numbers: such paths are not rounded.
fn walk_path(
    segments: &[Segment<'_>],
    mut point: impl FnMut(usize, usize, usize, f64, f64, bool) -> Option<f64>,
) -> Option<()> {
    let mut exact = [0.0f64; 2];
    let mut read = [0.0f64; 2];
    let mut start = (exact, read);
    for (index, segment) in segments.iter().enumerate() {
        let upper = segment.command.to_ascii_uppercase();
        let relative = segment.command.is_ascii_lowercase();
        match upper {
            b'Z' => {
                (exact, read) = start;
                continue;
            }
            // An arc can move far when its end points move a little, and a
            // smooth curve mirrors the previous control point, which doubles
            // its error from one segment to the next.
            b'A' | b'S' | b'T' => return None,
            _ => {}
        }
        let (mut exact_end, mut read_end) = (exact, read);
        for (k, arg) in segment.args.iter().enumerate() {
            let axis = match upper {
                b'H' => 0,
                b'V' => 1,
                _ => k % 2,
            };
            let value = parse_finite(arg)?;
            let target = if relative { exact[axis] + value } else { value };
            // Later control points and the end point overwrite earlier ones.
            exact_end[axis] = target;
            read_end[axis] = point(index, k, axis, target, read[axis], relative)?;
        }
        exact = exact_end;
        read = read_end;
        if upper == b'M' {
            start = (exact, read);
        }
    }
    Some(())
}

/// Round every coordinate of a path to [`LOSSY_DECIMALS`]. A relative
/// coordinate is rounded against the current point as it will be read back, so
/// the error of one segment is not carried into the next. Paths with arcs or
/// smooth curves (`S`, `T`) are not rounded at all.
fn round_path(segments: &[Segment<'_>]) -> Option<Vec<Vec<String>>> {
    let mut out: Vec<Vec<String>> = segments
        .iter()
        .map(|s| Vec::with_capacity(s.args.len()))
        .collect();
    walk_path(segments, |index, _, _, target, read_origin, relative| {
        let (text, value) = rounded(if relative {
            target - read_origin
        } else {
            target
        })?;
        out[index].push(text);
        Some(if relative { read_origin + value } else { value })
    })?;
    // Renderers add relative coordinates up in single precision. Far from the
    // origin its steps are coarse, and rounded steps can then add up to a
    // different point than the original ones: keep such a path as it is.
    let source: Vec<Vec<&str>> = segments.iter().map(|s| s.args.clone()).collect();
    let before = single_precision_coordinates(segments, &source)?;
    let after = single_precision_coordinates(segments, &out)?;
    before
        .iter()
        .zip(&after)
        .all(|(&a, &b)| {
            let ulp = f64::from(a.abs().max(b.abs())) * f64::from(f32::EPSILON);
            (f64::from(a) - f64::from(b)).abs() <= LOSSY_TOLERANCE + ulp
        })
        .then_some(out)
}

/// Absolute position of every coordinate of a path with the arguments `args`,
/// added up in single precision the way renderers do. `None` for arcs and
/// smooth curves, as in [`walk_path`].
fn single_precision_coordinates<S: AsRef<str>>(
    segments: &[Segment<'_>],
    args: &[Vec<S>],
) -> Option<Vec<f32>> {
    let mut out = Vec::new();
    let mut current = [0.0f32; 2];
    let mut start = current;
    for (segment, args) in segments.iter().zip(args) {
        let upper = segment.command.to_ascii_uppercase();
        let relative = segment.command.is_ascii_lowercase();
        match upper {
            b'Z' => {
                current = start;
                continue;
            }
            b'A' | b'S' | b'T' => return None,
            _ => {}
        }
        let mut end = current;
        for (k, arg) in args.iter().enumerate() {
            let axis = match upper {
                b'H' => 0,
                b'V' => 1,
                _ => k % 2,
            };
            let value = arg.as_ref().parse::<f32>().ok().filter(|v| v.is_finite())?;
            let target = if relative {
                current[axis] + value
            } else {
                value
            };
            end[axis] = target;
            out.push(target);
        }
        current = end;
        if upper == b'M' {
            start = current;
        }
    }
    Some(out)
}

/// Absolute position of every coordinate of a path.
fn absolute_coordinates(segments: &[Segment<'_>]) -> Option<Vec<f64>> {
    let mut out = Vec::new();
    walk_path(segments, |_, _, _, target, _, _| {
        out.push(target);
        Some(target)
    })?;
    Some(out)
}

/// Whether two attribute values of the given kind describe the same numbers:
/// the same decimals exactly, or, with `lossy`, positions within the rounding
/// tolerance.
fn same_numbers(kind: NumberKind, source: &str, output: &str, lossy: bool) -> bool {
    // Identical, or two ordinary spellings of one decimal number.
    let same =
        |a: &str, b: &str| a == b || (is_ordinary(a) && is_ordinary(b) && shorten(a) == shorten(b));
    let close = |a: &str, b: &str| {
        lossy
            && matches!((parse_finite(a), parse_finite(b)),
                (Some(a), Some(b)) if (a - b).abs() <= LOSSY_TOLERANCE)
    };
    match kind {
        NumberKind::Path => {
            let (Some(a), Some(b)) = (parse_path(source), parse_path(output)) else {
                return false;
            };
            let same_shape = a.len() == b.len()
                && a.iter().zip(&b).all(|(a, b)| {
                    a.command == b.command
                        && a.explicit == b.explicit
                        && a.args.len() == b.args.len()
                });
            if !same_shape {
                return false;
            }
            let exact = a
                .iter()
                .zip(&b)
                .all(|(a, b)| a.args.iter().zip(&b.args).all(|(a, b)| same(a, b)));
            exact
                || (lossy
                    && matches!(
                        (absolute_coordinates(&a), absolute_coordinates(&b)),
                        (Some(a), Some(b)) if a.len() == b.len()
                            && a.iter().zip(&b).all(|(a, b)| (a - b).abs() <= LOSSY_TOLERANCE)
                    ))
        }
        NumberKind::List => {
            let (Some(a), Some(b)) = (parse_number_list(source), parse_number_list(output)) else {
                return false;
            };
            a.len() == b.len() && a.iter().zip(&b).all(|(a, b)| same(a, b) || close(a, b))
        }
        NumberKind::Single => {
            let (Some((a, unit_a)), Some((b, unit_b))) = (split_unit(source), split_unit(output))
            else {
                return false;
            };
            unit_a == unit_b && (same(a, b) || (matches!(unit_a, "" | "px") && close(a, b)))
        }
    }
}

// --- verification ------------------------------------------------------------

/// One node of a document, flattened. `depth` together with document order
/// fixes the tree.
#[derive(Debug, PartialEq)]
enum Event {
    Element {
        depth: usize,
        namespace: Option<String>,
        name: String,
        /// Namespace declarations written on the element.
        bindings: Vec<(Option<String>, String)>,
        attributes: Vec<AttributeEvent>,
    },
    Text {
        depth: usize,
        text: String,
    },
    Comment {
        depth: usize,
        text: String,
    },
    Instruction {
        depth: usize,
        target: String,
        value: Option<String>,
    },
}

#[derive(Debug, PartialEq)]
struct AttributeEvent {
    namespace: Option<String>,
    name: String,
    value: String,
    /// Set on source attributes whose number may have been rewritten.
    rule: Option<NumberRule>,
}

/// Flatten a document. With `rules`, leave out what the rules remove: that is
/// the document the output has to equal.
fn events(doc: &Document<'_>, rules: Option<&Rules<'_>>) -> Option<Vec<Event>> {
    let text = doc.input_text();
    let mut out: Vec<Event> = Vec::new();
    let mut skip_end = 0;
    for node in doc.descendants() {
        if node.is_root() || node.range().start < skip_end {
            continue;
        }
        let depth = node.ancestors().count();
        if node.is_element() {
            if rules.is_some_and(|r| r.drops_element(node)) {
                skip_end = node.range().end;
                continue;
            }
            // The declarations written on this element. Equal declarations on
            // every element mean equal bindings in scope everywhere, without
            // repeating the inherited ones for each element.
            let mut bindings = Vec::new();
            for attr in &lex_start_tag(text, node.range().start)?.attributes {
                let qname = text.get(attr.name.clone())?;
                let prefix = match qname.strip_prefix("xmlns") {
                    Some("") => None,
                    Some(rest) => Some(rest.strip_prefix(':')?),
                    None => continue,
                };
                let uri = node.lookup_namespace_uri(prefix)?;
                if !rules.is_some_and(|r| r.drops_declaration(prefix, uri)) {
                    bindings.push((prefix.map(str::to_string), uri.to_string()));
                }
            }
            let attributes = node
                .attributes()
                .filter(|a| !rules.is_some_and(|r| r.drops_attribute(a.namespace())))
                .map(|a| AttributeEvent {
                    namespace: a.namespace().map(str::to_string),
                    name: a.name().to_string(),
                    value: a.value().to_string(),
                    rule: rules.and_then(|r| r.number_rule(node, a.namespace(), a.name())),
                })
                .collect();
            out.push(Event::Element {
                depth,
                namespace: node.tag_name().namespace().map(str::to_string),
                name: node.tag_name().name().to_string(),
                bindings,
                attributes,
            });
        } else if node.is_text() {
            if rules.is_some_and(|r| r.drops_text(node)) {
                continue;
            }
            let text = node.text().unwrap_or("");
            // Text on both sides of a removed comment is one text node in the
            // output.
            if let Some(Event::Text {
                depth: last_depth,
                text: last,
            }) = out.last_mut()
            {
                if *last_depth == depth {
                    last.push_str(text);
                    continue;
                }
            }
            out.push(Event::Text {
                depth,
                text: text.to_string(),
            });
        } else if node.is_comment() {
            if rules.is_some_and(|r| r.drops_comment(node)) {
                continue;
            }
            out.push(Event::Comment {
                depth,
                text: node.text().unwrap_or("").to_string(),
            });
        } else if let Some(pi) = node.pi() {
            out.push(Event::Instruction {
                depth,
                target: pi.target.to_string(),
                value: pi.value.map(str::to_string),
            });
        }
    }
    Some(out)
}

/// Check that `output` is the source document minus what the rules remove, with
/// equal attribute values or equal numbers. `Err` names what differs.
fn verify(source: &Document<'_>, output: &str, rules: &Rules<'_>) -> Result<(), &'static str> {
    if check_raw_nesting(output).is_err() {
        return Err("nesting");
    }
    let parsed = parse(output).map_err(|_| "output is not well-formed")?;
    let expected = events(source, Some(rules)).ok_or("source markup")?;
    let actual = events(&parsed, None).ok_or("output markup")?;
    if expected.len() != actual.len() {
        return Err("node count");
    }
    for (expected, actual) in expected.iter().zip(&actual) {
        match (expected, actual) {
            (
                Event::Element {
                    depth,
                    namespace,
                    name,
                    bindings,
                    attributes,
                },
                Event::Element {
                    depth: out_depth,
                    namespace: out_namespace,
                    name: out_name,
                    bindings: out_bindings,
                    attributes: out_attributes,
                },
            ) => {
                if depth != out_depth || namespace != out_namespace || name != out_name {
                    return Err("element");
                }
                if bindings != out_bindings {
                    return Err("namespace declarations");
                }
                if attributes.len() != out_attributes.len() {
                    return Err("attribute count");
                }
                for (a, b) in attributes.iter().zip(out_attributes) {
                    if a.namespace != b.namespace || a.name != b.name {
                        return Err("attribute name");
                    }
                    let equal = a.value == b.value
                        || a.rule.is_some_and(|rule| {
                            same_numbers(rule.kind, &a.value, &b.value, rule.round)
                        });
                    if !equal {
                        return Err("attribute value");
                    }
                }
            }
            (expected, actual) if expected == actual => {}
            _ => return Err("text, comment or processing instruction"),
        }
    }
    Ok(())
}

// --- structure depth ---------------------------------------------------------

fn too_deep_reason() -> String {
    format!("SVG nesting is deeper than {MAX_STRUCTURE_DEPTH} levels")
}

/// Bound element nesting on the raw markup, without recursion and without an
/// XML parser. The count is an upper bound: comments, CDATA and processing
/// instructions are skipped so markup inside them cannot close real elements,
/// and quoted attribute values cannot end a tag early.
///
/// Entities whose value contains markup could open elements this scan does not
/// see, so such documents are rejected as well.
fn check_raw_nesting(text: &str) -> Result<(), String> {
    let bytes = text.as_bytes();
    // Index just past the first `needle` at or after `from`, or the end.
    let skip_past = |from: usize, needle: &str| -> usize {
        text[from..]
            .find(needle)
            .map_or(bytes.len(), |rel| from + rel + needle.len())
    };
    // Index of the `>` that ends the tag starting at `from`, honoring quotes.
    let tag_end = |from: usize| -> usize {
        let mut quote = None;
        for (offset, &b) in bytes[from..].iter().enumerate() {
            match quote {
                Some(q) if b == q => quote = None,
                Some(_) => {}
                None if b == b'"' || b == b'\'' => quote = Some(b),
                None if b == b'>' => return from + offset,
                None => {}
            }
        }
        bytes.len()
    };

    let mut depth = 0usize;
    let mut i = 0;
    while let Some(rel) = text[i..].find('<') {
        let start = i + rel;
        let rest = &text[start..];
        if rest.starts_with("<!--") {
            i = skip_past(start + 4, "-->");
        } else if rest.starts_with("<![CDATA[") {
            i = skip_past(start + 9, "]]>");
        } else if rest.starts_with("<?") {
            i = skip_past(start + 2, "?>");
        } else if rest.starts_with("<!ENTITY") {
            let end = tag_end(start + 1);
            let declaration = &text[start + 1..end];
            if declaration.contains('<') || declaration.contains("&#") {
                return Err("SVG declares an entity that contains markup".to_string());
            }
            i = end;
        } else if rest.starts_with("<!") {
            // DOCTYPE and other declarations: step inside, so entity
            // declarations in an internal subset are still inspected.
            i = start + 2;
        } else if rest.starts_with("</") {
            depth = depth.saturating_sub(1);
            i = tag_end(start + 2);
        } else {
            let end = tag_end(start + 1);
            let self_closing = end > start + 1 && bytes.get(end - 1) == Some(&b'/');
            if !self_closing {
                depth += 1;
                if depth > MAX_STRUCTURE_DEPTH {
                    return Err(too_deep_reason());
                }
            }
            i = end;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(data: &str, lossy: bool) -> Option<String> {
        minify_number_value(NumberKind::Path, data, lossy)
    }

    #[test]
    fn numbers_are_shortened_without_changing_their_value() {
        for (input, expected) in [
            ("1.500", "1.5"),
            ("0.5", ".5"),
            ("-0.50", "-.5"),
            ("+7", "7"),
            ("007", "7"),
            ("10", "10"),
            ("100.0", "100"),
            ("0", "0"),
            ("-0", "0"),
            ("-0.000", "0"),
            (".0", "0"),
            ("1e3", "1e3"),
            ("+1.50E-2", "+1.50E-2"),
        ] {
            assert_eq!(shorten(input), expected, "{input}");
        }
    }

    #[test]
    fn only_ordinary_spellings_are_rewritten() {
        assert!(is_ordinary("-000123.456000"));
        assert!(is_ordinary("+1.5e-3"));
        assert!(!is_ordinary(&format!("{}1", "0".repeat(13))));
        assert!(!is_ordinary(&format!("1.{}", "0".repeat(13))));

        // Hundreds of padding zeros overflow some number parsers: the padded
        // number is then invalid, the short one is not.
        let padded = format!("M{}10 0L20.0 20L0 20Z", "0".repeat(400));
        assert_eq!(path(&padded, false), None);
        let short = "M10 0L20 20L0 20Z";
        assert!(!same_numbers(NumberKind::Path, &padded, short, false));
        assert!(same_numbers(NumberKind::Path, &padded, &padded, false));
        let list = format!("1.{}", "0".repeat(40));
        assert_eq!(minify_number_value(NumberKind::List, &list, false), None);
        assert_eq!(minify_number_value(NumberKind::Single, &list, true), None);
    }

    #[test]
    fn scripting_is_recognized_in_every_spelling() {
        let scripted = |body: &str| {
            let text = format!(r#"<svg xmlns="{SVG_NS}" xmlns:h="urn:h">{body}</svg>"#);
            has_scripting(&parse(&text).unwrap())
        };
        assert!(scripted("<script/>"));
        assert!(scripted("<h:Script/>"));
        assert!(scripted(
            "<foreignObject><h:iframe src='a.html'/></foreignObject>"
        ));
        assert!(scripted("<g onClick='x()'/>"));
        assert!(scripted("<a href='javascript:x()'/>"));
        assert!(scripted("<a href=' Java&#10;Script&#9;:x()'/>"));
        assert!(scripted("<set attributeName='href' to='javascript:x()'/>"));
        assert!(!scripted("<g on='x' id='online'/>"));
        // Multibyte attribute names must not be sliced inside a character.
        assert!(!scripted("<g 中='1' ön='2'/>"));
    }

    #[test]
    fn reference_tokens_cover_the_ways_to_name_an_element() {
        let mut tokens = HashSet::new();
        for value in [
            "#a",
            "url(#b)",
            "fill:url('#c');stroke:url(#d)",
            "other.svg#e",
            "f.click+1s;g.end",
            "h i",
            "url(#%6B)",
            "#%C3%A9%",
            "#l,m;n",
        ] {
            collect_reference_tokens(value, &mut tokens);
        }
        for id in [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "k", "é%", "l,m;n",
        ] {
            assert!(tokens.contains(id), "{id}");
        }
        assert!(!tokens.contains("j"));
    }

    #[test]
    fn number_lexer_follows_the_svg_grammar() {
        let end = |s: &str| number_end(s.as_bytes(), 0);
        assert_eq!(end("12.5,3"), Some(4));
        assert_eq!(end("-.5.5"), Some(3));
        assert_eq!(end("1e-3x"), Some(4));
        // `e` without digits is a unit, not an exponent.
        assert_eq!(end("1em"), Some(1));
        assert_eq!(end("1e"), Some(1));
        assert_eq!(end("."), None);
        assert_eq!(end("-"), None);
        assert_eq!(end("1."), None);
        assert_eq!(end("x"), None);
    }

    #[test]
    fn path_data_is_rewritten_by_the_grammar() {
        assert_eq!(
            path(
                "M 3.000000 12.000000 L 12.000000 3.000000 L 21.0 12.50 Z",
                false
            )
            .unwrap(),
            "M3 12L12 3L21 12.5Z"
        );
        // Implicit linetos, signs as separators, exponents left alone.
        assert_eq!(
            path("m10,20 30,40-5.0-6 1e1,+2", false).unwrap(),
            "m10 20 30 40-5-6 1e1 2"
        );
        // Arc flags may be written without separators; the number after them
        // must not be read as part of a flag.
        assert_eq!(
            path("M0 0a1.0 1.0 0 01.50.5", false).unwrap(),
            "M0 0a1 1 0 0 1 .5.5".replace(".5.5", ".5 .5")
        );
        assert_eq!(
            path("M0 0A5 5 0 1 0 10.0 10.0", false).unwrap(),
            "M0 0A5 5 0 1 0 10 10"
        );
        // Closepath resets the implicit command.
        assert_eq!(path("M0 0L1 1z 2 2", false), None);
        // Not valid path data: left alone.
        for bad in [
            "",
            "L1 1",
            "M1",
            "M1 2 3",
            "M1,,2",
            "M1 2,L3 4",
            "M0 0a1 1 0 2 1 5 5",
            "M0 0 X1 1",
            "M1. 2",
            "M 1 2 garbage",
        ] {
            assert_eq!(path(bad, false), None, "{bad}");
        }
    }

    #[test]
    fn lossy_rounding_stays_within_tolerance_in_single_precision() {
        // Far from the origin, single precision drops these steps: the
        // original ends where it starts, a rounded copy would not.
        let data = format!("M100000 100000{}", "l.0039 .0039".repeat(1000));
        let out = path(&data, true).unwrap();
        assert_eq!(out, path(&data, false).unwrap());
        // Near the origin the path is still rounded.
        assert_eq!(path("M1 1l.12345 .12345", true).unwrap(), "M1 1l.123 .123");
    }

    #[test]
    fn lossy_rounding_does_not_accumulate_along_a_path() {
        // 1000 relative steps of 0.0004: each rounds to 0 on its own.
        let steps = "l.0004 .0004".repeat(1000);
        let data = format!("M0 0{steps}");
        let out = path(&data, true).unwrap();
        let end = |d: &str| {
            let coords = absolute_coordinates(&parse_path(d).unwrap()).unwrap();
            (coords[coords.len() - 2], coords[coords.len() - 1])
        };
        let (x, y) = end(&out);
        assert!((x - 0.4).abs() <= LOSSY_TOLERANCE, "{x}");
        assert!((y - 0.4).abs() <= LOSSY_TOLERANCE, "{y}");
        assert!(same_numbers(NumberKind::Path, &data, &out, true));
        assert!(!same_numbers(NumberKind::Path, &data, &out, false));

        // Lines and curves, relative and absolute, with closepath resets.
        let data = "M1.23456 2.34567l3.33333 4.44444h5.55555v-6.66666H9.87654V1.23456\
                    c1.11111 2.22222 3.33333 4.44444 5.55555 6.66666\
                    q1.11111 2.22222 3.33333 4.44444zm1.11111 1.11111L5.55555 6.66666z";
        let out = path(data, true).unwrap();
        assert!(out.len() < data.len());
        assert!(same_numbers(NumberKind::Path, data, &out, true), "{out}");
        assert_eq!(path(&out, true).unwrap(), out, "rounding is idempotent");

        // A smooth curve mirrors the previous control point, so its error
        // would double from segment to segment: such paths keep every digit.
        let smooth = "M0 0Q1 0 2 .00049T4-.00049 6 .00049 8-.00049";
        assert_eq!(path(smooth, true).unwrap(), smooth);
        let smooth = "M0 0C1 1 2 2 3.00049 3S5 5 6.00049 6";
        assert_eq!(path(smooth, true).unwrap(), smooth);

        // Arcs keep their exact numbers.
        assert_eq!(
            path("M0 0A5.12345 5.12345 0 1 0 10.12345 10.12345", true).unwrap(),
            "M0 0A5.12345 5.12345 0 1 0 10.12345 10.12345"
        );
    }

    #[test]
    fn lists_and_single_numbers_are_rewritten() {
        let list = |v: &str, lossy| minify_number_value(NumberKind::List, v, lossy);
        assert_eq!(
            list(" 0.0,10.50 20.000,-5.0 ", false).unwrap(),
            "0 10.5 20 -5"
        );
        // A sign never separates two numbers of a list.
        assert_eq!(list("0 0 -10 10 0 20", false).unwrap(), "0 0 -10 10 0 20");
        // A trailing comma makes the list invalid: it is kept as written.
        assert_eq!(list("0 0 10 10,", false), None);
        assert_eq!(list("0 0 10 10 , ", false), None);
        assert_eq!(list(",0 0", false), None);
        // Without a separator only `-` may start the y of a point.
        assert_eq!(list("10-30 5-5", false).unwrap(), "10 -30 5 -5");
        assert_eq!(list("0 0-50 50 50 0", false), None);
        assert_eq!(list("1.5.5 0", false), None);
        assert_eq!(list("1+2 0", false), None);
        assert_eq!(list("0,,0", false), None);
        assert_eq!(list("1.23456 2.5", true).unwrap(), "1.235 2.5");
        assert_eq!(list("1 2 x", false), None);
        assert_eq!(list("", false), None);

        let single = |v: &str, lossy| minify_number_value(NumberKind::Single, v, lossy);
        assert_eq!(single("1.0000000", false).unwrap(), "1");
        assert_eq!(single("50.0%", false).unwrap(), "50%");
        assert_eq!(single("2.50em", false).unwrap(), "2.5em");
        assert_eq!(single("0.12345", true).unwrap(), ".123");
        // Only user units are rounded.
        assert_eq!(single("0.12345in", true).unwrap(), ".12345in");
        assert_eq!(single("1 2", false), None);
        assert_eq!(single("auto", false), None);
        assert_eq!(single(" 1", false), None);
    }

    #[test]
    fn same_numbers_rejects_any_other_change() {
        let path = NumberKind::Path;
        assert!(same_numbers(path, "M 1.0 2.0 L 3 4", "M1 2L3 4", false));
        assert!(!same_numbers(path, "M1 2L3 4", "M1 2L3 5", false));
        assert!(!same_numbers(path, "M1 2L3 4", "M1 2l3 4", false));
        assert!(!same_numbers(path, "M1 2L3 4", "M1 2 3 4", false));
        assert!(!same_numbers(path, "M1 2L3 4", "M1 2L3 4.001", true));
        assert!(same_numbers(path, "M1 2L3 4.0004", "M1 2L3 4", true));
        // Flags are compared exactly, also when rounding is allowed.
        assert!(!same_numbers(
            path,
            "M0 0a1 1 0 0 1 5 5",
            "M0 0a1 1 0 1 1 5 5",
            true
        ));
        assert!(!same_numbers(NumberKind::Single, "5px", "5", false));
        assert!(!same_numbers(NumberKind::List, "1 2 3", "1 2", false));
    }

    #[test]
    fn start_tags_are_split_into_their_parts() {
        let text = "<svg:path  d = 'M0 0'\n\tid=\"a>b\" />rest";
        let tag = lex_start_tag(text, 0).unwrap();
        assert_eq!(&text[tag.name.clone()], "svg:path");
        assert!(tag.self_closing);
        assert_eq!(&text[tag.end..], "rest");
        let attrs: Vec<_> = tag
            .attributes
            .iter()
            .map(|a| (&text[a.name.clone()], &text[a.value.clone()], a.quote))
            .collect();
        assert_eq!(attrs, [("d", "M0 0", b'\''), ("id", "a>b", b'"')]);

        assert!(lex_start_tag("<g>", 0).is_some_and(|t| !t.self_closing && t.end == 3));
        for bad in ["g>", "<g", "<g a>", "<g a=b>", "<g a='1>", "<g /", "< >"] {
            assert!(lex_start_tag(bad, 0).is_none(), "{bad}");
        }
    }

    #[test]
    fn raw_nesting_is_bounded_without_being_fooled_by_non_markup() {
        let nested = |n: usize| format!("<svg>{}{}</svg>", "<g>".repeat(n), "</g>".repeat(n));
        assert!(check_raw_nesting(&nested(MAX_STRUCTURE_DEPTH - 1)).is_ok());
        assert!(check_raw_nesting(&nested(MAX_STRUCTURE_DEPTH)).is_err());

        // Siblings and self-closing tags do not add depth.
        let flat = format!("<svg>{}</svg>", "<g><path d='M0 0'/></g>".repeat(1_000));
        assert!(check_raw_nesting(&flat).is_ok());

        // Fake closers in comments, CDATA, PIs or attribute values must not
        // lower the count.
        for filler in [
            "<!-- </g> -->",
            "<![CDATA[ </g> ]]>",
            "<?x </g> ?>",
            "<path d='</g>'/>",
        ] {
            let hidden = format!("<svg>{}</svg>", format!("<g>{filler}").repeat(1_000));
            assert!(check_raw_nesting(&hidden).is_err(), "{filler}");
        }

        // Plain-text entities are fine, markup entities are not.
        let doctype =
            |value: &str| format!(r#"<!DOCTYPE svg [ <!ENTITY e "{value}"> ]><svg>&e;</svg>"#);
        assert!(check_raw_nesting(&doctype("http://www.w3.org/2000/svg")).is_ok());
        assert!(check_raw_nesting(&doctype("<g><g>")).is_err());
        assert!(check_raw_nesting(&doctype("&#60;g>")).is_err());
    }
}
