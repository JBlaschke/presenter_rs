//! Slide splitter: `---` / `--` separators, slide attributes, heading lifting. Implemented by T11 (headings mode: T43).
//!
//! Contract: `docs/spec/markdown-dialect.md` §2.2 (separators, rules mode) and §3.2 (where
//! slide attributes come from). Splitting is decided on the comrak AST, looking only at the
//! `Document`'s direct children, so `---`/`--` inside code fences, lists, block quotes or
//! directives are never separators. Nothing is rendered here (T12 renders each [`RawSlide`]).
//!
//! Rules implemented by [`split`]:
//! - `FrontMatter` is skipped (neither a slide nor a separator).
//! - `ThematicBreak` (`---`, `***`, `___`) opens a horizontal slide; a `Paragraph` whose only
//!   child is `Text("--")` opens a vertical slide under the current horizontal one; a
//!   `Paragraph` whose text matches `^(---|--)\s*\{(.*)\}$` does the same with attributes
//!   ([`parse_attr_block`]) for the slide it opens.
//! - Content before the first separator is slide 1. When there is no such content the first
//!   separator opens slide 1 itself (a leading `--` has no horizontal slide to attach to: it
//!   is treated as horizontal and reported as `orphan-vertical`). A document without
//!   separators or content yields one empty slide.
//! - Two consecutive separators yield an empty slide (not collapsed; `empty-slide` warning).
//! - A horizontal separator closes an open vertical stack.
//! - Attributes on a slide's first node, when it is a heading, are lifted onto the slide
//!   ([`lift_heading_attrs`]); they win over the separator's attributes.

use std::path::PathBuf;

use comrak::nodes::{AstNode, NodeValue};

use crate::error::{Diagnostics, Error, Result};
use crate::ir::SourceSpan;
use crate::markdown::attrs::{ParsedAttrs, SlideAttrs, parse_attr_block};

/// How slide boundaries are found (`slides.split` in the config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitMode {
    /// `---` / `--` separator lines (dialect §2.2); the default.
    Rules,
    /// Heading-driven boundaries (dialect §2.3). Implemented in T43; [`split`] currently
    /// returns `Error::Other("headings mode: T43")`.
    Headings {
        /// Heading level that starts a vertical slide (`slides.slide_level`).
        slide_level: u8,
    },
}

/// One slide's top-level nodes, before rendering.
#[derive(Debug)]
pub struct RawSlide<'a> {
    /// Separator attributes merged with the lifted first-heading attributes (heading wins).
    pub attrs: SlideAttrs,
    /// The slide's top-level AST nodes in document order, separators excluded.
    pub nodes: Vec<&'a AstNode<'a>>,
    /// 1-based first line: the line of the separator that opened the slide; for a slide not
    /// opened by a separator (content before the first separator), its first node's line.
    pub start_line: usize,
    /// 1-based last line: the last node's last line, or the opening separator's last line
    /// when the slide has no nodes.
    pub end_line: usize,
}

impl RawSlide<'_> {
    fn empty(line: usize) -> Self {
        RawSlide {
            attrs: SlideAttrs::default(),
            nodes: Vec::new(),
            start_line: line,
            end_line: line,
        }
    }

    /// `start_line..=end_line` as a Markdown [`SourceSpan`] (`cell: None`).
    pub fn span(&self) -> SourceSpan {
        SourceSpan {
            start_line: self.start_line,
            end_line: self.end_line,
            cell: None,
        }
    }
}

/// The split document: horizontal slides with their vertical children, plus diagnostics.
#[derive(Debug)]
pub struct RawDeck<'a> {
    /// `(horizontal slide, its vertical children)` in document order.
    pub slides: Vec<(RawSlide<'a>, Vec<RawSlide<'a>>)>,
    /// `empty-slide`, `orphan-vertical` and `unknown-slide-attr` warnings (span set, no path).
    pub diagnostics: Diagnostics,
}

/// Splits a parsed document (`root` is comrak's `Document` node) into slides.
///
/// Heading attributes that are lifted onto a slide are removed from the heading node
/// (`attrs = None`), so the rendered `<h1>` is clean.
///
/// Errors: `Error::Other("headings mode: T43")` for [`SplitMode::Headings`]; and
/// `Error::Parse { path: <empty>, line: Some(separator line), .. }` when a separator's
/// attribute block has an unbalanced quote. `split` does not know the file, so the caller
/// fills in `path`.
pub fn split<'a>(root: &'a AstNode<'a>, mode: SplitMode) -> Result<RawDeck<'a>> {
    if let SplitMode::Headings { .. } = mode {
        return Err(Error::Other("headings mode: T43".into()));
    }
    let mut builder = Builder::new();
    for node in root.children() {
        builder.visit(node)?;
    }
    Ok(builder.finish())
}

/// If `nodes[0]` is a Heading with attributes, remove them from the node and return them.
pub fn lift_heading_attrs<'a>(nodes: &[&'a AstNode<'a>]) -> Option<ParsedAttrs> {
    let mut data = nodes.first()?.data_mut();
    if !matches!(data.value, NodeValue::Heading(_)) {
        return None;
    }
    let attrs = data.attrs.take()?;
    Some(ParsedAttrs::from(&*attrs))
}

/// Plain text of the first heading among `nodes` (concatenate Text/Code descendants).
///
/// Only the given (top-level) nodes are searched, not their descendants. Emphasis, links and
/// other inline containers contribute their text; code spans their literal; soft and hard
/// line breaks (multi-line setext headings) become a space; everything else (math, raw HTML,
/// images) is skipped. The result is trimmed; `None` when there is no heading or the first
/// heading has no text.
pub fn first_heading_text<'a>(nodes: &[&'a AstNode<'a>]) -> Option<String> {
    let heading = nodes
        .iter()
        .find(|node| matches!(node.data().value, NodeValue::Heading(_)))?;
    let mut text = String::new();
    for node in heading.descendants() {
        match &node.data().value {
            NodeValue::Text(t) => text.push_str(t),
            NodeValue::Code(code) => text.push_str(&code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => text.push(' '),
            _ => {}
        }
    }
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Horizontal,
    Vertical,
}

/// Classifies a top-level node as a separator (dialect §2.2): its direction and, for the
/// `--- {…}` form, the text between the braces.
fn separator<'a>(node: &'a AstNode<'a>) -> Option<(Direction, Option<String>)> {
    match node.data().value {
        NodeValue::ThematicBreak => return Some((Direction::Horizontal, None)),
        NodeValue::Paragraph => {}
        _ => return None,
    }
    if is_bare_vertical(node) {
        return Some((Direction::Vertical, None));
    }
    let text = separator_text(node)?;
    let (direction, inner) = match_attr_separator(&text)?;
    Some((direction, Some(inner.to_string())))
}

/// A `Paragraph` whose sole child is `Text` equal to `--`.
fn is_bare_vertical<'a>(paragraph: &'a AstNode<'a>) -> bool {
    let Some(child) = paragraph.first_child() else {
        return false;
    };
    child.next_sibling().is_none() && matches!(&child.data().value, NodeValue::Text(t) if t == "--")
}

/// The text of a paragraph that could be a `--- {…}` separator: its `Text` nodes, with line
/// breaks as `\n` and autolinked URLs (`Link` nodes) contributing their text. Any other
/// inline (code span, emphasis, raw HTML, math, …) means the paragraph is ordinary content,
/// so e.g. a code span showing `` `-- {auto-animate}` `` is never a separator.
fn separator_text<'a>(paragraph: &'a AstNode<'a>) -> Option<String> {
    let mut text = String::new();
    for node in paragraph.descendants().skip(1) {
        match &node.data().value {
            NodeValue::Text(t) => text.push_str(t),
            NodeValue::SoftBreak | NodeValue::LineBreak => text.push('\n'),
            NodeValue::Link(_) => {}
            _ => return None,
        }
    }
    Some(text)
}

/// `^(---|--)\s*\{(.*)\}$` (`.` excludes `\n`, `\s` is Unicode whitespace as in the `regex`
/// crate): returns the direction and the brace contents.
fn match_attr_separator(text: &str) -> Option<(Direction, &str)> {
    let (direction, rest) = if let Some(rest) = text.strip_prefix("---") {
        (Direction::Horizontal, rest)
    } else {
        (Direction::Vertical, text.strip_prefix("--")?)
    };
    let inner = rest.trim_start().strip_prefix('{')?.strip_suffix('}')?;
    (!inner.contains('\n')).then_some((direction, inner))
}

/// Accumulates slides while walking the document's top-level nodes.
struct Builder<'a> {
    /// Finished horizontal slides with their children.
    done: Vec<(RawSlide<'a>, Vec<RawSlide<'a>>)>,
    /// The open horizontal slide and its children; nodes go to its last child, if any.
    current: (RawSlide<'a>, Vec<RawSlide<'a>>),
    /// `current.0` is slide 1 and no separator has been seen yet.
    implicit: bool,
    /// Footnote definitions (comrak moves them to the end of the document), with their lines.
    footnotes: Vec<(&'a AstNode<'a>, usize, usize)>,
    diagnostics: Diagnostics,
}

impl<'a> Builder<'a> {
    fn new() -> Self {
        Builder {
            done: Vec::new(),
            current: (RawSlide::empty(1), Vec::new()),
            implicit: true,
            footnotes: Vec::new(),
            diagnostics: Diagnostics::new(),
        }
    }

    fn visit(&mut self, node: &'a AstNode<'a>) -> Result<()> {
        let (start, end) = {
            let data = node.data();
            let lines = (data.sourcepos.start.line, data.sourcepos.end.line);
            match data.value {
                NodeValue::FrontMatter(_) => {
                    // Always the first child: until content arrives, slide 1 points at the
                    // front matter's last line.
                    self.current.0 = RawSlide::empty(lines.1.max(1));
                    return Ok(());
                }
                NodeValue::FootnoteDefinition(_) => {
                    self.footnotes.push((node, lines.0, lines.1));
                    return Ok(());
                }
                _ => lines,
            }
        };
        let Some((direction, attr_text)) = separator(node) else {
            self.push(node, start, end);
            return Ok(());
        };
        let attrs = match attr_text {
            Some(text) => {
                SlideAttrs::from_parsed(parse_attr_block(&text).map_err(|err| Error::Parse {
                    path: PathBuf::new(),
                    line: Some(start),
                    message: err.to_string(),
                })?)
            }
            None => SlideAttrs::default(),
        };
        let slide = RawSlide {
            attrs,
            nodes: Vec::new(),
            start_line: start,
            end_line: end,
        };
        self.open(direction, slide);
        Ok(())
    }

    /// Starts the slide a separator opened.
    fn open(&mut self, direction: Direction, slide: RawSlide<'a>) {
        if std::mem::take(&mut self.implicit) && self.current.0.nodes.is_empty() {
            // No content before the first separator: it opens slide 1 itself.
            if direction == Direction::Vertical {
                self.diagnostics
                    .warn(
                        "orphan-vertical",
                        "vertical separator `--` before any horizontal slide; treated as `---`",
                    )
                    .span = Some(slide.span());
            }
            self.current.0 = slide;
            return;
        }
        match direction {
            Direction::Horizontal => {
                let previous = std::mem::replace(&mut self.current, (slide, Vec::new()));
                self.done.push(previous);
            }
            Direction::Vertical => self.current.1.push(slide),
        }
    }

    /// Adds a content node to the open slide (the last vertical child, if any).
    fn push(&mut self, node: &'a AstNode<'a>, start: usize, end: usize) {
        let first_content = self.implicit && self.current.0.nodes.is_empty();
        let slide = match self.current.1.last_mut() {
            Some(child) => child,
            None => &mut self.current.0,
        };
        if first_content {
            slide.start_line = start;
        }
        slide.nodes.push(node);
        slide.end_line = slide.end_line.max(end);
    }

    fn finish(self) -> RawDeck<'a> {
        let Builder {
            mut done,
            current,
            footnotes,
            mut diagnostics,
            ..
        } = self;
        done.push(current);
        for (node, start, end) in footnotes {
            place_footnote(&mut done, node, start, end);
        }
        for (i, (slide, children)) in done.iter_mut().enumerate() {
            finish_slide(slide, &(i + 1).to_string(), &mut diagnostics);
            for (j, child) in children.iter_mut().enumerate() {
                finish_slide(child, &format!("{}.{}", i + 1, j + 1), &mut diagnostics);
            }
        }
        RawDeck {
            slides: done,
            diagnostics,
        }
    }
}

/// Gives a footnote definition back to the slide it was written in (the last slide starting
/// at or before its line; slide 1 if none). comrak moves every referenced definition to the
/// end of the document, which would otherwise put all of them on the last slide.
fn place_footnote<'a>(
    slides: &mut [(RawSlide<'a>, Vec<RawSlide<'a>>)],
    node: &'a AstNode<'a>,
    start: usize,
    end: usize,
) {
    let mut target = (0, None);
    for (i, (slide, children)) in slides.iter().enumerate() {
        if slide.start_line <= start {
            target = (i, None);
        }
        for (j, child) in children.iter().enumerate() {
            if child.start_line <= start {
                target = (i, Some(j));
            }
        }
    }
    let Some((slide, children)) = slides.get_mut(target.0) else {
        return;
    };
    let slide = match target.1.and_then(|j| children.get_mut(j)) {
        Some(child) => child,
        None => slide,
    };
    slide.nodes.push(node);
    slide.end_line = slide.end_line.max(end);
}

/// Lifts the first heading's attributes and reports `empty-slide` / `unknown-slide-attr`.
fn finish_slide(slide: &mut RawSlide<'_>, label: &str, diagnostics: &mut Diagnostics) {
    if let Some(lifted) = lift_heading_attrs(&slide.nodes) {
        slide.attrs.merge(SlideAttrs::from_parsed(lifted));
    }
    let span = slide.span();
    // A slide with attributes but no content (e.g. a full-bleed background) is intentional.
    if slide.nodes.is_empty() && slide.attrs.is_empty() {
        diagnostics
            .warn("empty-slide", format!("slide {label} is empty"))
            .span = Some(span.clone());
    }
    for key in &slide.attrs.unknown_keys {
        diagnostics
            .warn(
                "unknown-slide-attr",
                format!("unknown slide attribute `{key}` on slide {label} (kept verbatim)"),
            )
            .span = Some(span.clone());
    }
}

/// comrak options exactly as `docs/spec/markdown-dialect.md` §1 with the config defaults
/// (`smart_punctuation = false`, `sourcepos = false`). Test-only: the production builder is
/// `markdown::comrak_options` (T12).
#[cfg(test)]
pub(crate) fn dialect_options() -> comrak::Options<'static> {
    let mut o = comrak::Options::default();
    o.extension.front_matter_delimiter = Some("---".into());
    o.extension.table = true;
    o.extension.strikethrough = true;
    o.extension.tasklist = true;
    o.extension.autolink = true;
    o.extension.footnotes = true;
    o.extension.description_lists = true;
    o.extension.alerts = true;
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    o.extension.block_directive = true;
    o.extension.header_attributes = true;
    o.extension.fenced_code_attributes = true;
    o.extension.link_attributes = true;
    o.extension.inline_code_attributes = true;
    o.parse.smart = false;
    o.parse.relaxed_autolinks = false;
    o.render.r#unsafe = true;
    o.render.escape = false;
    o.render.hardbreaks = false;
    o.render.github_pre_lang = false;
    o.render.full_info_string = false;
    o.render.sourcepos = false;
    o
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use comrak::nodes::NodeHeading;
    use comrak::{Arena, parse_document};

    use super::*;
    use crate::ir::{Deck, DeckMeta, Features, Slide, SourceInfo, SourceKind};

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/md")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn parse<'a>(arena: &'a Arena<'a>, text: &str) -> &'a AstNode<'a> {
        parse_document(arena, text, &dialect_options())
    }

    fn split_text<'a>(arena: &'a Arena<'a>, text: &str) -> RawDeck<'a> {
        split(parse(arena, text), SplitMode::Rules).unwrap()
    }

    /// `start-end` per horizontal slide, children in brackets, slides separated by ` | `.
    fn shape(deck: &RawDeck<'_>) -> String {
        let lines = |s: &RawSlide<'_>| format!("{}-{}", s.start_line, s.end_line);
        deck.slides
            .iter()
            .map(|(slide, children)| {
                if children.is_empty() {
                    lines(slide)
                } else {
                    let children: Vec<String> = children.iter().map(lines).collect();
                    format!("{} [{}]", lines(slide), children.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// `(code, span.start_line)` of every diagnostic, in order.
    fn diags(deck: &RawDeck<'_>) -> Vec<(&'static str, usize)> {
        deck.diagnostics
            .iter()
            .map(|d| {
                let span = d.span.as_ref().expect("every split diagnostic has a span");
                assert_eq!(span.cell, None);
                (d.code, span.start_line)
            })
            .collect()
    }

    fn flatten<'d, 'a>(deck: &'d RawDeck<'a>) -> Vec<&'d RawSlide<'a>> {
        deck.slides
            .iter()
            .flat_map(|(slide, children)| std::iter::once(slide).chain(children))
            .collect()
    }

    fn line_range(node: &AstNode<'_>) -> (usize, usize) {
        let pos = node.data().sourcepos;
        (pos.start.line, pos.end.line)
    }

    /// Expected `(code, span.start_line)` diagnostics.
    type Diags = &'static [(&'static str, usize)];

    /// Expected shapes (from each fixture's "Expected shape" comment; line numbers read off
    /// comrak's sourcepos) and diagnostics.
    const FIXTURE_SHAPES: &[(&str, &str, Diags)] = &[
        ("minimal.md", "1-15", &[]),
        (
            "separators.md",
            "22-24 | 26-30 [32-36, 38-42] | 44-78 | 80-80 | 81-85 | 87-91 | 93-93 [95-100]",
            &[("empty-slide", 80), ("empty-slide", 93)],
        ),
        (
            "attributes.md",
            "27-29 | 31-35 | 37-41 [43-47] | 49-53 | 55-59 | 61-65 | 67-71 | 73-82",
            &[("unknown-slide-attr", 49)],
        ),
        (
            "directives.md",
            "23-29 | 31-45 | 47-58 | 60-78 | 80-86",
            &[],
        ),
        (
            "code.md",
            "22-27 | 29-35 | 37-46 | 48-62 | 64-71 | 73-79 | 81-87 | 89-95 | 97-103 | 105-111 \
             | 113-122",
            &[],
        ),
        ("math.md", "15-23 | 25-38", &[]),
        (
            "images.md",
            "22-26 | 28-32 | 34-41 | 43-47 | 49-58 | 60-64 | 66-70",
            &[],
        ),
        ("notes.md", "15-34 | 36-40 [42-50]", &[]),
    ];

    #[test]
    fn fixtures_split_into_the_expected_shapes() {
        for (name, expected_shape, expected_diags) in FIXTURE_SHAPES {
            let text = fixture(name);
            let arena = Arena::new();
            let deck = split_text(&arena, &text);
            assert_eq!(shape(&deck), *expected_shape, "{name}");
            assert_eq!(diags(&deck), *expected_diags, "{name}");
        }
    }

    /// Spans are 1-based and come from comrak's sourcepos: every separator node's line
    /// starts exactly one slide, a slide not opened by a separator starts at its first node,
    /// and every slide ends at its last node (or at its separator when empty).
    #[test]
    fn fixture_spans_match_sourcepos() {
        for (name, _, _) in FIXTURE_SHAPES {
            let text = fixture(name);
            let arena = Arena::new();
            let root = parse(&arena, &text);
            let separator_lines: Vec<(usize, usize)> = root
                .children()
                .filter(|node| separator(node).is_some())
                .map(line_range)
                .collect();
            let deck = split(root, SplitMode::Rules).unwrap();
            let slides = flatten(&deck);
            let opened: Vec<&&RawSlide<'_>> = slides
                .iter()
                .filter(|s| {
                    separator_lines
                        .iter()
                        .any(|(start, _)| *start == s.start_line)
                })
                .collect();
            assert_eq!(opened.len(), separator_lines.len(), "{name}");
            for slide in &slides {
                assert!(
                    slide.start_line >= 1 && slide.end_line >= slide.start_line,
                    "{name}"
                );
                let Some(first) = slide.nodes.first() else {
                    let sep = separator_lines.iter().find(|(s, _)| *s == slide.start_line);
                    assert_eq!(sep.map(|(_, end)| *end), Some(slide.end_line), "{name}");
                    continue;
                };
                let last_end = slide.nodes.iter().map(|n| line_range(n).1).max();
                assert_eq!(last_end, Some(slide.end_line), "{name}");
                let opened_by_separator =
                    separator_lines.iter().any(|(s, _)| *s == slide.start_line);
                assert!(
                    opened_by_separator || line_range(first).0 == slide.start_line,
                    "{name}: slide at {} does not start at a separator or its first node",
                    slide.start_line
                );
                assert_eq!(
                    slide.span(),
                    SourceSpan {
                        start_line: slide.start_line,
                        end_line: slide.end_line,
                        cell: None
                    }
                );
            }
        }
    }

    fn assert_attrs(
        slide: &RawSlide<'_>,
        id: Option<&str>,
        layout: Option<&str>,
        classes: &[&str],
        attrs: &[(&str, &str)],
    ) {
        let expected: BTreeMap<String, String> = attrs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let line = slide.start_line;
        assert_eq!(slide.attrs.id.as_deref(), id, "slide at line {line}");
        assert_eq!(
            slide.attrs.layout.as_deref(),
            layout,
            "slide at line {line}"
        );
        assert_eq!(slide.attrs.classes, classes, "slide at line {line}");
        assert_eq!(slide.attrs.attrs, expected, "slide at line {line}");
    }

    #[test]
    fn attributes_fixture_normalizes_and_lifts() {
        let text = fixture("attributes.md");
        let arena = Arena::new();
        let deck = split_text(&arena, &text);
        let s = flatten(&deck);
        assert_eq!(s.len(), 9);
        // 1: nothing to lift
        assert!(s[0].attrs.is_empty());
        // 2: `--- {.dark background=#111 layout=title}`
        assert_attrs(
            s[1],
            None,
            Some("title"),
            &["dark"],
            &[("data-background-color", "#111")],
        );
        // 3: `--- {background=bg.png}`
        assert_attrs(
            s[2],
            None,
            None,
            &[],
            &[("data-background-image", "bg.png")],
        );
        // 3.1: `-- {auto-animate}`
        assert_eq!(deck.slides[2].1.len(), 1);
        assert_attrs(s[3], None, None, &[], &[("data-auto-animate", "")]);
        // 4: `# T {#intro .big k=v}` lifted from the heading
        assert_attrs(s[4], Some("intro"), None, &["big"], &[("k", "v")]);
        assert_eq!(s[4].attrs.unknown_keys, ["k"]);
        // 5: separator attrs merged with heading attrs; the heading wins
        assert_attrs(
            s[5],
            Some("merged"),
            None,
            &["a", "shared", "b"],
            &[
                ("data-auto-animate", ""),
                ("data-state-note", "from separator"),
                ("data-transition", "zoom"),
            ],
        );
        // 6: rgba colour, data- prefix, verbatim aria-*, single quotes
        assert_attrs(
            s[6],
            None,
            None,
            &[],
            &[
                ("aria-label", "Single quoted"),
                ("data-background-color", "rgba(0, 0, 0, 0.5)"),
                ("data-background-opacity", "0.4"),
            ],
        );
        // 7: colour keyword and more shorthands
        assert_attrs(
            s[7],
            None,
            None,
            &[],
            &[
                ("data-background-color", "navy"),
                ("data-background-size", "contain"),
                ("data-state", "calm"),
                ("data-transition-speed", "fast"),
                ("data-visibility", "uncounted"),
            ],
        );
        // 8: layout lifted from the first heading
        assert_attrs(s[8], None, Some("section"), &["x"], &[]);
        for slide in &s {
            if slide.start_line != 49 {
                assert!(slide.attrs.unknown_keys.is_empty(), "{}", slide.start_line);
            }
        }

        // Lifted headings are clean afterwards; a heading that is not first keeps its attrs.
        for slide in [s[4], s[5], s[8]] {
            let heading = slide.nodes[0].data();
            assert!(matches!(heading.value, NodeValue::Heading(_)));
            assert!(
                heading.attrs.is_none(),
                "heading at line {}",
                heading.sourcepos.start.line
            );
        }
        let kept = s[8].nodes[2].data();
        assert!(matches!(
            kept.value,
            NodeValue::Heading(NodeHeading { level: 3, .. })
        ));
        let kept_attrs = kept.attrs.as_deref().map(ParsedAttrs::from);
        assert_eq!(
            kept_attrs,
            Some(ParsedAttrs {
                id: Some("kept".into()),
                classes: vec!["muted".into()],
                pairs: vec![],
            })
        );

        let unknown = deck.diagnostics.iter().next().unwrap();
        assert_eq!(unknown.span, Some(s[4].span()));
        assert_eq!(
            format!("{unknown:#}"),
            "line 49: warning: unknown slide attribute `k` on slide 4 (kept verbatim) \
             [unknown-slide-attr]"
        );
    }

    /// The splitter's output, applied to IR slides, satisfies the IR invariants.
    #[test]
    fn attributes_fixture_applies_to_valid_ir_slides() {
        let text = fixture("attributes.md");
        let arena = Arena::new();
        let deck = split_text(&arena, &text);
        let to_slide = |raw: &RawSlide<'_>| {
            let mut slide = Slide::new();
            raw.attrs.clone().apply_to(&mut slide);
            slide.span = Some(raw.span());
            slide
        };
        let slides: Vec<Slide> = deck
            .slides
            .iter()
            .map(|(raw, children)| Slide {
                children: children.iter().map(to_slide).collect(),
                ..to_slide(raw)
            })
            .collect();
        assert_eq!(slides[1].layout, "title");
        assert_eq!(slides[3].id.as_deref(), Some("intro"));
        let ir = Deck {
            meta: DeckMeta::default(),
            slides,
            features: Features::default(),
            source: SourceInfo {
                path: "/abs/attributes.md".into(),
                kind: SourceKind::Markdown,
                base_dir: "/abs".into(),
            },
            assets: vec![],
        };
        assert_eq!(ir.invariant_violations(), Vec::<String>::new());
    }

    #[test]
    fn setext_underline_is_not_a_separator() {
        // CommonMark reads `Text` directly followed by a `--` line as a level-2 setext
        // heading, so the AST has no `--` paragraph and the splitter cannot see a separator
        // (dialect §2.2 "Setext pitfall"; `check` warns about the raw line, T34).
        let arena = Arena::new();
        let deck = split_text(&arena, "# A\n\nText\n--\n\nmore\n");
        assert_eq!(shape(&deck), "1-6");
        assert!(deck.diagnostics.is_empty());
        let setext = deck.slides[0].0.nodes[1].data();
        assert!(matches!(
            setext.value,
            NodeValue::Heading(NodeHeading {
                level: 2,
                setext: true,
                ..
            })
        ));
        drop(setext);
        // With a blank line before it, the same `--` line is a vertical separator.
        let deck = split_text(&arena, "# A\n\nText\n\n--\n\nmore\n");
        assert_eq!(shape(&deck), "1-3 [5-7]");
    }

    #[test]
    fn edge_cases_split_as_specified() {
        let cases: &[(&str, &str, &str, Diags)] = &[
            (
                "empty document: one empty slide",
                "",
                "1-1",
                &[("empty-slide", 1)],
            ),
            (
                "front matter only",
                "---\ntitle: x\n---\n",
                "3-3",
                &[("empty-slide", 3)],
            ),
            (
                "a leading separator opens slide 1 (no empty slide before it)",
                "---\ntitle: x\n---\n\n---\n\n# A\n",
                "5-7",
                &[],
            ),
            (
                "content before the first separator is slide 1",
                "<!-- c -->\n\n---\n\n# A\n",
                "1-1 | 3-5",
                &[],
            ),
            (
                "orphan vertical separator is treated as horizontal",
                "--\n\n# A\n\n--\n\n# B\n",
                "1-3 [5-7]",
                &[("orphan-vertical", 1)],
            ),
            (
                "trailing separator yields an empty slide",
                "# A\n\n---\n",
                "1-1 | 3-3",
                &[("empty-slide", 3)],
            ),
            (
                "`--` right after `---`: empty first vertical slide",
                "# A\n\n---\n\n--\n\n# B\n",
                "1-1 | 3-3 [5-7]",
                &[("empty-slide", 3)],
            ),
            (
                "an empty slide with attributes is intentional (no warning)",
                "# A\n\n--- {background=x.png}\n\n---\n\n# B\n",
                "1-1 | 3-3 | 5-7",
                &[],
            ),
            (
                "`---` closes the vertical stack",
                "# A\n\n--\n\n# A.1\n\n---\n\n# B\n",
                "1-1 [3-5] | 7-9",
                &[],
            ),
            (
                "code spans and emphasis are not separators",
                "# A\n\n`-- {auto-animate}`\n\n*--- {.x}*\n\n_--_\n",
                "1-7",
                &[],
            ),
            (
                "`--` must be the whole paragraph",
                "# A\n\n--\nmore\n\nText\n-- {x}\n",
                "1-7",
                &[],
            ),
            (
                "attribute separator directly before a heading",
                "# A\n\n-- {auto-animate}\n# B\n",
                "1-1 [3-4]",
                &[],
            ),
            ("empty braces", "# A\n\n--- {}\n\n# B\n", "1-1 | 3-5", &[]),
            (
                "`.` in the pattern does not match a line break",
                "# A\n\n--- {a\nb}\n",
                "1-4",
                &[],
            ),
            (
                "`\\s*` does match a line break",
                "# A\n\n--\n{.x}\n\n# B\n",
                "1-1 [3-6]",
                &[],
            ),
            (
                "separators nested in containers are content",
                "# A\n\n> ---\n\n- item\n\n  --\n\n:::note\n---\n:::\n",
                "1-11",
                &[],
            ),
        ];
        for (what, text, expected_shape, expected_diags) in cases {
            let arena = Arena::new();
            let deck = split_text(&arena, text);
            assert_eq!(shape(&deck), *expected_shape, "{what}");
            assert_eq!(diags(&deck), *expected_diags, "{what}");
        }
    }

    #[test]
    fn separator_attributes_reach_the_slide_they_open() {
        let arena = Arena::new();
        let text = "-- {.first}\n\n# A\n\n--- {background=https://example.org/bg.png}\n\n# B\n\n\
                    --\n{.x}\n\n# B.1\n";
        let deck = split_text(&arena, text);
        assert_eq!(shape(&deck), "1-3 | 5-7 [9-12]");
        // the orphan `-- {…}` keeps its attributes on slide 1
        assert_eq!(deck.slides[0].0.attrs.classes, ["first"]);
        // an autolinked URL is still part of the separator text
        assert_eq!(
            deck.slides[1].0.attrs.attrs["data-background-image"],
            "https://example.org/bg.png"
        );
        assert_eq!(deck.slides[1].1[0].attrs.classes, ["x"]);
    }

    #[test]
    fn unbalanced_quote_in_separator_is_a_parse_error_with_line() {
        let arena = Arena::new();
        let root = parse(&arena, "# A\n\n--- {k=\"v}\n");
        let err = split(root, SplitMode::Rules).unwrap_err();
        let Error::Parse {
            path,
            line,
            message,
        } = &err
        else {
            panic!("expected Error::Parse, got {err:?}");
        };
        assert_eq!(path, &PathBuf::new());
        assert_eq!(*line, Some(3));
        assert!(message.contains("unbalanced"), "{message}");
    }

    #[test]
    fn headings_mode_is_not_implemented_yet() {
        let arena = Arena::new();
        let root = parse(&arena, "# A\n");
        let err = split(root, SplitMode::Headings { slide_level: 2 }).unwrap_err();
        assert!(
            matches!(&err, Error::Other(m) if m == "headings mode: T43"),
            "{err:?}"
        );
    }

    #[test]
    fn footnote_definitions_stay_on_their_slide() {
        let arena = Arena::new();
        let text =
            "# A\n\nText[^1].\n\n[^1]: The note.\n\n---\n\n# B\n\nMore[^2].\n\n[^2]: Second.\n";
        let deck = split_text(&arena, text);
        assert_eq!(shape(&deck), "1-5 | 7-13");
        for (slide, _) in &deck.slides {
            let last = slide.nodes.last().unwrap().data();
            assert!(matches!(last.value, NodeValue::FootnoteDefinition(_)));
        }
    }

    #[test]
    fn lift_heading_attrs_only_takes_a_first_heading() {
        let arena = Arena::new();
        let root = parse(&arena, "# T {#a .b k=v}\n\npara\n\n## U {#c}\n");
        let nodes: Vec<_> = root.children().collect();
        assert_eq!(lift_heading_attrs(&nodes[1..]), None, "paragraph first");
        assert_eq!(
            lift_heading_attrs(&nodes),
            Some(ParsedAttrs {
                id: Some("a".into()),
                classes: vec!["b".into()],
                pairs: vec![("k".into(), "v".into())],
            })
        );
        assert!(nodes[0].data().attrs.is_none());
        assert_eq!(lift_heading_attrs(&nodes), None, "already lifted");
        assert!(
            nodes[2].data().attrs.is_some(),
            "a later heading is untouched"
        );
        assert_eq!(lift_heading_attrs(&[]), None);
        // a heading without attributes (comrak leaves `{auto-animate}` in its text)
        let root = parse(&arena, "## X {auto-animate}\n");
        let nodes: Vec<_> = root.children().collect();
        assert_eq!(lift_heading_attrs(&nodes), None);
    }

    #[test]
    fn first_heading_text_is_plain_text() {
        let cases: &[(&str, Option<&str>)] = &[
            ("# Hello *world*\n", Some("Hello world")),
            (
                "## Use `cargo build` **now**\n",
                Some("Use cargo build now"),
            ),
            ("# Hello *world* `code` {#h .c}\n", Some("Hello world code")),
            ("# A [link](https://x.org) here\n", Some("A link here")),
            ("Some\ntext\n---\n", Some("Some text")),
            ("para\n\n## Second {#s}\n\n# Third\n", Some("Second")),
            ("para only\n", None),
            ("#\n\n# Later\n", None),
            ("> # Quoted\n", None),
        ];
        for (text, expected) in cases {
            let arena = Arena::new();
            let nodes: Vec<_> = parse(&arena, text).children().collect();
            assert_eq!(first_heading_text(&nodes).as_deref(), *expected, "{text:?}");
        }
    }

    #[test]
    fn attribute_separator_pattern() {
        use Direction::{Horizontal, Vertical};
        let cases: &[(&str, Option<(Direction, &str)>)] = &[
            ("--- {.a}", Some((Horizontal, ".a"))),
            ("---{.a}", Some((Horizontal, ".a"))),
            ("-- {auto-animate}", Some((Vertical, "auto-animate"))),
            ("--\t{x}", Some((Vertical, "x"))),
            ("--- {a} {b}", Some((Horizontal, "a} {b"))),
            ("--- {}", Some((Horizontal, ""))),
            ("---- {x}", None),
            ("- {x}", None),
            ("--- x", None),
            ("--- {x} trailing", None),
            (" -- {x}", None),
            ("--", None),
        ];
        for (text, expected) in cases {
            assert_eq!(match_attr_separator(text), *expected, "{text:?}");
        }
    }
}
