//! reveal.js compatibility: `<!-- .slide: -->` and `<!-- .element: -->` comments and the
//! `Note:` convention. Implemented by T43.
//!
//! Contract: `docs/spec/markdown-dialect.md` §2.2 (row 4), §3.2 (source 2), §3.3, §7 and §9.
//! Everything here is opt-in by syntax: a deck without these comments or a `Note:` paragraph
//! renders exactly as before.
//!
//! - **`<!-- .slide: … -->`** (slide attributes): a top-level HTML block that is exactly one
//!   comment whose text starts with `.slide:` ([`slide_comment`]). The splitter
//!   ([`split`](crate::markdown::split)) reads it when it is the slide's first node, or the node
//!   right after the slide's first heading (the heading is the boundary in headings mode, and
//!   reveal decks often put the comment under the title). The text goes through
//!   [`parse_attr_block`](crate::markdown::attrs::parse_attr_block) and
//!   [`SlideAttrs::from_parsed`](crate::markdown::attrs::SlideAttrs::from_parsed), so it takes
//!   `k="v"`, `k=v`, `k='v'`, bare keys, `.class` and `#id`, and keys are normalized like every
//!   other slide attribute (`data-background="#f00"` stays verbatim, the `background=`
//!   shorthand works too). The comment is removed from the slide. Its attributes come after
//!   the separator's and before the lifted heading attributes (§3.2). Several such comments in
//!   a row are all read, in order. One anywhere else stays a plain comment and is reported as
//!   `misplaced-slide-comment`.
//! - **`<!-- .element: … -->`** (element attributes, [`element_comment`]): applied to the
//!   rendered HTML by [`postprocess`](crate::markdown::postprocess) the way reveal's Markdown
//!   plugin does: to the nearest preceding sibling element (`<br>` skipped), or else to the
//!   enclosing element. So `- item <!-- .element: class="fragment" -->` sets the `<li>`'s
//!   class, `![](x.png) <!-- .element: width="300" -->` the `<img>`'s width, and a comment on
//!   its own line after a list or paragraph the `<ul>`/`<p>` before it.
//! - **`Note:`** (speaker notes, §7): in [`RenderMode::Deck`] a top-level paragraph whose text
//!   starts with `Note:` or `Notes:` (exactly so, as the spec lists them) ends the slide body:
//!   it, without the marker, and every later node become notes ([`render_slide`]).

use comrak::Arena;
use comrak::Options;
use comrak::nodes::{AstNode, NodeValue};

use crate::error::Result;
use crate::markdown::render::{RenderCtx, RenderMode, Rendered, render_nodes};

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
/// inline is a `Text` starting with `Note:` or `Notes:`. Markdown escapes and entities are
/// already resolved there, so `**Note:**` (emphasis) is the way to show the word as content.
fn note_marker<'a>(node: &'a AstNode<'a>) -> Option<&'static str> {
    if !matches!(node.data().value, NodeValue::Paragraph) {
        return None;
    }
    let first = node.first_child()?;
    let data = first.data();
    let text = data.value.text()?;
    NOTE_MARKERS
        .into_iter()
        .find(|marker| text.starts_with(marker))
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
/// [`RenderMode::Deck`], the first top-level paragraph starting with `Note:` or `Notes:` ends
/// the body. That paragraph (without the marker; dropped when nothing else is in it) and every
/// later node are rendered as a second document and become notes. Footnote definitions, which
/// the splitter keeps last, go with the part they were written in (by source line). The
/// slide's notes are, in document order, the `:::notes` blocks of the body, then the `Note:`
/// part, then any `:::notes` blocks inside that part. The title comes from the body only.
///
/// Without such a paragraph, and always in [`RenderMode::Notebook`], this is exactly
/// [`render_nodes`]. Errors as [`render_nodes`].
pub fn render_slide<'a>(
    arena: &'a Arena<'a>,
    nodes: &[&'a AstNode<'a>],
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<Rendered> {
    let found = match ctx.mode {
        RenderMode::Deck => nodes
            .iter()
            .enumerate()
            .find_map(|(at, node)| note_marker(node).map(|marker| (at, marker))),
        RenderMode::Notebook => None,
    };
    let Some((at, marker)) = found else {
        return render_nodes(arena, nodes, opts, ctx);
    };
    let cut_line = nodes[at].data().sourcepos.start.line;
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
    let mut all = rendered.notes.take().unwrap_or_default();
    all.push_str(&part.html);
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

    fn render_deck(text: &str, mode: RenderMode) -> Rendered {
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
        let rendered = render_slide(&arena, &nodes, &opts, &mut ctx).unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        rendered
    }

    #[test]
    fn note_paragraph_cuts_the_body() {
        let text = "# Title\n\nBody.\n\n:::notes\nFirst.\n:::\n\nNote: Say *this*.\n\n- and\n- this\n\n\
                    :::notes\nNested.\n:::\n";
        let rendered = render_deck(text, RenderMode::Deck);
        assert_eq!(rendered.html, "<h1>Title</h1>\n<p>Body.</p>\n");
        assert_eq!(
            rendered.notes.as_deref(),
            Some(
                "<p>First.</p>\n<p>Say <em>this</em>.</p>\n<ul>\n<li>and</li>\n<li>this</li>\n\
                 </ul>\n<p>Nested.</p>\n"
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
        ];
        for (text, html, notes) in cases {
            let rendered = render_deck(text, RenderMode::Deck);
            assert_eq!(rendered.html, *html, "{text:?}");
            assert_eq!(rendered.notes.as_deref(), *notes, "{text:?}");
            assert_eq!(rendered.features.notes, notes.is_some(), "{text:?}");
        }
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
        assert!(notes.starts_with("<p>Directive notes come first.</p>\n<p>These are"));
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
        let found: Vec<_> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
            .collect();
        assert_eq!(found, [("misplaced-slide-comment", Some(52))]);
    }

    #[test]
    fn math_and_code_in_notes_count_as_features() {
        let rendered = render_deck("A\n\nNote: $x$\n\n```\ncode\n```\n", RenderMode::Deck);
        assert!(rendered.features.math && rendered.features.code && rendered.features.notes);
        assert_eq!(rendered.html, "<p>A</p>\n");
    }
}
