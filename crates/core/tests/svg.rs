//! SVG minifier: what it removes, what it must leave alone, and that the
//! result is the same image.
//!
//! Two checks are independent of the codec's own verification: `signature`
//! reads source and output with a plain XML walk, and `geometry` reads them
//! with usvg, a separate SVG implementation.

#![cfg(feature = "svg")]

use imageopt_core::{optimize_bytes, MetadataPolicy, OptimizeOptions, OptimizeStatus};

const SVG: &str = "http://www.w3.org/2000/svg";

fn options(metadata: MetadataPolicy, lossy: bool) -> OptimizeOptions {
    OptimizeOptions {
        metadata,
        lossy,
        ..Default::default()
    }
}

fn optimize(input: &str, metadata: MetadataPolicy) -> imageopt_core::OptimizedImage {
    optimize_bytes(input.as_bytes(), &options(metadata, false)).unwrap()
}

/// Optimize with the default policy and return the output text.
fn minified(input: &str) -> String {
    let out = optimize(input, MetadataPolicy::default());
    assert_eq!(out.status, OptimizeStatus::Optimized, "{input}");
    String::from_utf8(out.bytes).unwrap()
}

fn assert_skipped(input: &str, metadata: MetadataPolicy) {
    let out = optimize(input, metadata);
    assert!(
        matches!(out.status, OptimizeStatus::Skipped { .. }),
        "expected a skip, got {:?} for {input}",
        out.status
    );
    assert_eq!(out.bytes, input.as_bytes(), "a skip returns the input");
}

/// Attributes whose numbers the minifier may respell.
const NUMERIC: &[&str] = &[
    "d",
    "points",
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
    "width",
    "height",
    "offset",
    "opacity",
    "stroke-width",
];

/// Elements, attributes and text of a document, without comments and without
/// whitespace directly inside `<svg>`, `<g>` and `<defs>`.
fn signature(svg: &str) -> Vec<String> {
    let doc = roxmltree::Document::parse_with_options(
        svg,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut out = Vec::new();
    for node in doc.descendants() {
        let depth = node.ancestors().count();
        if node.is_element() {
            let mut line = format!("{depth} <{:?}", node.tag_name());
            for attr in node.attributes() {
                let value = if attr.namespace().is_none() && NUMERIC.contains(&attr.name()) {
                    match attr.value().parse::<f64>() {
                        Ok(number) => number.to_string(),
                        Err(_) => "(numbers)".to_string(),
                    }
                } else {
                    attr.value().to_string()
                };
                line.push_str(&format!(
                    " {:?}:{}={value:?}",
                    attr.namespace(),
                    attr.name()
                ));
            }
            out.push(line);
        } else if node.is_text() {
            let text = node.text().unwrap();
            let parent = node.parent().unwrap().tag_name().name().to_string();
            let layout = text.trim().is_empty() && matches!(parent.as_str(), "svg" | "g" | "defs");
            if !layout {
                out.push(format!("{depth} text {text:?}"));
            }
        } else if let Some(pi) = node.pi() {
            out.push(format!("{depth} pi {} {:?}", pi.target, pi.value));
        }
    }
    out
}

/// The image as usvg reads it: resolved, absolute, normalized.
fn geometry(svg: &str) -> String {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
        .unwrap_or_else(|e| panic!("usvg cannot read {svg}: {e}"));
    tree.to_string(&usvg::WriteOptions::default())
}

/// Optimize `input` and require the same document and the same geometry.
/// Returns the output.
fn assert_same_image(input: &str, metadata: MetadataPolicy) -> String {
    let out = optimize(input, metadata);
    assert!(
        !matches!(out.status, OptimizeStatus::Failed { .. }),
        "{:?} for {input}",
        out.status
    );
    let text = String::from_utf8(out.bytes).unwrap();
    if let OptimizeStatus::Skipped { reason } = &out.status {
        panic!("skipped ({reason}): {input}");
    }
    assert_eq!(signature(input), signature(&text), "{input}\n→ {text}");
    assert_eq!(geometry(input), geometry(&text), "{input}\n→ {text}");
    text
}

const VERBOSE_PATH: &str =
    r#"<path d="M 3.000000 12.000000 L 12.000000 3.000000 L 21.000000 12.000000 Z" fill="red"/>"#;

#[test]
fn layout_whitespace_and_redundant_digits_are_removed() {
    let input = r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg"
     width="10.0"   height="10.0" >
  <g>
    <rect width="10.000" height="10.000"
          fill="red" />
  </g>
</svg>
"#;
    let out = minified(input);
    assert_eq!(
        out,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">\
         <g><rect width=\"10\" height=\"10\" fill=\"red\"/></g></svg>\n"
    );
    assert_same_image(input, MetadataPolicy::default());

    // A second run finds nothing to do.
    let again = optimize(&out, MetadataPolicy::default());
    assert_eq!(again.status, OptimizeStatus::AlreadyOptimal);
}

/// Everything the earlier normalizer deleted, froze or had to skip. The
/// minifier does not interpret any of it, so it must come out unchanged.
#[test]
fn content_the_minifier_does_not_understand_is_kept_as_written() {
    let ns = r#"xmlns="http://www.w3.org/2000/svg" width="100" height="100""#;
    let cases: Vec<(String, &str)> = vec![
        (
            format!(r#"<svg {ns}>{VERBOSE_PATH}<text x="10" y="50">Hello</text></svg>"#),
            r#"<text x="10" y="50">Hello</text>"#,
        ),
        (
            format!(r#"<svg {ns}><a href="https://example.com/">{VERBOSE_PATH}</a></svg>"#),
            r#"<a href="https://example.com/">"#,
        ),
        (
            format!(
                r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="currentColor"/></svg>"#
            ),
            r#"stroke="currentColor""#,
        ),
        (
            format!(
                r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="var(--c, blue)"/></svg>"#
            ),
            r#"stroke="var(--c, blue)""#,
        ),
        (
            format!(
                r#"<svg {ns}>{VERBOSE_PATH}<image href="photo.png" width="10" height="10"/></svg>"#
            ),
            r#"<image href="photo.png""#,
        ),
        (
            format!(r#"<svg {ns}>{VERBOSE_PATH}<use href="sprite.svg#a"/></svg>"#),
            r#"<use href="sprite.svg#a"/>"#,
        ),
        (
            format!(
                r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" fill="url(sprite.svg#g)"/></svg>"#
            ),
            r#"fill="url(sprite.svg#g)""#,
        ),
        (
            format!(
                r#"<svg {ns}><switch> <metadata/> <g systemLanguage="de">{VERBOSE_PATH}</g></switch></svg>"#
            ),
            // In a switch nothing moves: it renders its first usable child.
            r#"<switch> <metadata/> <g systemLanguage="de">"#,
        ),
        (
            format!(
                r#"<svg {ns}><style>.t{{fill:#333}} @media (prefers-color-scheme: dark){{.t{{fill:#eee}}}}</style>{VERBOSE_PATH}</svg>"#
            ),
            "<style>.t{fill:#333} @media (prefers-color-scheme: dark){.t{fill:#eee}}</style>",
        ),
        (
            format!(r#"<svg {ns}><style>path:hover{{fill:blue}}</style>{VERBOSE_PATH}</svg>"#),
            "<style>path:hover{fill:blue}</style>",
        ),
        (
            format!(
                r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="current&#67;olor"/></svg>"#
            ),
            r#"stroke="current&#67;olor""#,
        ),
        (
            format!(r#"<svg {ns} fill="inherit">{VERBOSE_PATH}</svg>"#),
            r#"fill="inherit""#,
        ),
        (
            format!(
                r#"<svg {ns}><path d="M 0.000 0.000 L 9.000 9.000" stroke="color(srgb 1 0 0)"/></svg>"#
            ),
            r#"stroke="color(srgb 1 0 0)""#,
        ),
        (
            format!(
                r#"<svg {ns}><path pathLength="1" stroke="red" stroke-dasharray="0.5 0.5" d="M 0.000 50.000 L 100.000 50.000"/></svg>"#
            ),
            r#"pathLength="1" stroke="red" stroke-dasharray="0.5 0.5""#,
        ),
        (
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100" transform="translate(10 0)">{VERBOSE_PATH}</svg>"#
            ),
            r#"viewBox="0 0 100 100" transform="translate(10 0)""#,
        ),
        (
            format!(
                r##"<svg {ns} xml:base="sprites.svg">{VERBOSE_PATH}<use href="#icon"/></svg>"##
            ),
            r#"xml:base="sprites.svg""#,
        ),
        (
            format!(
                r#"<svg {ns}><rect style="width:100px;height:100px;fill:red"/>{VERBOSE_PATH}</svg>"#
            ),
            r#"style="width:100px;height:100px;fill:red""#,
        ),
        (
            format!(
                r#"<svg {ns}><rect width="2em" height="2.000em" fill="red"/>{VERBOSE_PATH}</svg>"#
            ),
            r#"<rect width="2em" height="2em""#,
        ),
        (
            format!(r#"<svg {ns} overflow="visible">{VERBOSE_PATH}</svg>"#),
            r#"overflow="visible""#,
        ),
        (
            format!(
                r##"<svg {ns}><filter id="f"><feComposite in="FillPaint" in2="SourceGraphic" operator="in"/></filter><g filter="url(#f)">{VERBOSE_PATH}</g></svg>"##
            ),
            r#"in="FillPaint""#,
        ),
        (
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%">{VERBOSE_PATH}</svg>"#
            ),
            r#"width="100%" height="100%""#,
        ),
        (
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{VERBOSE_PATH}</svg>"#),
            r#"<svg xmlns="http://www.w3.org/2000/svg"><path"#,
        ),
        (
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 100 100">{VERBOSE_PATH}</svg>"#
            ),
            r#"width="200" height="100" viewBox="0 0 100 100""#,
        ),
        (
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" preserveAspectRatio="none">{VERBOSE_PATH}</svg>"#
            ),
            r#"viewBox="0 0 24 24" preserveAspectRatio="none""#,
        ),
        (
            format!(
                r#"<svg {ns}><style>@keyframes spin {{ to {{ transform: rotate(360deg); }} }} rect {{ animation: spin 2s linear infinite; }}</style>{VERBOSE_PATH}</svg>"#
            ),
            "@keyframes spin { to { transform: rotate(360deg); } }",
        ),
        (
            format!(
                r#"<svg {ns}><circle r="5.0"><animate attributeName="r" values="5.0;10.0;5.0" dur="2s" repeatCount="indefinite"/></circle></svg>"#
            ),
            r#"<animate attributeName="r" values="5.0;10.0;5.0" dur="2s" repeatCount="indefinite"/>"#,
        ),
        (
            format!(
                r#"<svg {ns} xmlns:x="urn:example:app" x:data="keep me"><x:note>keep me</x:note>{VERBOSE_PATH}</svg>"#
            ),
            r#"x:data="keep me"><x:note>keep me</x:note>"#,
        ),
        (
            format!(
                r#"<svg {ns} data-label="温度(°C)" custom="x" xml:lang="en">{VERBOSE_PATH}</svg>"#
            ),
            r#"data-label="温度(°C)" custom="x" xml:lang="en""#,
        ),
    ];
    // usvg reads only the first child of `<style>`, so it cannot compare this
    // one: a comment in front of the rules. A browser applies the rules with
    // and without the comment.
    let commented = format!(
        r#"<svg {ns}><style><!--x-->.a{{fill:#00ff00}}</style><rect class="a" width="20.000" height="10.000"/></svg>"#
    );
    let out = minified(&commented);
    assert_eq!(signature(&commented), signature(&out));
    assert!(
        out.contains(r#"<style>.a{fill:#00ff00}</style><rect class="a" width="20" height="10"/>"#)
    );

    for (input, kept) in &cases {
        for policy in [
            MetadataPolicy::StripAll,
            MetadataPolicy::KeepColorProfile,
            MetadataPolicy::KeepAll,
        ] {
            let out = assert_same_image(input, policy);
            assert!(out.contains(kept), "{kept} is missing from {out}");
            assert!(out.len() < input.len(), "nothing was saved in {input}");
        }
    }
}

#[test]
fn text_content_and_its_whitespace_are_never_touched() {
    let input = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100.0" height="20.0">
  <text x="0" y="15">  Two   <tspan>words</tspan> <tspan>apart</tspan>
  </text>
  <text xml:space="preserve">  kept  </text>
  <g xml:space="preserve">
    <path d="M0 0L1 1"/>
  </g>
  <style>
    .a { fill: red }
  </style>
  <foreignObject width="10" height="10">
    <p xmlns="http://www.w3.org/1999/xhtml"> a <b>b</b> <i>c</i> </p>
  </foreignObject>
  <title> A  title </title>
  <desc><![CDATA[ a < b ]]></desc>
</svg>"#;
    let out = assert_same_image(input, MetadataPolicy::StripAll);
    for kept in [
        "<text x=\"0\" y=\"15\">  Two   <tspan>words</tspan> <tspan>apart</tspan>\n  </text>",
        "<text xml:space=\"preserve\">  kept  </text>",
        "<g xml:space=\"preserve\">\n    <path d=\"M0 0L1 1\"/>\n  </g>",
        "<style>\n    .a { fill: red }\n  </style>",
        "<foreignObject width=\"10\" height=\"10\">\n    <p xmlns=\"http://www.w3.org/1999/xhtml\"> a <b>b</b> <i>c</i> </p>\n  </foreignObject>",
        "<title> A  title </title>",
        "<desc><![CDATA[ a < b ]]></desc>",
    ] {
        assert!(out.contains(kept), "{kept:?} is missing from {out}");
    }
    // The layout whitespace around them is gone.
    assert!(out.contains("</text><text"), "{out}");
    assert!(out.contains("</title><desc>"), "{out}");
}

#[test]
fn root_size_and_view_box_are_kept_as_written() {
    let icon = format!(r#"<svg xmlns="{SVG}" viewBox="0 0 24 24">{VERBOSE_PATH}</svg>"#);
    let out = assert_same_image(&icon, MetadataPolicy::default());
    assert!(
        out.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path"#),
        "{out}"
    );

    let sized = format!(
        r#"<svg xmlns="{SVG}" width="200.000" height="100.000" viewBox="0 0 400 200">{VERBOSE_PATH}</svg>"#
    );
    let out = assert_same_image(&sized, MetadataPolicy::default());
    assert!(
        out.contains(r#"width="200" height="100" viewBox="0 0 400 200""#),
        "{out}"
    );
    let again = optimize(&out, MetadataPolicy::default());
    assert_eq!(again.status, OptimizeStatus::AlreadyOptimal);
}

#[test]
fn styles_references_and_embedded_images_survive() {
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let input = format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">
  <style>.a{{fill:#ff0000}} g > .b {{ fill: #00ff00 }}</style>
  <defs>
    <linearGradient id="g"><stop offset="0.000" stop-color="#fff"/><stop offset="1.000" stop-color="#000"/></linearGradient>
    <rect id="r" width="10.000000" height="10.000000"/>
  </defs>
  <rect class="a" x="0.000000" y="0.000000" width="50.000000" height="50.000000"/>
  <g><rect class="b" x="50.000000" y="50.000000" width="50.000000" height="50.000000"/></g>
  <use xlink:href="#r" x="20.000000" y="20.000000" fill="url(#g)"/>
  <g transform="translate(5.000000 5.000000) rotate(45.000000)"><rect style="fill:rgb(0, 0, 255); stroke-width: 2.000" width="5.000000" height="5.000000"/></g>
  <polygon points="0.0,0.0 10.50,0.0 10.50,10.50" fill="url(#g)"/>
  <image width="10.000000" height="10.000000" href="{png}"/>
</svg>"##
    );
    let out = assert_same_image(&input, MetadataPolicy::default());
    assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE svg PUBLIC"));
    assert!(out.contains(".a{fill:#ff0000} g > .b { fill: #00ff00 }"));
    assert!(out.contains(r##"<use xlink:href="#r" x="20" y="20" fill="url(#g)"/>"##));
    // Transforms and style attributes are not numbers the minifier rewrites.
    assert!(out.contains(r#"transform="translate(5.000000 5.000000) rotate(45.000000)""#));
    assert!(out.contains(r#"style="fill:rgb(0, 0, 255); stroke-width: 2.000""#));
    assert!(out.contains(r#"points="0 0 10.5 0 10.5 10.5""#));
    assert!(out.contains(png));
}

#[test]
fn comments_and_editor_data_follow_the_metadata_policy() {
    let input = format!(
        r##"<svg xmlns="{SVG}" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100" inkscape:version="1.3" sodipodi:docname="a.svg">
  <!-- made by hand -->
  <!--! Licence: keep this notice -->
  <sodipodi:namedview id="namedview1" pagecolor="#ffffff"><inkscape:grid id="grid1"/></sodipodi:namedview>
  <metadata id="metadata1"><rdf:RDF><rdf:Description rdf:about=""/></rdf:RDF></metadata>
  <title>Home</title>
  <g inkscape:label="Layer 1" inkscape:groupmode="layer" id="layer1" role="img" aria-label="Home">
    {VERBOSE_PATH}
  </g>
</svg>"##
    );

    // Default and strip-all: comments and editor data go, content stays.
    for policy in [MetadataPolicy::KeepColorProfile, MetadataPolicy::StripAll] {
        let out = optimize(&input, policy);
        assert_eq!(out.status, OptimizeStatus::Optimized);
        let text = String::from_utf8(out.bytes).unwrap();
        assert_eq!(
            text,
            format!(
                r##"<svg xmlns="{SVG}" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100"><!--! Licence: keep this notice --><title>Home</title><g id="layer1" role="img" aria-label="Home"><path d="M3 12L12 3L21 12Z" fill="red"/></g></svg>"##
            )
        );
        assert_eq!(geometry(&input), geometry(&text));
    }

    // Keep-all: only layout whitespace and digits.
    let out = assert_same_image(&input, MetadataPolicy::KeepAll);
    for kept in [
        "<!-- made by hand -->",
        "<sodipodi:namedview",
        "<metadata id=\"metadata1\"><rdf:RDF>",
        "inkscape:label=\"Layer 1\"",
        "xmlns:rdf=",
    ] {
        assert!(out.contains(kept), "{kept} is missing from {out}");
    }
    assert!(out.len() < input.len());
}

#[test]
fn editor_data_stays_when_removing_it_could_change_the_image() {
    let ink = r#"xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape""#;

    // A stylesheet can select by position or attribute.
    let styled = format!(
        r#"<svg xmlns="{SVG}" {ink} width="10" height="10"><style>g:first-child{{fill:red}}</style><metadata/><inkscape:x/><g inkscape:label="a">{VERBOSE_PATH}</g></svg>"#
    );
    let out = assert_same_image(&styled, MetadataPolicy::StripAll);
    assert!(out.contains("<metadata/><inkscape:x/><g inkscape:label=\"a\">"));

    // An element inside the editor data is used by the drawing.
    let referenced = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><g id="shape">{VERBOSE_PATH}</g></metadata><use href="#shape"/></svg>"##
    );
    let out = assert_same_image(&referenced, MetadataPolicy::StripAll);
    assert!(out.contains("<metadata><g id=\"shape\">"));

    // The same with the reference written as a character reference, in a
    // style attribute, and as an animation trigger.
    for reference in [
        r##"<use href="&#35;shape"/>"##,
        r##"<rect width="1" height="1" style="fill:url( '#shape' )"/>"##,
        r##"<set attributeName="x" to="1" begin="shape.click"/>"##,
    ] {
        let input = format!(
            r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><g id="shape">{VERBOSE_PATH}</g></metadata>{reference}</svg>"##
        );
        let out = optimize(&input, MetadataPolicy::StripAll);
        let text = String::from_utf8(out.bytes).unwrap();
        assert!(text.contains("<metadata><g id=\"shape\">"), "{text}");
    }
    // A CSS escape can spell the id: `\67` is `g`.
    let escaped = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><linearGradient id="g"><stop offset="0" stop-color="red"/></linearGradient></metadata>  <rect width="10" height="10" style="fill:url(#\67) blue"/></svg>"##
    );
    let out = optimize(&escaped, MetadataPolicy::StripAll);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(
        text.contains("<metadata><linearGradient id=\"g\">"),
        "{text}"
    );

    // A percent-encoded fragment names the decoded id: `#%61` is `a`.
    let encoded = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><rect id="a" width="10" height="10"/></metadata><use href="#%61"/>{VERBOSE_PATH}</svg>"##
    );
    let out = optimize(&encoded, MetadataPolicy::StripAll);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(text.contains("<metadata><rect id=\"a\""), "{text}");

    // An id may contain a comma or a semicolon.
    let listed = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><rect id="a,b" width="10" height="10"/></metadata><use href="#a,b"/>{VERBOSE_PATH}</svg>"##
    );
    let out = optimize(&listed, MetadataPolicy::StripAll);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(text.contains("<metadata><rect id=\"a,b\""), "{text}");

    // Or a parenthesis.
    let paren = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><rect id="a)b" width="10" height="10"/></metadata><use href="#a)b"/>{VERBOSE_PATH}</svg>"##
    );
    let out = optimize(&paren, MetadataPolicy::StripAll);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(text.contains("<metadata><rect id=\"a)b\""), "{text}");

    // An animation acts on its target from inside the editor data.
    let animated = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata><set href="#r" attributeName="fill" to="red"/></metadata><rect id="r" width="10" height="10" fill="blue"/>{VERBOSE_PATH}</svg>"##
    );
    let out = optimize(&animated, MetadataPolicy::StripAll);
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(text.contains("<metadata><set href=\"#r\""), "{text}");

    // An id nobody names does not keep editor data alive.
    let unused = format!(
        r##"<svg xmlns="{SVG}" {ink} width="10" height="10"><metadata id="m"><g id="shape"/></metadata><use href="#other"/>{VERBOSE_PATH}</svg>"##
    );
    assert!(!minified(&unused).contains("metadata"));

    // Editor data inside text is part of the text layout.
    let in_text = format!(
        r#"<svg xmlns="{SVG}" {ink} width="10" height="10">{VERBOSE_PATH}<text>a<inkscape:x/> b</text></svg>"#
    );
    let out = assert_same_image(&in_text, MetadataPolicy::StripAll);
    assert!(out.contains("<text>a<inkscape:x/> b</text>"));

    // A namespace that is still in use keeps its declaration.
    let used = format!(
        r#"<svg xmlns="{SVG}" xmlns:dc="http://purl.org/dc/elements/1.1/" width="10" height="10"><title dc:x="1">t</title>{VERBOSE_PATH}</svg>"#
    );
    let out = assert_same_image(&used, MetadataPolicy::StripAll);
    assert!(out.contains("xmlns:dc="));
}

#[test]
fn stylesheets_limit_what_is_rewritten() {
    // `:empty` sees whitespace: an element without child elements keeps it.
    let empty = format!(
        r#"<svg xmlns="{SVG}" width="10.0" height="10.0"><style>g:empty{{display:none}}</style><g> </g>  <g>{VERBOSE_PATH}</g></svg>"#
    );
    let out = assert_same_image(&empty, MetadataPolicy::default());
    assert!(out.contains("<g> </g><g>"), "{out}");

    // An attribute selector can match the spelling of a number.
    let selector = format!(
        r#"<svg xmlns="{SVG}" width="10.0" height="10.0"><style>rect[width="5.0"]{{fill:red}}</style>  <rect width="5.0" height="5.0"/></svg>"#
    );
    let out = assert_same_image(&selector, MetadataPolicy::default());
    assert!(
        out.contains(r#"width="10.0" height="10.0""#) && out.contains(r#"<rect width="5.0""#),
        "{out}"
    );

    // The sheet is the text around a comment taken together, escapes can
    // spell anything, and `attr()` reads attribute text.
    for css in [
        r#"@im<!--x-->port url("a.css");"#,
        r#"@\69mport url("a.css");"#,
        r#"rect{width:attr(width px)}"#,
        r#"rect<!--x-->[width="5.0"]{fill:red}"#,
    ] {
        let input = format!(
            r#"<svg xmlns="{SVG}" width="10.0" height="10.0"><style>{css}</style>  <rect width="5.0" height="5.0"/></svg>"#
        );
        let out = optimize(&input, MetadataPolicy::default());
        let text = String::from_utf8(out.bytes).unwrap();
        assert!(
            text.contains(r#"width="10.0""#) && text.contains(r#"<rect width="5.0""#),
            "{css}: {text}"
        );
    }

    // A stylesheet the codec cannot read is treated the same way.
    let external = format!(
        "<?xml-stylesheet href=\"a.css\"?>\n<svg xmlns=\"{SVG}\" width=\"10.0\" height=\"10.0\">  <metadata/><rect width=\"5.0\" height=\"5.0\"/></svg>"
    );
    let out = assert_same_image(&external, MetadataPolicy::default());
    assert!(
        out.contains(r#"width="10.0""#) && out.contains("<metadata/><rect width=\"5.0\""),
        "{out}"
    );
}

#[test]
fn scripted_and_unreadable_documents_are_skipped() {
    // XSLT can read comments and whitespace.
    for pi in [
        r#"<?xml-stylesheet type="text/xsl" href="t.xsl"?>"#,
        r#"<?xml-stylesheet href="t.xsl" type="application/xslt+xml"?>"#,
    ] {
        let input = format!(r#"{pi}<svg xmlns="{SVG}"><!-- c -->  <path d="M0.0 0"/></svg>"#);
        assert_skipped(&input, MetadataPolicy::StripAll);
    }
    let css = format!(
        r#"<?xml-stylesheet type="text/css" href="s.css"?><svg xmlns="{SVG}"><!-- c -->  <path d="M0.0 0"/></svg>"#
    );
    assert!(!matches!(
        optimize(&css, MetadataPolicy::StripAll).status,
        OptimizeStatus::Skipped { .. }
    ));

    // `attr()` in a presentation attribute reads other attributes as text.
    let input = format!(
        r#"<svg xmlns="{SVG}"><text x="10.0" font-family="attr(x)">a</text><!-- c --></svg>"#
    );
    let text = String::from_utf8(optimize(&input, MetadataPolicy::StripAll).bytes).unwrap();
    assert!(text.contains(r#"x="10.0""#), "{text}");

    let cases = [
        format!(r#"<svg xmlns="{SVG}">  <script>alert(1)</script></svg>"#),
        format!(r#"<svg xmlns="{SVG}">  <rect width="10.0" height="10.0" onfocus="steal()"/></svg>"#),
        format!(r#"<svg xmlns="{SVG}">  <a href=" JavaScript:alert(1)"><rect width="1.0" height="1.0"/></a></svg>"#),
        format!(r#"<svg xmlns="{SVG}">  <a href="java&#10;script:alert(1)"><rect width="1.0" height="1.0"/></a></svg>"#),
        format!(
            r#"<svg xmlns="{SVG}">  <foreignObject><iframe xmlns="http://www.w3.org/1999/xhtml" src="a.html"/></foreignObject></svg>"#
        ),
        format!(
            r#"<svg xmlns="{SVG}" xmlns:h="http://www.w3.org/1999/xhtml">  {VERBOSE_PATH}<h:script>alert(1)</h:script></svg>"#
        ),
        format!(
            r#"<svg xmlns="{SVG}" xmlns:x="urn:example:x">  {VERBOSE_PATH}<x:wrap><x:Script>1</x:Script></x:wrap></svg>"#
        ),
        // Entities have no position of their own in the source.
        format!(
            r#"<!DOCTYPE svg [ <!ENTITY c "red"> ]><svg xmlns="{SVG}" width="10.0" height="10.0">  <rect fill="&c;" width="1" height="1"/></svg>"#
        ),
        // A prefixed root is not recognized as SVG by the format sniffing.
        r#"<s:svg xmlns:s="http://www.w3.org/2000/svg" width="100.0" height="100.0"><s:text x="1" y="9">Hi</s:text></s:svg>"#.to_string(),
        // Not an SVG document in the SVG namespace.
        format!(r#"<svg width="100.0" height="100.0">  {VERBOSE_PATH}</svg>"#),
        format!(r#"<html xmlns="http://www.w3.org/1999/xhtml">  <svg xmlns="{SVG}"/></html>"#),
    ];
    for case in &cases {
        for policy in [MetadataPolicy::StripAll, MetadataPolicy::KeepAll] {
            assert_skipped(case, policy);
        }
    }

    // Not UTF-8.
    let mut bytes = format!(r#"<svg xmlns="{SVG}">  "#).into_bytes();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    let out = optimize_bytes(&bytes, &OptimizeOptions::default()).unwrap();
    assert!(matches!(out.status, OptimizeStatus::Skipped { .. }));
    assert_eq!(out.bytes, bytes);
}

/// The XML parser recurses along nesting; a stack overflow would abort the
/// whole process, so over-deep documents are skipped before it runs.
#[test]
fn deep_nesting_is_skipped_and_reference_chains_are_harmless() {
    let open = format!(r#"<svg xmlns="{SVG}" width="24" height="24">"#);
    let path = r#"<path d="M0.0000 0.0000L24.0000 24.0000"/>"#;

    let nested = format!(
        "{open}{}{path}{}</svg>",
        "<g>".repeat(100_000),
        "</g>".repeat(100_000)
    );
    assert_skipped(&nested, MetadataPolicy::StripAll);

    // References are not followed any more, so long chains and cycles are
    // ordinary flat markup.
    let chain: String = (0..5_000)
        .map(|i| format!(r##"<g id="a{i}"> <use href="#a{}"/></g>"##, i + 1))
        .collect();
    let chained = format!(r#"{open}{chain}<path id="a5000" d="M0 0L24 24"/></svg>"#);
    let out = optimize(&chained, MetadataPolicy::StripAll);
    assert_eq!(out.status, OptimizeStatus::Optimized);
    assert_eq!(
        signature(&chained),
        signature(std::str::from_utf8(&out.bytes).unwrap())
    );

    let cyclic =
        format!(r##"{open}  <g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g></svg>"##);
    let out = optimize(&cyclic, MetadataPolicy::StripAll);
    assert_eq!(out.status, OptimizeStatus::Optimized);

    // 40 nested groups are ordinary artwork.
    let ordinary = format!(
        r#"{open}{}{path}{}</svg>"#,
        "<g>".repeat(40),
        "</g>".repeat(40)
    );
    assert_same_image(&ordinary, MetadataPolicy::default());
    assert_eq!(
        optimize(&ordinary, MetadataPolicy::default()).status,
        OptimizeStatus::Optimized
    );
}

#[test]
fn windows_line_endings_a_bom_and_odd_quoting_are_handled() {
    let input = format!(
        "\u{feff}<?xml version='1.0'?>\r\n<svg xmlns='{SVG}'\r\n  width='10.0' height=\"10.0\">\r\n  <rect width = '5.0'\r\n\theight='5.0' fill='a\"b'/>\r\n</svg>\r\n"
    );
    let out = assert_same_image(&input, MetadataPolicy::default());
    assert_eq!(
        out,
        format!(
            "\u{feff}<?xml version='1.0'?>\r\n<svg xmlns='{SVG}' width='10' height=\"10\"><rect width='5' height='5' fill='a\"b'/></svg>\r\n"
        )
    );
}

/// A small deterministic generator (no test dependency for randomness).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as u32) % bound
    }

    /// A number in one of the spellings SVG allows.
    fn number(&mut self, signed: bool) -> String {
        let int = self.next(40);
        let frac = self.next(100_000);
        let sign = match (signed, self.next(4)) {
            (true, 0) => "-",
            (true, 1) => "+",
            _ => "",
        };
        match self.next(8) {
            0 => format!("{sign}{int}"),
            1 => format!("{sign}{int}.{frac:05}"),
            2 => format!("{sign}{int}.{:02}000", frac % 100),
            3 => format!("{sign}.{frac:05}"),
            4 => format!("{sign}0.{:03}", frac % 1000),
            5 => format!("{sign}00{int}.50"),
            6 => format!("{sign}{}e1", int % 9),
            _ => format!("{sign}{int}.{}E-1", frac % 10),
        }
    }

    fn separator(&mut self) -> &'static str {
        match self.next(5) {
            0 => ",",
            1 => " , ",
            2 => "\n\t",
            3 => "  ",
            _ => " ",
        }
    }

    fn path(&mut self, arcs: bool) -> String {
        let mut d = String::from(if self.next(2) == 0 { "M" } else { "m " });
        d.push_str(&format!(
            "{}{}{}",
            self.number(true),
            self.separator(),
            self.number(true)
        ));
        let commands: &[(char, usize)] = &[
            ('L', 2),
            ('H', 1),
            ('V', 1),
            ('C', 6),
            ('S', 4),
            ('Q', 4),
            ('T', 2),
            ('A', 7),
            ('Z', 0),
            ('M', 2),
        ];
        for _ in 0..2 + self.next(12) {
            // Sometimes continue the previous command without a letter.
            let (letter, count) = commands[self.next(commands.len() as u32) as usize];
            if letter == 'A' && !arcs {
                continue;
            }
            let letter = if self.next(2) == 0 {
                letter
            } else {
                letter.to_ascii_lowercase()
            };
            d.push_str(["", " ", "\n\t"][self.next(3) as usize]);
            d.push(letter);
            for repeat in 0..1 + self.next(2) as usize {
                if count == 0 && repeat > 0 {
                    break;
                }
                for k in 0..count {
                    let after_flag = letter.eq_ignore_ascii_case(&'A') && (k == 4 || k == 5);
                    if after_flag {
                        // usvg, the reference here, does not accept spaces
                        // before a comma after a flag (the grammar does).
                        d.push_str(["", " ", ","][self.next(3) as usize]);
                    } else if k > 0 || repeat > 0 {
                        d.push_str(self.separator());
                    } else if self.next(2) == 0 {
                        d.push(' ');
                    }
                    if letter.eq_ignore_ascii_case(&'A') && (k == 3 || k == 4) {
                        d.push(if self.next(2) == 0 { '0' } else { '1' });
                    } else {
                        let radius = letter.eq_ignore_ascii_case(&'A') && k < 2;
                        d.push_str(&self.number(!radius));
                    }
                }
            }
        }
        d
    }
}

/// Numbers that usvg wrote, in order.
fn numbers(text: &str) -> Vec<f64> {
    text.split(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e')))
        .filter_map(|token| token.parse::<f64>().ok())
        .collect()
}

#[test]
fn generated_paths_keep_their_geometry() {
    let mut rng = Lcg(0x5eed);
    let mut rewritten = 0;
    for round in 0..400 {
        let d = rng.path(true);
        let input = format!(
            r#"<svg xmlns="{SVG}" viewBox="-200 -200 400 400"><path d="{d}" stroke="red" fill="none"/></svg>"#
        );
        let out = optimize(&input, MetadataPolicy::default());
        assert!(
            !matches!(
                out.status,
                OptimizeStatus::Failed { .. } | OptimizeStatus::Skipped { .. }
            ),
            "round {round}: {:?} for {d}",
            out.status
        );
        let text = String::from_utf8(out.bytes).unwrap();
        assert_eq!(
            geometry(&input),
            geometry(&text),
            "round {round}: {d}\n→ {text}"
        );
        rewritten += usize::from(out.status == OptimizeStatus::Optimized);
    }
    assert!(rewritten > 350, "only {rewritten} paths were rewritten");
}

#[test]
fn lossy_rounding_moves_no_point_by_more_than_the_tolerance() {
    let mut rng = Lcg(0xc0ffee);
    let opts = OptimizeOptions {
        lossy: true,
        metadata: MetadataPolicy::StripAll,
        ..Default::default()
    };
    let mut rounded = 0;
    for round in 0..300 {
        let d = rng.path(false);
        let input = format!(
            r#"<svg xmlns="{SVG}" viewBox="-200 -200 400 400"><path d="{d}" stroke="red" fill="none"/><polyline points="1.23456,2.34567 3.45678,4.56789" stroke-width="0.12345"/></svg>"#
        );
        let out = optimize_bytes(input.as_bytes(), &opts).unwrap();
        assert!(
            !matches!(out.status, OptimizeStatus::Failed { .. }),
            "round {round}: {:?}",
            out.status
        );
        let text = String::from_utf8(out.bytes).unwrap();
        let (before, after) = (numbers(&geometry(&input)), numbers(&geometry(&text)));
        assert_eq!(before.len(), after.len(), "round {round}: {d}\n→ {text}");
        for (a, b) in before.iter().zip(&after) {
            // 0.0005 per coordinate, plus usvg's single-precision output.
            assert!(
                (a - b).abs() <= 0.0007,
                "round {round}: {a} vs {b}\n{d}\n→ {text}"
            );
        }
        rounded += usize::from(text.contains("1.235 2.346 3.457 4.568"));
    }
    assert_eq!(rounded, 300, "every polyline is rounded");

    // The root sets the scale of the image: its size is not rounded.
    let root = format!(
        r#"<svg xmlns="{SVG}" width="10.12345" height="10.12345" x="0.00049">  <rect width="1.23456" height="1"/></svg>"#
    );
    let out = optimize_bytes(root.as_bytes(), &opts).unwrap();
    let text = String::from_utf8(out.bytes).unwrap();
    assert!(
        text.contains(r#"width="10.12345" height="10.12345" x=".00049"><rect width="1.235""#),
        "{text}"
    );

    // An arc is sensitive to its end points: such a path keeps every digit.
    let arc = format!(
        r#"<svg xmlns="{SVG}" viewBox="0 0 24 24">  <path d="M1.23456 2.34567A5.55555 5.55555 0 0 1 9.87654 3.21098"/></svg>"#
    );
    let out = optimize_bytes(arc.as_bytes(), &opts).unwrap();
    assert!(String::from_utf8(out.bytes)
        .unwrap()
        .contains(r#"d="M1.23456 2.34567A5.55555 5.55555 0 0 1 9.87654 3.21098""#));
}

/// Damaged input must never panic, and whatever is written must still be the
/// same document.
#[test]
fn mutated_documents_never_panic_or_change_meaning() {
    let base = format!(
        r##"<?xml version="1.0"?>
<!-- c -->
<svg xmlns="{SVG}" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" viewBox="0 0 24.0 24.0">
  <defs> <linearGradient id="g"><stop offset="0.50"/></linearGradient> </defs>
  <g inkscape:label="x" fill="url(#g)">
    <path d="M 1.50,2.50 l 3.0 4.0 a1 1 0 01.5.5 z"/> <text x="1.0"> a <tspan>b</tspan> </text>
    <polygon points="0.0,0.0 1.0,1.0"/>
  </g>
</svg>
"##
    );
    let alphabet: Vec<char> = "<>/\"'= .,-+e01aMzL&;!?[]:x\n中\\#".chars().collect();
    let mut rng = Lcg(42);
    for _ in 0..4_000 {
        let mut chars: Vec<char> = base.chars().collect();
        for _ in 0..1 + rng.next(3) {
            let at = rng.next(chars.len() as u32) as usize;
            match rng.next(3) {
                0 => chars[at] = alphabet[rng.next(alphabet.len() as u32) as usize],
                1 => {
                    chars.remove(at);
                }
                _ => chars.insert(at, alphabet[rng.next(alphabet.len() as u32) as usize]),
            }
        }
        let input: String = chars.into_iter().collect();
        for policy in [MetadataPolicy::StripAll, MetadataPolicy::KeepAll] {
            match optimize_bytes(input.as_bytes(), &options(policy, false)) {
                Ok(out) if out.status == OptimizeStatus::Optimized => {
                    let text = String::from_utf8(out.bytes).unwrap();
                    if policy == MetadataPolicy::KeepAll {
                        assert_eq!(signature(&input), signature(&text), "{input}\n→ {text}");
                    }
                    if let Ok(before) = usvg::Tree::from_str(&input, &usvg::Options::default()) {
                        let after = geometry(&text);
                        let before = before.to_string(&usvg::WriteOptions::default());
                        assert_eq!(before, after, "{input}\n→ {text}");
                    }
                }
                Ok(_) => {}
                Err(error) => assert!(
                    !matches!(error, imageopt_core::Error::Panicked(_)),
                    "{error} for {input}"
                ),
            }
        }
    }
}

#[test]
fn odd_but_valid_markup_is_handled() {
    // Non-ASCII names and values.
    let unicode =
        format!(r#"<svg xmlns="{SVG}" 中="1" ön="x">  {VERBOSE_PATH}<text>Größe 中</text></svg>"#);
    let out = assert_same_image(&unicode, MetadataPolicy::default());
    assert!(out.contains(r#"中="1" ön="x""#), "{out}");

    // Hundreds of padding zeros: left exactly as written (see MAX_DIGITS).
    let padded = format!(
        r#"<svg xmlns="{SVG}" viewBox="0 0 24 24">  <path d="M{}10 0L20.0 20L0 20Z"/></svg>"#,
        "0".repeat(400)
    );
    let out = minified(&padded);
    assert!(out.contains(&format!("{}10 0L20.0 20L0 20Z", "0".repeat(400))));
}

/// Work must grow with the size of the file, not with its square.
#[test]
fn large_flat_documents_are_processed_in_linear_time() {
    let started = std::time::Instant::now();

    let metadata: String = (0..40_000)
        .map(|i| format!(r#"<metadata id="a{i}"/>"#))
        .collect();
    let input = format!(r##"<svg xmlns="{SVG}"><use href="#a7"/>{metadata}</svg>"##);
    let out = minified(&input);
    assert_eq!(
        out.matches("<metadata").count(),
        1,
        "only the referenced one stays"
    );

    let comments = "<!--!x--> \n".repeat(40_000);
    let input =
        format!(r#"<svg xmlns="{SVG}"><style>g{{fill:red}}</style><g>{comments}<path/></g></svg>"#);
    let out = minified(&input);
    assert!(out.contains("<!--!x--><!--!x-->"));

    // A value made of reference marks.
    let input = format!(
        r#"<svg xmlns="{SVG}" data-x="{}">  <path/></svg>"#,
        "#".repeat(200_000)
    );
    assert!(minified(&input).contains("data-x=\"####"));

    // Many declarations on the root, many children that inherit them.
    let declarations: String = (0..5_000)
        .map(|i| format!(r#" xmlns:p{i}="urn:p{i}""#))
        .collect();
    let input = format!(
        r#"<svg xmlns="{SVG}"{declarations}> {}</svg>"#,
        "<g/>".repeat(5_000)
    );
    let out = minified(&input);
    assert!(out.contains("xmlns:p4999=\"urn:p4999\"><g/><g/>"));

    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "took {:?}",
        started.elapsed()
    );
}
