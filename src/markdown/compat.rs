//! reveal.js compatibility: `<!-- .slide: -->` and `<!-- .element: -->` comments and the
//! `Note:` convention. Implemented by T43.
//!
//! Contract: `docs/spec/markdown-dialect.md` §2.2 (row 4), §3.2 (source 2), §3.3, §7 and §9.
//! Everything here is opt-in by syntax: a deck without these comments or a `Note:` paragraph
//! renders exactly as before.
//!
//! - **`<!-- .slide: … -->`** (slide attributes): a top-level HTML block that is exactly one
//!   comment whose text starts with `.slide:` ([`slide_comment`]). The splitter
//!   ([`split`]) reads it among the comments that start the slide, or among those right after
//!   the slide's first visible node when that is a heading (the heading is the boundary in
//!   headings mode, and reveal decks often put the comment under the title); other comments
//!   ([`is_blank`]) may come before or between. The text goes through
//!   [`parse_attr_block`](crate::markdown::attrs::parse_attr_block) and
//!   [`SlideAttrs::from_parsed`](crate::markdown::attrs::SlideAttrs::from_parsed), so it takes
//!   `k="v"`, `k=v`, `k='v'`, bare keys, `.class` and `#id`, and keys are normalized like every
//!   other slide attribute (`data-background="#f00"` stays verbatim, the `background=`
//!   shorthand works too). The comment is removed from the slide. Its attributes come after
//!   the separator's and before the lifted heading attributes (§3.2). Several such comments
//!   are all read, in order. One anywhere else (also nested, or inline in a paragraph or
//!   heading) stays a plain comment and is reported as `misplaced-slide-comment`.
//! - **`data-background="image.png"`** ([`image_background`]): reveal treats a
//!   `data-background` value that looks like an image as `data-background-image`; so does the
//!   frontend, before slide assets are registered.
//! - **`<!-- .element: … -->`** (element attributes, [`element_comment`]): applied to the
//!   rendered HTML by [`postprocess`](crate::markdown::postprocess) the way reveal's Markdown
//!   plugin does: to the nearest preceding sibling element (`<br>` skipped), or else to the
//!   enclosing element. So `- item <!-- .element: class="fragment" -->` sets the `<li>`'s
//!   class, `![](x.png) <!-- .element: width="300" -->` the `<img>`'s width, and a comment on
//!   its own line after a list or paragraph the `<ul>`/`<p>` before it.
//! - **`Note:`** (speaker notes, §7): in [`RenderMode::Deck`] a top-level paragraph whose text
//!   starts with `Note:` or `Notes:` (exactly so, as the spec lists them, and as typed in the
//!   source) ends the slide body: it, without the marker, and every later node become notes,
//!   placed before the notes from `:::notes` directives ([`render_slide`]). To start a
//!   paragraph with the word as content, escape the colon (`Note\:`) or emphasise the word
//!   (`**Note:**`).

use comrak::Arena;
use comrak::Options;
use comrak::nodes::{AstNode, NodeValue};

use crate::error::Result;
use crate::markdown::render::{RenderCtx, RenderMode, Rendered, render_nodes};
use crate::markdown::split;

/// The text after `.slide:` when `literal` (an HTML block's literal) is exactly one HTML
/// comment, surrounded by optional whitespace, whose content starts with `.slide:` (after
/// optional whitespace): `<!-- .slide: data-background="#f00" class="x" -->` gives
/// ` data-background="#f00" class="x" `. `None` for anything else, e.g. a comment followed by
/// more text or by a second comment.
pub fn slide_comment(literal: &str) -> Option<&str> {
    let content = literal.trim().strip_prefix("<!--")?.strip_suffix("-->")?;
    if content.contains("-->") {
        return None;
    }
    content.trim_start().strip_prefix(".slide:")
}

/// [`slide_comment`] of a node: `Some` for an `HtmlBlock` holding one `.slide:` comment.
pub fn slide_comment_node<'a>(node: &'a AstNode<'a>) -> Option<String> {
    match &node.data().value {
        NodeValue::HtmlBlock(block) => slide_comment(&block.literal).map(str::to_string),
        _ => None,
    }
}

/// Whether some HTML comment in `html` starts with `.slide:` (used to report comments that
/// are not where the splitter reads them).
pub fn mentions_slide_comment(html: &str) -> bool {
    comment_contents(html).any(|content| content.trim_start().starts_with(".slide:"))
}

/// The attribute text of an `.element` comment, given the comment's content (the text between
/// `<!--` and `-->`): the content after optional whitespace must start with `.element:` or
/// with `.element` and whitespace (reveal's plugin does not require the colon). `None` for
/// any other comment.
pub fn element_comment(content: &str) -> Option<&str> {
    let rest = content.trim_start().strip_prefix(".element")?;
    match rest.strip_prefix(':') {
        Some(attrs) => Some(attrs),
        None => rest.starts_with(char::is_whitespace).then_some(rest),
    }
}

/// Whether a top-level node renders nothing visible: an `HtmlBlock` made only of HTML
/// comments and whitespace (a licence header, a `<!-- .slide: -->` comment). The headings
/// mode of the splitter does not start a slide for such nodes alone (dialect §2.3 "only if it
/// is non-blank").
pub fn is_blank<'a>(node: &'a AstNode<'a>) -> bool {
    match &node.data().value {
        NodeValue::HtmlBlock(block) => is_comment_only(&block.literal),
        _ => false,
    }
}

/// Whether `html` is nothing but HTML comments (`<!-- … -->`, also the empty `<!-->` and
/// `<!--->`) and whitespace.
fn is_comment_only(html: &str) -> bool {
    let mut rest = html.trim_start();
    while !rest.is_empty() {
        let Some(after) = rest.strip_prefix("<!--") else {
            return false;
        };
        let end = if let Some(tail) = after.strip_prefix('>') {
            tail
        } else if let Some(tail) = after.strip_prefix("->") {
            tail
        } else {
            match after.find("-->") {
                Some(at) => &after[at + "-->".len()..],
                None => return false,
            }
        };
        rest = end.trim_start();
    }
    true
}

/// The contents of the `<!-- … -->` comments in `html`, naively (no tokenizer: for
/// diagnostics only). An unterminated comment runs to the end.
fn comment_contents(html: &str) -> impl Iterator<Item = &str> {
    html.split("<!--").skip(1).map(|chunk| {
        chunk
            .split_once("-->")
            .map_or(chunk, |(content, _)| content)
    })
}

/// The `Note:` markers (dialect §7), longest first.
const NOTE_MARKERS: [&str; 2] = ["Notes:", "Note:"];

/// The marker a top-level paragraph starts with, if it is a `Note:` paragraph: its first
/// inline is a `Text` starting with `Note:` or `Notes:` and, with the source (`lines`, see
/// [`split::source_lines`]), the first line of its text as typed starts with that marker too
/// (after spaces and tabs). So `Note\:` and `Note&#58;` (escape and entity, resolved in the
/// `Text`) are content, like `**Note:**`. Without the source (or when the paragraph lies
/// outside it) only the `Text` is read.
fn note_marker<'a>(node: &'a AstNode<'a>, lines: Option<&[&str]>) -> Option<&'static str> {
    if !matches!(node.data().value, NodeValue::Paragraph) {
        return None;
    }
    let typed = lines
        .and_then(|lines| split::first_text_line(node, lines))
        .map(|line| line.trim_start_matches([' ', '\t']));
    let first = node.first_child()?;
    let data = first.data();
    let text = data.value.text()?;
    NOTE_MARKERS.into_iter().find(|marker| {
        text.starts_with(marker) && typed.is_none_or(|typed| typed.starts_with(marker))
    })
}

/// reveal.js's test for a `data-background` value that is an image (its `Backgrounds`
/// module, 6.0.2): a URL starting with `http`, `file` or `//`, or a path with an image
/// extension (`/\.(svg|png|jpg|jpeg|gif|bmp|webp)([?#\s]|$)/i`).
fn is_image_background(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if ["http", "file", "//"].iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    const EXTENSIONS: [&str; 7] = ["svg", "png", "jpg", "jpeg", "gif", "bmp", "webp"];
    lower.match_indices('.').any(|(at, _)| {
        let rest = &lower[at + 1..];
        EXTENSIONS.iter().any(|ext| {
            rest.strip_prefix(ext).is_some_and(|after| {
                after.is_empty()
                    || after.starts_with(['?', '#'])
                    || after.starts_with(char::is_whitespace)
            })
        })
    })
}

/// The reveal.js form `data-background="images/bg.png"` (a `.slide:` comment's or a
/// separator's verbatim key, dialect §3.2): when the value is an image to reveal
/// ([`is_image_background`]), it moves to `data-background-image`, which reveal would set
/// from it at runtime (replacing an explicit one, so it does here too). Then the image is
/// registered as a local asset or reported missing like any `data-background-image`
/// (dialect §6), and still works once the URL is rewritten (a `data:` URI in standalone
/// output does not look like an image to reveal). Colors and anything else stay as they are.
pub fn image_background(attrs: &mut std::collections::BTreeMap<String, String>) {
    if attrs
        .get("data-background")
        .is_some_and(|value| is_image_background(value))
        && let Some(value) = attrs.remove("data-background")
    {
        attrs.insert("data-background-image".to_string(), value);
    }
}

/// Removes the `Note:` marker (and the whitespace or line break after it) from the start of a
/// `Note:` paragraph. Returns whether the paragraph still has content.
fn strip_note_marker<'a>(paragraph: &'a AstNode<'a>, marker: &str) -> bool {
    if let Some(first) = paragraph.first_child() {
        let emptied = {
            let mut data = first.data_mut();
            match data.value.text_mut() {
                Some(text) => {
                    let rest = text[marker.len()..].trim_start().to_string();
                    *text = rest.into();
                    text.is_empty()
                }
                None => false,
            }
        };
        if emptied {
            first.detach();
            let lead = paragraph.first_child();
            if lead.is_some_and(|n| {
                matches!(n.data().value, NodeValue::SoftBreak | NodeValue::LineBreak)
            }) {
                lead.into_iter().for_each(|n| n.detach());
            }
        }
    }
    paragraph.first_child().is_some()
}

/// Renders one deck slide ([`render_nodes`]) with the `Note:` convention (dialect §7): in
/// [`RenderMode::Deck`], the first top-level paragraph starting with `Note:` or `Notes:`
/// ([`note_marker`]; `lines` is the deck's source split by [`split::source_lines`], or `None`
/// to read the marker from the AST only) ends the body. That paragraph (without the marker;
/// dropped when nothing else is in it) and every later node are rendered as a second document
/// and become notes. Footnote definitions, which the splitter keeps last, go with the part
/// they were written in (by source line). The `Note:` part comes first in the slide's notes,
/// before the notes from `:::notes` directives (task T43: "prepend to existing notes from
/// directives"): those of the body, then those inside the `Note:` part, in document order.
/// The title comes from the body only.
///
/// Without such a paragraph, and always in [`RenderMode::Notebook`], this is exactly
/// [`render_nodes`]. Errors as [`render_nodes`].
pub fn render_slide<'a>(
    arena: &'a Arena<'a>,
    nodes: &[&'a AstNode<'a>],
    lines: Option<&[&str]>,
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<Rendered> {
    let found = match ctx.mode {
        RenderMode::Deck => nodes
            .iter()
            .enumerate()
            .find_map(|(at, node)| note_marker(node, lines).map(|marker| (at, marker))),
        RenderMode::Notebook => None,
    };
    let Some((at, marker)) = found else {
        return render_nodes(arena, nodes, opts, ctx);
    };
    let cut_line = nodes[at].data().sourcepos.start.line;
    tracing::debug!(
        line = cut_line,
        "`{marker}` paragraph: the rest of the slide is notes"
    );
    let keep_paragraph = strip_note_marker(nodes[at], marker);
    let (mut body, mut notes) = (Vec::new(), Vec::new());
    for (i, node) in nodes.iter().enumerate() {
        let to_notes = {
            let data = node.data();
            match data.value {
                NodeValue::FootnoteDefinition(_) => data.sourcepos.start.line >= cut_line,
                _ => i >= at,
            }
        };
        if i == at && !keep_paragraph {
            continue;
        }
        if to_notes {
            notes.push(*node);
        } else {
            body.push(*node);
        }
    }
    let mut rendered = render_nodes(arena, &body, opts, ctx)?;
    let part = render_nodes(arena, &notes, opts, ctx)?;
    let mut all = part.html;
    all.push_str(rendered.notes.as_deref().unwrap_or_default());
    all.push_str(part.notes.as_deref().unwrap_or_default());
    rendered.notes = (!all.trim().is_empty()).then_some(all);
    rendered.features.merge(part.features);
    rendered.features.notes = rendered.notes.is_some();
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::assets::registry::AssetRegistry;
    use crate::error::Diagnostics;
    use crate::markdown::comrak_options;

    #[test]
    fn slide_comments() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "<!-- .slide: data-background=\"#f00\" class=\"x\" -->\n",
                Some(" data-background=\"#f00\" class=\"x\" "),
            ),
            ("  <!--.slide:k=v-->", Some("k=v")),
            ("<!--\n.slide:\n  k=v\n-->\n", Some("\n  k=v\n")),
            ("<!-- .slide: -->", Some(" ")),
            ("<!-- slide: k=v -->", None),
            ("<!-- .slides: k=v -->", None),
            ("<!-- .element: k=v -->", None),
            ("<!-- .slide: k=v --> trailing\n", None),
            ("<!-- .slide: k=v --><!-- x -->\n", None),
            ("<div><!-- .slide: k=v --></div>", None),
        ];
        for (literal, expected) in cases {
            assert_eq!(slide_comment(literal), *expected, "{literal:?}");
        }
        assert!(mentions_slide_comment("<!-- .slide: k=v --> x"));
        assert!(mentions_slide_comment("<p>a</p><!--\n .slide: k=v"));
        assert!(!mentions_slide_comment("<!-- see the .slide: docs -->"));
    }

    #[test]
    fn element_comments() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                " .element: class=\"fragment\" ",
                Some(" class=\"fragment\" "),
            ),
            (".element:class=x", Some("class=x")),
            (" .element class=\"x\" ", Some(" class=\"x\" ")),
            (" .elements: x ", None),
            (" .elementx ", None),
            (" .slide: x ", None),
            (" plain ", None),
        ];
        for (content, expected) in cases {
            assert_eq!(element_comment(content), *expected, "{content:?}");
        }
    }

    #[test]
    fn comment_only_html() {
        for (html, blank) in [
            ("<!-- a -->\n", true),
            ("  <!-- a\nb -->  <!-- c -->\n", true),
            ("<!---->", true),
            ("<!-->", true),
            ("<!--->\n<!-- x -->", true),
            ("<!-- a --> b\n", false),
            ("<!-- unterminated\n", false),
            ("<div></div>\n", false),
        ] {
            assert_eq!(is_comment_only(html), blank, "{html:?}");
        }
    }

    /// [`render_slide`] on the whole of `text`, with its source lines.
    fn render_deck(text: &str, mode: RenderMode) -> Rendered {
        render_with(text, mode, true)
    }

    /// [`render_slide`] on the whole of `text`; with `source`, its lines are passed.
    fn render_with(text: &str, mode: RenderMode, source: bool) -> Rendered {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md");
        let (mut assets, mut diagnostics) = (AssetRegistry::new(), Diagnostics::new());
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        let opts = comrak_options(&Default::default());
        let arena = Arena::new();
        let root = comrak::parse_document(&arena, text, &opts);
        let nodes: Vec<_> = root.children().collect();
        let lines = split::source_lines(text);
        let lines = source.then_some(lines.as_slice());
        let rendered = render_slide(&arena, &nodes, lines, &opts, &mut ctx).unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        rendered
    }

    #[test]
    fn note_paragraph_cuts_the_body() {
        let text = "# Title\n\nBody.\n\n:::notes\nFirst.\n:::\n\nNote: Say *this*.\n\n- and\n- this\n\n\
                    :::notes\nNested.\n:::\n";
        let rendered = render_deck(text, RenderMode::Deck);
        assert_eq!(rendered.html, "<h1>Title</h1>\n<p>Body.</p>\n");
        // the `Note:` part first, then the directive notes in document order (task T43:
        // "prepend to existing notes from directives")
        assert_eq!(
            rendered.notes.as_deref(),
            Some(
                "<p>Say <em>this</em>.</p>\n<ul>\n<li>and</li>\n<li>this</li>\n</ul>\n\
                 <p>First.</p>\n<p>Nested.</p>\n"
            )
        );
        assert_eq!(rendered.title.as_deref(), Some("Title"));
        assert!(rendered.features.notes);
    }

    #[test]
    fn note_marker_variants() {
        // (text, body html, notes html)
        let cases: &[(&str, &str, Option<&str>)] = &[
            (
                "A\n\nNotes:\nline two\n",
                "<p>A</p>\n",
                Some("<p>line two</p>\n"),
            ),
            (
                "A\n\nNote:\n\n- x\n",
                "<p>A</p>\n",
                Some("<ul>\n<li>x</li>\n</ul>\n"),
            ),
            ("Note: only notes\n", "", Some("<p>only notes</p>\n")),
            ("A\n\nNote:\n", "<p>A</p>\n", None),
            // not at the start of a paragraph, lowercase, emphasised, nested: content
            ("A\nNote: x\n", "<p>A\nNote: x</p>\n", None),
            ("note: x\n", "<p>note: x</p>\n", None),
            ("**Note:** x\n", "<p><strong>Note:</strong> x</p>\n", None),
            (
                "> Note: x\n",
                "<blockquote>\n<p>Note: x</p>\n</blockquote>\n",
                None,
            ),
            ("Notebook: x\n", "<p>Notebook: x</p>\n", None),
            // the marker as typed: an escaped colon or an entity is content
            ("A\n\nNote\\: x\n", "<p>A</p>\n<p>Note: x</p>\n", None),
            ("Notes&#58; x\n", "<p>Notes: x</p>\n", None),
            // `\N` is no escape: the backslash stays, so it is no marker either
            ("\\Note: x\n", "<p>\\Note: x</p>\n", None),
            // indented up to three spaces, or after a link reference definition: a marker
            ("A\n\n   Note: x\n", "<p>A</p>\n", Some("<p>x</p>\n")),
            (
                "A\n\n[r]: http://a\nNote: see [r]\n",
                "<p>A</p>\n",
                Some("<p>see <a href=\"http://a\">r</a></p>\n"),
            ),
        ];
        for (text, html, notes) in cases {
            let rendered = render_deck(text, RenderMode::Deck);
            assert_eq!(rendered.html, *html, "{text:?}");
            assert_eq!(rendered.notes.as_deref(), *notes, "{text:?}");
            assert_eq!(rendered.features.notes, notes.is_some(), "{text:?}");
        }
        // without the source only the resolved text can be read
        let rendered = render_with("A\n\nNote\\: x\n", RenderMode::Deck, false);
        assert_eq!(rendered.notes.as_deref(), Some("<p>x</p>\n"));
    }

    #[test]
    fn image_like_data_background_becomes_an_image() {
        let cases: &[(&str, Option<&str>, Option<&str>)] = &[
            // (data-background, kept as data-background, moved to data-background-image)
            ("#f00", Some("#f00"), None),
            ("rgb(1, 2, 3)", Some("rgb(1, 2, 3)"), None),
            (
                "linear-gradient(red, blue)",
                Some("linear-gradient(red, blue)"),
                None,
            ),
            ("images/bg.png", None, Some("images/bg.png")),
            ("bg.JPEG?v=2", None, Some("bg.JPEG?v=2")),
            ("a.svg#frag", None, Some("a.svg#frag")),
            (
                "https://example.com/bg",
                None,
                Some("https://example.com/bg"),
            ),
            ("//cdn/bg", None, Some("//cdn/bg")),
            ("photo.pngx", Some("photo.pngx"), None),
            // reveal's own test: anything starting with `file` is an image to it
            ("filed.css", None, Some("filed.css")),
            ("png", Some("png"), None),
        ];
        for (value, kept, moved) in cases {
            let mut attrs = std::collections::BTreeMap::from([
                ("data-background".to_string(), value.to_string()),
                (
                    "data-background-image".to_string(),
                    "explicit.png".to_string(),
                ),
            ]);
            image_background(&mut attrs);
            assert_eq!(
                attrs.get("data-background").map(String::as_str),
                *kept,
                "{value}"
            );
            let image = attrs.get("data-background-image").map(String::as_str);
            assert_eq!(image, Some(moved.unwrap_or("explicit.png")), "{value}");
        }
        // end to end: a missing image is reported like any `data-background-image`
        use crate::config::Config;
        use crate::ir::{SourceInfo, SourceKind};
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md");
        let source = SourceInfo {
            path: dir.join("deck.md"),
            kind: SourceKind::Markdown,
            base_dir: dir,
        };
        let text = "<!-- .slide: data-background=\"images/missing.png\" -->\n\n# A\n\n---\n\n\
                    # B {data-background=\"images/ok.png\"}\n";
        let parsed = crate::markdown::parse(&source, text, &Config::default()).unwrap();
        let slides = &parsed.deck.slides;
        assert_eq!(
            slides[0].attrs["data-background-image"],
            "images/missing.png"
        );
        assert!(slides[1].attrs["data-background-image"].starts_with("asset:"));
        let codes: Vec<_> = parsed.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["missing-asset"]);
    }

    #[test]
    fn note_paragraph_is_content_in_notebook_mode() {
        let rendered = render_deck("A\n\nNote: x\n", RenderMode::Notebook);
        assert_eq!(rendered.html, "<p>A</p>\n<p>Note: x</p>\n");
        assert_eq!(rendered.notes, None);
    }

    #[test]
    fn footnotes_follow_the_part_they_are_written_in() {
        let text = "A[^a]\n\n[^a]: body note\n\nNote: B[^b]\n\n[^b]: notes note\n";
        let rendered = render_deck(text, RenderMode::Deck);
        assert!(rendered.html.contains("body note"), "{}", rendered.html);
        assert!(!rendered.html.contains("notes note"), "{}", rendered.html);
        let notes = rendered.notes.unwrap();
        assert!(
            notes.starts_with("<p>B<sup class=\"footnote-ref\">"),
            "{notes}"
        );
        assert!(notes.contains("notes note") && !notes.contains("body note"));
    }

    /// T43 acceptance criteria on `tests/fixtures/md/reveal-compat.md`, end to end.
    #[test]
    fn reveal_compat_fixture() {
        use crate::config::Config;
        use crate::ir::{Block, SourceInfo, SourceKind};
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md");
        let path = dir.join("reveal-compat.md");
        let text = std::fs::read_to_string(&path).unwrap();
        let source = SourceInfo {
            path,
            kind: SourceKind::Markdown,
            base_dir: dir,
        };
        let parsed = crate::markdown::parse(&source, &text, &Config::default()).unwrap();
        let slides = &parsed.deck.slides;
        let html = |slide: &crate::ir::Slide| match slide.blocks.as_slice() {
            [Block::Html { html }] => html.clone(),
            other => panic!("{other:?}"),
        };
        // `<!-- .slide: data-background="#f00" class="x" -->`: verbatim key, class x
        assert_eq!(slides[0].attrs["data-background"], "#f00");
        assert_eq!(slides[0].classes, ["x"]);
        assert!(!html(&slides[0]).contains("<!--"));
        // keys go through normalize_key: the shorthand works
        assert_eq!(slides[1].attrs["data-background-color"], "navy");
        assert_eq!(slides[1].id.as_deref(), Some("compat"));
        // precedence: separator < comment < heading
        assert_eq!(slides[2].attrs["data-state"], "comment");
        assert_eq!(slides[2].attrs["data-x"], "heading");
        assert_eq!(slides[2].classes, ["from-separator", "from-comment"]);
        assert_eq!(slides[2].children[0].attrs["data-auto-animate"], "");
        // `Note:` splits the notes off
        let notes = slides[3].notes.as_deref().unwrap();
        assert!(notes.starts_with("<p>These are"), "{notes}");
        assert!(
            notes.ends_with("<p>Directive notes come after the <code>Note:</code> part.</p>\n")
        );
        assert!(!html(&slides[3]).contains("speaker notes"));
        assert!(html(&slides[3]).contains("<strong>Note:</strong> emphasised"));
        // `.element:` on a list item and on an image
        let elements = html(&slides[4]);
        assert!(
            elements.contains("<li class=\"fragment\">item </li>"),
            "{elements}"
        );
        assert!(
            regex::Regex::new(r#"<img src="asset:[0-9a-f]{12}" alt="" width="300" />"#)
                .unwrap()
                .is_match(&elements),
            "{elements}"
        );
        assert!(!elements.contains(".element"), "{elements}");
        // a plain comment first: the heading's attributes are lifted, the `.slide:` comment
        // under it is read, and its image-like `data-background` is an asset
        let last = &slides[5];
        assert_eq!(last.id.as_deref(), Some("comment-first"));
        assert_eq!(last.classes, ["wide"]);
        assert!(
            !last.attrs.contains_key("data-background"),
            "{:?}",
            last.attrs
        );
        assert!(last.attrs["data-background-image"].starts_with("asset:"));
        let body = html(last);
        assert!(
            body.starts_with("<!-- TODO: a plain comment first"),
            "{body}"
        );
        assert!(body.contains("<h2>Comment first</h2>\n<p>"), "{body}");
        assert!(body.contains("<p>Note: an escaped colon"), "{body}");
        assert_eq!(last.notes, None);
        let found: Vec<_> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
            .collect();
        assert_eq!(
            found,
            [
                ("misplaced-slide-comment", Some(57)),
                ("misplaced-slide-comment", Some(105))
            ]
        );
    }

    #[test]
    fn math_and_code_in_notes_count_as_features() {
        let rendered = render_deck("A\n\nNote: $x$\n\n```\ncode\n```\n", RenderMode::Deck);
        assert!(rendered.features.math && rendered.features.code && rendered.features.notes);
        assert_eq!(rendered.html, "<p>A</p>\n");
    }
}
