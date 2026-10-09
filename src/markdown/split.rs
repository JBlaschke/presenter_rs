//! Slide splitter: `---` / `--` separators, slide attributes, heading lifting. Implemented by T11 (headings mode: T43).
//!
//! Contract: `docs/spec/markdown-dialect.md` §2.2 (separators, rules mode), §2.3 (headings
//! mode) and §3.2 (where slide attributes come from). Splitting is decided on the comrak AST,
//! looking only at the `Document`'s direct children, so `---`/`--` inside code fences, lists,
//! block quotes or directives are never separators, and headings there never start slides.
//! Nothing is rendered here (T12 renders each [`RawSlide`]).
//!
//! The entry points differ only in how a top-level `Paragraph` is read:
//! - [`split_with_source`] (preferred; T12 calls it) also gets the Markdown text and reads the
//!   paragraph's **raw source lines** (the lines of its text, see below, each trimmed of spaces
//!   and tabs, joined with `\n`). Exactly `--` is a vertical separator;
//!   `^(---|--)\s*\{(.*)\}$` is a separator with attributes, and the brace contents go to
//!   [`parse_attr_block`] verbatim: the same text comrak's heading-attribute parser reads, so
//!   backslash escapes, entities, emphasis delimiters (`bg_draft_.png`) and smart punctuation
//!   (`parse.smart`) cannot change or hide a separator. It is the only exact reading when
//!   smart punctuation is on. A `--` paragraph spelled any other way (`\--`) is the escape for
//!   a literal `--`.
//! - [`split_ast`] has only the AST and the `parse.smart` flag comrak ran with, and reads the
//!   paragraph's inlines: `Text`, line breaks and autolinks. Any other inline (emphasis, code,
//!   math, raw HTML, an ordinary link) or a leading escape means "not a separator". A bare `--`
//!   must be one `Text` covering exactly the two source columns where the paragraph starts (so
//!   `\--`, `-\-` and `&#45;&#45;` stay content). Escapes and entities inside the braces are
//!   already resolved (`title="a \"b\""` reads as `title="a "b""`). Smart punctuation is
//!   undone only with `smart = true`: a candidate must then start with comrak's `–`/`—` (ASCII
//!   dashes there were escaped, so the line is content), and every `—`, `–`, curly quote and
//!   `…` in it is mapped back to `---`, `--`, `"`/`'` and `...` (so `bg--v2.png` survives).
//!   That cannot be exact: such a character *typed* in a value comes back as ASCII too. With
//!   `smart = false` the inlines are the typed text, so a typed `— {…}` line stays content.
//! - [`split`] (the task-file signature) is [`split_ast`] with `smart = false`, the
//!   `markdown.smart_punctuation` default.
//!
//! The text of a paragraph is its sourcepos line range minus leading link reference
//! definitions: comrak removes them from the paragraph's content but not from its sourcepos,
//! so `[x]: http://a` directly followed by a `--- {.a}` line is still a separator (opened on
//! the `--- {.a}` line), as dialect §2.2 reads the paragraph's text. [`split_with_source`]
//! finds those definition lines exactly (comrak parses the paragraph's first lines again).
//! [`split_ast`] infers them from the inlines' line numbers, and only for a paragraph whose
//! inlines are all text, breaks, code spans, raw HTML, math, emphasis or autolinks (always
//! true for a separator): another link or image can hide line endings from those numbers, so
//! for such a paragraph after definitions a lookalike line (below) is reported up to that
//! many lines early.
//!
//! Rules implemented by all of them:
//! - `FrontMatter` is skipped (neither a slide nor a separator).
//! - `ThematicBreak` (`---`, `***`, `___`) opens a horizontal slide; a `--` paragraph opens a
//!   vertical slide under the current horizontal one; the `{…}` forms do the same and carry
//!   attributes for the slide they open.
//! - Content before the first separator is slide 1. When there is no such content the first
//!   separator opens slide 1 itself (a leading `--` has no horizontal slide to attach to: it
//!   is treated as horizontal and reported as `orphan-vertical`). A document without
//!   separators or content yields one empty slide.
//! - Every slide without nodes is reported as `empty-slide` (two consecutive separators, a
//!   `--` right after `---`, a trailing separator), also when it carries attributes.
//! - A horizontal separator closes an open vertical stack.
//! - HTML blocks made only of comments render nothing, and a top-level `:::notes` block goes
//!   to the speaker notes ([`compat::is_invisible`]), so they do not count when looking for a
//!   slide's *first node*: its first visible node is (a licence, lint or TODO comment, or
//!   the speaker notes, before the title change nothing; A3).
//! - A `<!-- .slide: … -->` comment (a top-level HTML block that is exactly that comment, see
//!   [`compat::slide_comment`]) among the invisible nodes that start the slide, or among those
//!   right after the slide's first visible node when that is a heading, gives the slide attributes
//!   and is removed from its nodes; several are all read, in order. One anywhere else, also
//!   nested in a list or directive or inline in a paragraph or heading, stays a plain comment
//!   and is reported as `misplaced-slide-comment`. An unbalanced quote in one that is read is
//!   an `Error::Parse` on its line, as for separators.
//! - Attributes on a slide's first visible node, when it is a heading, are lifted onto the
//!   slide ([`lift_heading_attrs`]). Precedence (§3.2, later wins): separator, then `.slide:`
//!   comments, then the heading.
//! - A line of an ordinary top-level paragraph that looks like an attribute separator
//!   (`^---?\s*\{`) but is not one (no blank line around it, trailing text, or for
//!   [`split_ast`] inline syntax) is reported as `separator-not-recognized`, so a slide
//!   boundary is never lost silently. (T34 reports raw `---`/`--` lines; this covers the
//!   `{…}` forms.)
//! - Footnote definitions, which comrak moves to the end of the document, belong to the slide
//!   whose source range they are written in (they count for its span, and footnotes before
//!   the first separator make slide 1). They stay last among its nodes, in comrak's order, as
//!   comrak's HTML formatter expects.
//!
//! Headings mode ([`SplitMode::Headings`], `slides.split = "headings"`, dialect §2.3; both
//! entry points) adds slide boundaries at top-level headings with `level <= slide_level`;
//! everything above stays as it is (separators are still honored, mixed mode):
//! - A heading with `level < slide_level` (a *section* heading, `#` by default) starts a new
//!   horizontal slide.
//! - A heading with `level == slide_level` (`##` by default) starts a new vertical slide under
//!   the current horizontal slide when that slide's stack was started by a section heading
//!   (`##` "under" a `#`). Otherwise there is nothing to go under, and it starts a horizontal
//!   slide, as pandoc does for slide-level headings outside any section (and as an orphan
//!   `--` is treated in rules mode). So a deck of `##` slides only is a row of horizontal
//!   slides, and with `slide_level = 1` every `#` is one.
//! - Deeper headings are ordinary content. The boundary heading is the slide's first visible
//!   node, so its `{…}` attributes are lifted and the `.slide:` comments under it are read as
//!   usual.
//! - The open slide does not end at a boundary heading while it has no visible content yet:
//!   nothing at all, or only HTML comments and `:::notes` blocks ([`compat::is_invisible`]).
//!   Then the heading joins it
//!   instead. That gives dialect §2.3's "content before the first heading becomes an initial
//!   slide only if it is non-blank", and makes `---` (or `--`) directly followed by a heading,
//!   possibly with comments between them, one slide, as in rules mode: the separator decides
//!   the direction and its attributes apply. A rules-mode deck whose slides start with
//!   headings therefore keeps its shape in headings mode.
//! - Likewise only HTML comments and `:::notes` blocks before the first separator are not a
//!   slide of their own:
//!   they go to the slide that separator opens (so a leading `--` after a licence comment is
//!   still reported as `orphan-vertical`). In rules mode they stay slide 1 (§2.2).
//! - `---` starts a horizontal stack that no section heading started, so the `##` headings
//!   after it start horizontal slides until the next section heading. `--` keeps the stack.

use std::borrow::Cow;
use std::path::PathBuf;

use comrak::nodes::{AstNode, NodeValue};

use crate::error::{Diagnostics, Error, Result};
use crate::ir::SourceSpan;
use crate::markdown::attrs::{ParsedAttrs, SlideAttrs, parse_attr_block};
use crate::markdown::{compat, render};

/// How slide boundaries are found (`slides.split` in the config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitMode {
    /// `---` / `--` separator lines (dialect §2.2); the default.
    Rules,
    /// Heading-driven boundaries (dialect §2.3), with separator lines still honored; see the
    /// module docs.
    Headings {
        /// Heading level that starts a vertical slide (`slides.slide_level`); lower levels
        /// start horizontal slides, higher ones are content. `0` makes every heading content.
        slide_level: u8,
    },
}

/// One slide's top-level nodes, before rendering.
#[derive(Debug)]
pub struct RawSlide<'a> {
    /// Separator attributes merged with the `<!-- .slide: -->` comment attributes and the
    /// lifted first-heading attributes (later wins, in that order).
    pub attrs: SlideAttrs,
    /// The slide's top-level AST nodes in document order, separators and the `.slide:`
    /// comments that were read excluded; footnote definitions come last (in comrak's order).
    pub nodes: Vec<&'a AstNode<'a>>,
    /// 1-based first line: the line of the separator that opened the slide (in headings mode,
    /// of the comments before it when the first separator took them, see the module docs);
    /// for a slide not opened by a separator (content before the first separator), its
    /// earliest node's first line (the front matter's last line, or 1, when it has no nodes).
    pub start_line: usize,
    /// 1-based last line: the last line of its latest node, or the opening separator's last
    /// line when the slide has no nodes.
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
    /// `empty-slide`, `orphan-vertical`, `separator-not-recognized`, `unknown-slide-attr`,
    /// `invalid-slide-attr` and `misplaced-slide-comment` warnings, ordered by line (span
    /// set, `cell: None`, no path).
    pub diagnostics: Diagnostics,
}

/// Splits a parsed document (`root` is comrak's `Document` node) into slides, reading
/// separator paragraphs from their inlines, for a document parsed **without** smart
/// punctuation (`parse.smart = false`, the `markdown.smart_punctuation` default): exactly
/// [`split_ast`]`(root, mode, false)`. Prefer [`split_with_source`], which reads separators
/// from the source text and is exact for escapes, entities, inline syntax inside `{…}` and
/// smart punctuation (the module docs list the differences).
///
/// Heading attributes that are lifted onto a slide are removed from the heading node
/// (`attrs = None`), so the rendered `<h1>` is clean.
///
/// Errors: `Error::Parse { path: <empty>, line: Some(line), .. }` when a separator's attribute
/// block or a `<!-- .slide: -->` comment that is read has an unbalanced quote (the line is the
/// separator's or the comment's first line). `split` does not know the file, so the caller
/// fills in `path`.
pub fn split<'a>(root: &'a AstNode<'a>, mode: SplitMode) -> Result<RawDeck<'a>> {
    split_ast(root, mode, false)
}

/// Like [`split`], for a document comrak parsed with `parse.smart = smart`
/// (`markdown.smart_punctuation`). With `smart = true` separator candidates are read with
/// comrak's smart punctuation mapped back (`—` → `---`, `–` → `--`, curly quotes → straight,
/// `…` → `...`); with `false` the inlines are taken as typed. Passing the wrong flag misreads
/// separators: `smart = true` is needed to see `-- {…}`/`--- {…}` in a smart document, and in
/// a document without it, it would read a typed `— {…}` line as a separator. Additive to the
/// T11 task-file API; errors as [`split`].
pub fn split_ast<'a>(root: &'a AstNode<'a>, mode: SplitMode, smart: bool) -> Result<RawDeck<'a>> {
    split_impl(root, None, mode, smart)
}

/// Like [`split`], but classifies separator paragraphs by their raw source lines in `source`,
/// the exact text `root` was parsed from (dialect §2.2 "text matches", §3.1 "the two must
/// agree"), with or without smart punctuation. Additive to the T11 task-file API; T12 calls
/// this one.
///
/// If a node's sourcepos lies outside `source` (wrong text passed), that node is read from
/// its inlines as [`split`] does (as if parsed without smart punctuation). Errors as [`split`].
pub fn split_with_source<'a>(
    root: &'a AstNode<'a>,
    source: &str,
    mode: SplitMode,
) -> Result<RawDeck<'a>> {
    let lines = source_lines(source);
    split_impl(root, Some(&lines), mode, false)
}

/// The shared walk: `lines` is the source ([`split_with_source`]) or `None` ([`split_ast`]);
/// `smart` tells how to read a paragraph from its inlines (always without `lines`, else only
/// for a node outside them).
fn split_impl<'a>(
    root: &'a AstNode<'a>,
    lines: Option<&[&str]>,
    mode: SplitMode,
    smart: bool,
) -> Result<RawDeck<'a>> {
    let mut builder = Builder::new(root, lines, smart, mode);
    for node in root.children() {
        builder.visit(node)?;
    }
    builder.finish()
}

/// The heading whose `{…}` attributes the splitter lifts onto the slide that the start of
/// `root`'s document belongs to (the slide before any separator): the first top-level node
/// that is not front matter, a footnote definition (the splitter moves those to the end of
/// the slide) or invisible ([`compat::is_invisible`]: HTML comments, `:::notes`), when it is
/// a heading. A notebook markdown cell split by separators (`split_markdown_cells`) takes the
/// attributes off this heading before splitting when they must stay on it because the
/// current slide is not empty (notebook-mapping §2, A3).
pub fn lifted_heading<'a>(root: &'a AstNode<'a>) -> Option<&'a AstNode<'a>> {
    root.children()
        .find(|node| {
            !matches!(
                node.data().value,
                NodeValue::FrontMatter(_) | NodeValue::FootnoteDefinition(_)
            ) && !compat::is_invisible(node)
        })
        .filter(|node| matches!(node.data().value, NodeValue::Heading(_)))
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
/// The headings searched are the `nodes` themselves and, depth first, those inside a
/// directive (`:::center`, `::::columns`, …; not `:::notes`), so a title wrapped in a layout
/// directive still names the slide. Headings in block quotes, lists, alerts or footnotes do
/// not count. The text is every `Text` (also inside emphasis, links, …) plus the literals of
/// code spans and inline math (the TeX without its `$`, so `# The $E=mc^2$ law` is
/// `The E=mc^2 law`); soft and hard line breaks (multi-line setext headings) become spaces;
/// images (including their alt text), raw HTML and footnote references are skipped. Runs of
/// whitespace collapse to one space and the result is trimmed; `None` when there is no
/// heading or the first heading has no text.
pub fn first_heading_text<'a>(nodes: &[&'a AstNode<'a>]) -> Option<String> {
    let heading = nodes.iter().find_map(|node| find_heading(node))?;
    let mut text = String::new();
    push_plain_text(heading, &mut text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// `node` when it is a heading, else the first heading inside it if it is a directive other
/// than `notes`.
fn find_heading<'a>(node: &'a AstNode<'a>) -> Option<&'a AstNode<'a>> {
    let descend = match &node.data().value {
        NodeValue::Heading(_) => return Some(node),
        NodeValue::BlockDirective(directive) => !render::is_notes_info(&directive.info),
        _ => false,
    };
    if descend {
        node.children().find_map(find_heading)
    } else {
        None
    }
}

/// Appends the plain text of `node`'s inline descendants (see [`first_heading_text`]).
fn push_plain_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        match &child.data().value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            NodeValue::Math(math) => out.push_str(&math.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
            NodeValue::Image(_) => {}
            // raw HTML and footnote references have no children: nothing is added
            _ => push_plain_text(child, out),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Horizontal,
    Vertical,
}

/// A recognized separator: its direction and, for the `{…}` forms, the brace contents.
#[derive(Debug, PartialEq, Eq)]
struct Separator {
    direction: Direction,
    attrs: Option<String>,
}

/// Classifies a top-level node (dialect §2.2). With `lines` (the source, see
/// [`source_lines`]) a paragraph is read from its raw source, otherwise from its inlines as
/// parsed with `parse.smart = smart`.
fn separator<'a>(node: &'a AstNode<'a>, lines: Option<&[&str]>, smart: bool) -> Option<Separator> {
    match node.data().value {
        NodeValue::ThematicBreak => {
            return Some(Separator {
                direction: Direction::Horizontal,
                attrs: None,
            });
        }
        NodeValue::Paragraph => {}
        _ => return None,
    }
    match lines.and_then(|lines| raw_lines(lines, node)) {
        Some(raw) => separator_in_text(&join_lines(&raw)),
        None => separator_in_inlines(node, smart),
    }
}

/// A separator paragraph's text: exactly `--`, or `^(---|--)\s*\{(.*)\}$`.
fn separator_in_text(text: &str) -> Option<Separator> {
    if text == "--" {
        return Some(Separator {
            direction: Direction::Vertical,
            attrs: None,
        });
    }
    let (direction, inner) = match_attr_separator(text)?;
    Some(Separator {
        direction,
        attrs: Some(inner.to_string()),
    })
}

/// A separator read from a paragraph's inlines (no source text; see the module docs).
fn separator_in_inlines<'a>(paragraph: &'a AstNode<'a>, smart: bool) -> Option<Separator> {
    if is_bare_vertical(paragraph, smart) {
        return Some(Separator {
            direction: Direction::Vertical,
            attrs: None,
        });
    }
    let text = inline_separator_text(paragraph)?;
    let text = typed_text(&text, smart)?;
    let (direction, inner) = match_attr_separator(&text)?;
    Some(Separator {
        direction,
        attrs: Some(inner.to_string()),
    })
}

/// The 1-based numbers and trimmed text of the source lines of a top-level block's text (see
/// [`text_lines`]); `None` when they lie outside `lines`.
fn raw_lines<'a, 's>(lines: &[&'s str], node: &'a AstNode<'a>) -> Option<Vec<(usize, &'s str)>> {
    let (first, last) = text_lines(node, Some(lines));
    let block = lines.get(first.checked_sub(1)?..last)?;
    let trimmed = block.iter().map(|line| line.trim_matches([' ', '\t']));
    Some((first..).zip(trimmed).collect())
}

/// The 1-based first and last line of a top-level block's own text: its sourcepos lines,
/// except that a paragraph starting with link reference definitions (`[x]: http://a` directly
/// followed by text) does not include them. comrak removes the definitions from the
/// paragraph's content but keeps its sourcepos start on the first one.
///
/// [`unnumbered_lines`] bounds how many lines that can be (0 for almost every paragraph).
/// With the source (`lines`, when the paragraph lies inside them) comrak itself tells which
/// of those lines are definitions ([`definition_lines`]). Without it, all of them are taken
/// to be definitions when the inlines number every line ([`inlines_number_every_line`]), and
/// none otherwise.
fn text_lines<'a>(node: &'a AstNode<'a>, lines: Option<&[&str]>) -> (usize, usize) {
    let (start, end) = {
        let pos = node.data().sourcepos;
        (pos.start.line, pos.end.line)
    };
    let unnumbered = unnumbered_lines(node);
    if unnumbered == 0 {
        return (start, end);
    }
    let skipped = match lines.and_then(|lines| lines.get(start.checked_sub(1)?..end)) {
        Some(block) => definition_lines(block, unnumbered),
        None if inlines_number_every_line(node) => unnumbered,
        None => 0,
    };
    (start + skipped, end)
}

/// How many of a paragraph's sourcepos lines its inlines' line numbers do not account for
/// (0 for other blocks): the range's length minus the inlines' line count, which is the last
/// inline's end line minus the paragraph's start line plus one, and at least one more than
/// the number of line breaks. comrak numbers the inlines from the paragraph's start line
/// after removing leading link reference definitions, and only ever advances that number on
/// a line ending of the remaining text. So this is at least the number of definition lines:
/// exactly that when every line ending is numbered, more when a link or image hides some in
/// its destination, title or reference label (`[l](u "a` + newline + `b")`), as an inline
/// attribute block also does. Always less than the range's length.
fn unnumbered_lines<'a>(node: &'a AstNode<'a>) -> usize {
    let (start, end) = {
        let data = node.data();
        if !matches!(data.value, NodeValue::Paragraph) {
            return 0;
        }
        (data.sourcepos.start.line, data.sourcepos.end.line)
    };
    let Some(last_inline) = node.last_child() else {
        return 0;
    };
    let last_line = last_inline.data().sourcepos.end.line;
    let breaks = node
        .descendants()
        .filter(|n| matches!(n.data().value, NodeValue::SoftBreak | NodeValue::LineBreak))
        .count();
    let numbered = (last_line + 1).saturating_sub(start).max(breaks + 1);
    (end + 1).saturating_sub(start).saturating_sub(numbered)
}

/// How many of a paragraph's first source lines are link reference definitions, given its
/// source lines (`block`, untrimmed) and an upper bound (`limit`, see [`unnumbered_lines`]):
/// the largest `k <= limit` for which comrak parses the first `k` lines on their own as
/// nothing but definitions (an empty document). comrak removes definitions from the start
/// of a paragraph greedily, and each one reads the same text in the full paragraph as in
/// such a prefix (a title it looks for on the next line can only make it longer), so that
/// `k` is the number of lines it removed; 0 when the paragraph cannot start with one (its
/// first line does not start with `[`). The definitions themselves do not depend on the
/// extensions, so the prefixes are parsed with comrak's default options.
///
/// Only the prefixes [`definition_candidates`] lists are parsed (largest first), and at most
/// [`MAX_DEFINITION_PARSES`] of them, so the work stays linear in the paragraph: a long
/// multi-line link title is parsed once or not at all, not once per line. Every candidate
/// above the answer fails, and there are at most as many of those as line endings hidden in
/// links of the paragraph's own text (see [`unnumbered_lines`]): none for a separator
/// paragraph, a handful for real text. Only a paragraph built to hide more of them than the
/// cap, with a `]:` or a title's closing character on each such line, runs out; it is then
/// read as if it had no definitions (0, as [`text_lines`] does without the source). It is
/// not a separator either way; [`Builder::report_lookalikes`] then also looks at its
/// definition lines, and a `Note:` after them is not seen ([`first_text_line`]).
fn definition_lines(block: &[&str], limit: usize) -> usize {
    let options = comrak::Options::default();
    definition_candidates(block, limit)
        .take(MAX_DEFINITION_PARSES)
        .find(|&k| {
            let arena = comrak::Arena::new();
            let root = comrak::parse_document(&arena, &block[..k].join("\n"), &options);
            root.first_child().is_none()
        })
        .unwrap_or(0)
}

/// How many prefixes of one paragraph [`definition_lines`] parses at most.
const MAX_DEFINITION_PARSES: usize = 16;

/// The line counts `k` (largest first, `k <= limit`, `k < block.len()`) for which the first `k`
/// lines can be nothing but link reference definitions, by a necessary condition that needs no
/// parsing: the first line starts with `[`, some line up to `k` holds the `]:` that ends a
/// label, and line `k` (the last one) can end a definition. comrak reads a definition as a
/// label (ending in `]:` on one line), a destination on that line or the next one, an optional
/// title (closed by `"`, `'` or `)`) and then only spaces up to the line end. So the last line
/// holds a `]:`, follows a line that does, or ends (trailing spaces and tabs aside) with a
/// title's closing character. Every `k` for which [`definition_lines`]' parse can succeed is
/// listed; most others are not (a crafted paragraph can make every line a candidate, hence
/// the cap there).
fn definition_candidates<'b>(block: &'b [&str], limit: usize) -> impl Iterator<Item = usize> + 'b {
    let label_first = block
        .first()
        .is_some_and(|line| line.trim_start_matches([' ', '\t']).starts_with('['));
    let most = if label_first {
        limit.min(block.len().saturating_sub(1))
    } else {
        0
    };
    let first_label_end = block[..most].iter().position(|line| line.contains("]:"));
    let lowest = first_label_end.map_or(usize::MAX, |at| at + 1);
    (lowest..=most).rev().filter(move |&k| {
        let last = block[k - 1];
        last.contains("]:")
            || (k >= 2 && block[k - 2].contains("]:"))
            || last
                .trim_end_matches([' ', '\t'])
                .ends_with(['"', '\'', ')'])
    })
}

/// Whether comrak numbered every line ending of a paragraph's inlines, so that
/// [`unnumbered_lines`] is exactly its number of link reference definition lines: true when
/// all of them are text, line breaks, code spans, raw HTML, math, emphasis, strikethrough or
/// autolinks, without inline attributes. Any other link or image may hide line endings in its
/// destination, title or label; footnote references and the rest are not trusted either.
fn inlines_number_every_line<'a>(paragraph: &'a AstNode<'a>) -> bool {
    paragraph.descendants().skip(1).all(|node| {
        let data = node.data();
        if data.attrs.is_some() {
            return false;
        }
        match &data.value {
            NodeValue::Text(_)
            | NodeValue::SoftBreak
            | NodeValue::LineBreak
            | NodeValue::Code(_)
            | NodeValue::HtmlInline(_)
            | NodeValue::Math(_)
            | NodeValue::Emph
            | NodeValue::Strong
            | NodeValue::Strikethrough
            | NodeValue::Escaped => true,
            NodeValue::Link(link) => autolink_text(node, &link.url).is_some(),
            _ => false,
        }
    })
}

fn join_lines(raw: &[(usize, &str)]) -> String {
    let lines: Vec<&str> = raw.iter().map(|(_, line)| *line).collect();
    lines.join("\n")
}

/// The source text of the first line of a top-level block's own text ([`text_lines`]: a
/// paragraph's leading link reference definitions are skipped), untrimmed; `None` when it
/// lies outside `lines` (the source split by [`source_lines`]). Used by
/// [`compat::render_slide`] to read a `Note:` marker as typed.
pub(crate) fn first_text_line<'s, 'a>(node: &'a AstNode<'a>, lines: &[&'s str]) -> Option<&'s str> {
    let (first, _) = text_lines(node, Some(lines));
    lines.get(first.checked_sub(1)?).copied()
}

/// Splits Markdown source into lines the way comrak numbers them: `\n`, `\r\n` and a lone
/// `\r` end a line; a leading byte-order mark is dropped.
pub(crate) fn source_lines(source: &str) -> Vec<&str> {
    let mut rest = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = Vec::new();
    while let Some(end) = rest.find(['\n', '\r']) {
        lines.push(&rest[..end]);
        let ending = if rest[end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        rest = &rest[end + ending..];
    }
    lines.push(rest);
    lines
}

/// A paragraph that is one `Text("--")` (with `smart`, `–`: comrak's smart form of it)
/// covering exactly the two source columns where the paragraph starts, i.e. its source is
/// literally `--`.
fn is_bare_vertical<'a>(paragraph: &'a AstNode<'a>, smart: bool) -> bool {
    let Some(child) = paragraph.first_child() else {
        return false;
    };
    if child.next_sibling().is_some() {
        return false;
    }
    let start = paragraph.data().sourcepos.start;
    let data = child.data();
    let pos = data.sourcepos;
    let dashes = if smart { "–" } else { "--" };
    matches!(&data.value, NodeValue::Text(t) if t == dashes)
        && pos.start == start
        && pos.end.line == start.line
        && pos.end.column == start.column + 1
}

/// The text of a paragraph that could be a `--- {…}` separator: its `Text` nodes, line
/// breaks as `\n` and autolinks' text. `None` for any other inline (code span, emphasis, raw
/// HTML, math, an ordinary link, …), so a code span showing `` `-- {auto-animate}` `` is never
/// a separator, and when the first `Text` does not start where the paragraph does (a leading
/// escape such as `\---`).
fn inline_separator_text<'a>(paragraph: &'a AstNode<'a>) -> Option<String> {
    let start = paragraph.data().sourcepos.start;
    if paragraph.first_child()?.data().sourcepos.start != start {
        return None;
    }
    let mut text = String::new();
    for node in paragraph.children() {
        match &node.data().value {
            NodeValue::Text(t) => text.push_str(t),
            NodeValue::SoftBreak | NodeValue::LineBreak => text.push('\n'),
            NodeValue::Link(link) => text.push_str(&autolink_text(node, &link.url)?),
            _ => return None,
        }
    }
    Some(text)
}

/// The text of an autolink (`https://…`, `www.…`, an e-mail address, `<…>`): a `Link` whose
/// only child is a `Text` equal to its URL (minus the `mailto:`/`http://` comrak adds).
fn autolink_text<'a>(link: &'a AstNode<'a>, url: &str) -> Option<String> {
    let child = link.first_child()?;
    if child.next_sibling().is_some() {
        return None;
    }
    let data = child.data();
    let NodeValue::Text(text) = &data.value else {
        return None;
    };
    let shown = Some(&**text);
    let autolink = Some(url) == shown
        || url.strip_prefix("mailto:") == shown
        || url.strip_prefix("http://") == shown;
    autolink.then(|| text.to_string())
}

/// A separator candidate read from the AST, as its author typed it. Without smart
/// punctuation the inlines are the typed text. With it, comrak turned every run of two or
/// more `-` into `–`/`—`, so a candidate starting with an ASCII dash had its dashes escaped
/// or written as entities (content, as `\--` is in the source): `None`. Anything else goes
/// through [`unsmart`].
fn typed_text(text: &str, smart: bool) -> Option<Cow<'_, str>> {
    if !smart {
        return Some(Cow::Borrowed(text));
    }
    text.starts_with(['—', '–']).then(|| unsmart(text))
}

/// Maps comrak's smart punctuation (`parse.smart`) back in a separator candidate that starts
/// with a smart dash: every `—` is `---` and every `–` is `--` (comrak turns a run of `n`
/// hyphens into em and en dashes worth exactly `n` hyphens), curly quotes are straight and
/// `…` is `...`, anywhere in the text, so values such as `bg--v2.png` survive. Text that does
/// not start with a smart dash is returned unchanged. Only for text parsed with
/// `parse.smart` on ([`typed_text`]); not exact there either, since these characters typed as
/// Unicode come back as ASCII.
fn unsmart(text: &str) -> Cow<'_, str> {
    if !text.starts_with(['—', '–']) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '—' => out.push_str("---"),
            '–' => out.push_str("--"),
            '“' | '”' => out.push('"'),
            '‘' | '’' => out.push('\''),
            '…' => out.push_str("..."),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
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

/// `^---?\s*\{`: a line its author probably meant as an attribute separator.
fn looks_like_attr_separator(line: &str) -> bool {
    line.strip_prefix("--")
        .map(|rest| rest.strip_prefix('-').unwrap_or(rest))
        .is_some_and(|rest| rest.trim_start().starts_with('{'))
}

/// The start of each source line of a paragraph, read from its inlines (parsed with
/// `parse.smart = smart`): the first `Text` (when it starts where the paragraph does) and
/// every `Text` right after a line break, as typed ([`typed_text`]), with their source lines
/// (inlines are numbered as if leading link reference definitions were not there; see
/// [`text_lines`], which is exact here only when the inlines number every line).
fn inline_line_starts<'a>(paragraph: &'a AstNode<'a>, smart: bool) -> Vec<(usize, String)> {
    let start = paragraph.data().sourcepos.start;
    let shift = text_lines(paragraph, None).0 - start.line;
    let mut out = Vec::new();
    let mut line_start = true;
    for (i, node) in paragraph.children().enumerate() {
        let data = node.data();
        match &data.value {
            NodeValue::SoftBreak | NodeValue::LineBreak => {
                line_start = true;
                continue;
            }
            NodeValue::Text(text) if line_start && (i > 0 || data.sourcepos.start == start) => {
                if let Some(typed) = typed_text(text, smart) {
                    out.push((data.sourcepos.start.line + shift, typed.into_owned()));
                }
            }
            _ => {}
        }
        line_start = false;
    }
    out
}

/// Accumulates slides while walking the document's top-level nodes.
struct Builder<'a, 's> {
    /// The source split into lines ([`split_with_source`]), or `None` ([`split_ast`]).
    lines: Option<&'s [&'s str]>,
    /// comrak's `parse.smart`, for paragraphs read from their inlines.
    smart: bool,
    /// Rules or headings mode.
    mode: SplitMode,
    /// Headings mode: the open horizontal stack was started by a section heading (level below
    /// `slide_level`), so a slide-level heading starts a vertical slide under it.
    section: bool,
    /// Finished horizontal slides with their children.
    done: Vec<(RawSlide<'a>, Vec<RawSlide<'a>>)>,
    /// The open horizontal slide and its children; nodes go to its last child, if any.
    current: (RawSlide<'a>, Vec<RawSlide<'a>>),
    /// `current.0` is slide 1 and no separator has been seen yet.
    implicit: bool,
    /// Every footnote definition, in comrak's order (the order they keep on a slide).
    footnotes: Vec<&'a AstNode<'a>>,
    /// Footnote definitions not placed yet, with their lines, the latest first.
    pending: Vec<(&'a AstNode<'a>, usize, usize)>,
    diagnostics: Diagnostics,
}

impl<'a, 's> Builder<'a, 's> {
    fn new(
        root: &'a AstNode<'a>,
        lines: Option<&'s [&'s str]>,
        smart: bool,
        mode: SplitMode,
    ) -> Self {
        let footnotes: Vec<&'a AstNode<'a>> = root
            .children()
            .filter(|node| matches!(node.data().value, NodeValue::FootnoteDefinition(_)))
            .collect();
        let mut pending: Vec<_> = footnotes
            .iter()
            .map(|node| {
                let pos = node.data().sourcepos;
                (*node, pos.start.line, pos.end.line)
            })
            .collect();
        pending.sort_by_key(|&(_, start, _)| std::cmp::Reverse(start));
        Builder {
            lines,
            smart,
            mode,
            section: false,
            done: Vec::new(),
            current: (RawSlide::empty(1), Vec::new()),
            implicit: true,
            footnotes,
            pending,
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
                // placed by their source line, see `flush_footnotes`
                NodeValue::FootnoteDefinition(_) => return Ok(()),
                _ => lines,
            }
        };
        self.flush_footnotes(start);
        if let Some(section) = self.boundary(node) {
            self.heading(node, section, start, end);
            return Ok(());
        }
        let Some(separator) = separator(node, self.lines, self.smart) else {
            self.report_lookalikes(node);
            self.push(node, start, end);
            return Ok(());
        };
        // The separator's own line, after any link reference definitions in its paragraph.
        let start = text_lines(node, self.lines).0;
        let attrs = match separator.attrs {
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
        if separator.direction == Direction::Horizontal {
            self.section = false;
        }
        self.open(separator.direction, slide);
        Ok(())
    }

    /// Headings mode: for a top-level heading that is a slide boundary
    /// (`level <= slide_level`), whether it is a section heading (`level < slide_level`);
    /// `None` for anything else and in rules mode.
    fn boundary(&self, node: &'a AstNode<'a>) -> Option<bool> {
        let SplitMode::Headings { slide_level } = self.mode else {
            return None;
        };
        match node.data().value {
            NodeValue::Heading(heading) if heading.level <= slide_level => {
                Some(heading.level < slide_level)
            }
            _ => None,
        }
    }

    /// Headings mode: a boundary heading (a section heading when `section`) starts a slide
    /// (see the module docs) and becomes its first node, or joins the open slide while that
    /// has no visible content yet.
    fn heading(&mut self, node: &'a AstNode<'a>, section: bool, start: usize, end: usize) {
        let open = self.current.1.last().unwrap_or(&self.current.0);
        if open.nodes.iter().all(|node| compat::is_invisible(node)) {
            if section && self.current.1.is_empty() {
                self.section = true;
            }
            self.push(node, start, end);
            return;
        }
        let direction = if !section && self.section {
            Direction::Vertical
        } else {
            self.section = section;
            Direction::Horizontal
        };
        self.open(direction, RawSlide::empty(start));
        self.push(node, start, end);
    }

    /// Starts the slide a separator opened.
    fn open(&mut self, direction: Direction, mut slide: RawSlide<'a>) {
        if std::mem::take(&mut self.implicit) && self.nothing_before_first_separator() {
            // No content before the first separator: it opens slide 1 itself.
            if direction == Direction::Vertical {
                self.diagnostics
                    .warn(
                        "orphan-vertical",
                        "vertical separator `--` before any horizontal slide; treated as `---`",
                    )
                    .span = Some(slide.span());
            }
            // headings mode: the comments before it start that slide
            let comments = std::mem::take(&mut self.current.0.nodes);
            if let Some(first) = comments.first() {
                slide.start_line = slide.start_line.min(first.data().sourcepos.start.line);
            }
            slide.nodes = comments;
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

    /// While slide 1 is implicit: whether it has no content, so the first separator opens
    /// slide 1 itself. In headings mode HTML comments and `:::notes` blocks alone
    /// ([`compat::is_invisible`]) do not count either (dialect §2.3 "only if it is
    /// non-blank"); in rules mode they are slide 1 (§2.2 "content before the first
    /// separator").
    fn nothing_before_first_separator(&self) -> bool {
        let nodes = &self.current.0.nodes;
        match self.mode {
            SplitMode::Rules => nodes.is_empty(),
            SplitMode::Headings { .. } => nodes.iter().all(|node| compat::is_invisible(node)),
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

    /// Puts the footnote definitions written before `line` on the open slide, as content.
    fn flush_footnotes(&mut self, line: usize) {
        while let Some(&(node, start, end)) = self.pending.last() {
            if start >= line {
                break;
            }
            self.pending.pop();
            self.push(node, start, end);
        }
    }

    /// Reports the lines of an ordinary top-level paragraph that look like attribute
    /// separators (`separator-not-recognized`).
    fn report_lookalikes(&mut self, node: &'a AstNode<'a>) {
        if !matches!(node.data().value, NodeValue::Paragraph) {
            return;
        }
        let (starts, hint) = match self.lines.and_then(|lines| raw_lines(lines, node)) {
            Some(raw) => {
                let starts = raw.into_iter().map(|(n, line)| (n, line.to_string()));
                (starts.collect(), "")
            }
            None => (
                inline_line_starts(node, self.smart),
                " and contain no Markdown inline syntax",
            ),
        };
        for (line, text) in starts {
            if !looks_like_attr_separator(&text) {
                continue;
            }
            let message = format!(
                "`{text}` looks like a slide separator with attributes but is not one: it must \
                 be a paragraph of its own (blank lines around it) matching `--- {{…}}` or \
                 `-- {{…}}`{hint}"
            );
            self.diagnostics
                .warn("separator-not-recognized", message)
                .span = Some(SourceSpan {
                start_line: line,
                end_line: line,
                cell: None,
            });
        }
    }

    fn finish(mut self) -> Result<RawDeck<'a>> {
        self.flush_footnotes(usize::MAX);
        let Builder {
            mut done,
            current,
            footnotes,
            mut diagnostics,
            ..
        } = self;
        done.push(current);
        for (i, (slide, children)) in done.iter_mut().enumerate() {
            finish_slide(slide, &(i + 1).to_string(), &footnotes, &mut diagnostics)?;
            for (j, child) in children.iter_mut().enumerate() {
                let label = format!("{}.{}", i + 1, j + 1);
                finish_slide(child, &label, &footnotes, &mut diagnostics)?;
            }
        }
        let mut diagnostics = diagnostics.into_vec();
        diagnostics.sort_by_key(|d| d.span.as_ref().map(|span| span.start_line));
        Ok(RawDeck {
            slides: done,
            diagnostics: diagnostics.into(),
        })
    }
}

/// Moves footnote definitions to the end (in comrak's order), applies the slide's
/// `<!-- .slide: -->` comments and then the first heading's attributes, and reports
/// `empty-slide`, `unknown-slide-attr`, `invalid-slide-attr` and `misplaced-slide-comment`.
/// Errors: `Error::Parse` for an unbalanced quote in a `.slide:` comment.
fn finish_slide<'a>(
    slide: &mut RawSlide<'a>,
    label: &str,
    footnotes: &[&'a AstNode<'a>],
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    // stable: other nodes (`None`) keep their order, footnotes follow by comrak's order
    slide
        .nodes
        .sort_by_key(|node| footnotes.iter().position(|f| std::ptr::eq(*f, *node)));
    let (comments, lead) = take_slide_comments(&mut slide.nodes)?;
    if let Some(comments) = comments {
        slide.attrs.merge(comments);
    }
    if let Some(lifted) = lift_heading_attrs(&slide.nodes[lead..]) {
        slide.attrs.merge(SlideAttrs::from_parsed(lifted));
    }
    report_misplaced_slide_comments(&slide.nodes, label, diagnostics);
    let span = slide.span();
    let mut warn = |code: &'static str, message: String| {
        diagnostics.warn(code, message).span = Some(span.clone());
    };
    if slide.nodes.is_empty() {
        let only = if slide.attrs.is_empty() {
            ""
        } else {
            " (it only has attributes)"
        };
        warn("empty-slide", format!("slide {label} is empty{only}"));
    }
    for key in &slide.attrs.unknown_keys {
        warn(
            "unknown-slide-attr",
            format!("unknown slide attribute `{key}` on slide {label} (kept verbatim)"),
        );
    }
    for key in &slide.attrs.invalid_keys {
        warn(
            "invalid-slide-attr",
            format!("`{key}` on slide {label} is not a valid attribute name (dropped)"),
        );
    }
    Ok(())
}

/// Removes the `<!-- .slide: -->` comments the slide's attributes come from and returns their
/// attributes merged in order, with the index of the slide's first visible node (once they
/// are removed). They are read among the invisible nodes ([`compat::is_invisible`]: HTML
/// comments and `:::notes` blocks) that start the slide, and, when the first visible node is
/// a heading, among those right after it. Other invisible nodes there stay where they are,
/// so a licence comment or speaker notes before the title change nothing. `None` when there
/// are no such comments.
fn take_slide_comments<'a>(
    nodes: &mut Vec<&'a AstNode<'a>>,
) -> Result<(Option<SlideAttrs>, usize)> {
    let mut attrs: Option<SlideAttrs> = None;
    let lead = take_slide_comments_from(nodes, 0, &mut attrs)?;
    if nodes
        .get(lead)
        .is_some_and(|node| matches!(node.data().value, NodeValue::Heading(_)))
    {
        take_slide_comments_from(nodes, lead + 1, &mut attrs)?;
    }
    Ok((attrs, lead))
}

/// Removes the `.slide:` comments among the invisible nodes (comment-only HTML blocks and
/// `:::notes` blocks) starting at `nodes[at]`, merging their attributes into `attrs` in
/// order. Returns the index of the first node after them.
fn take_slide_comments_from<'a>(
    nodes: &mut Vec<&'a AstNode<'a>>,
    mut at: usize,
    attrs: &mut Option<SlideAttrs>,
) -> Result<usize> {
    while let Some(&node) = nodes.get(at).filter(|node| compat::is_invisible(node)) {
        let Some(text) = compat::slide_comment_node(node) else {
            at += 1;
            continue;
        };
        let line = node.data().sourcepos.start.line;
        let parsed = parse_attr_block(&text).map_err(|err| Error::Parse {
            path: PathBuf::new(),
            line: Some(line),
            message: format!("<!-- .slide: --> comment: {err}"),
        })?;
        attrs
            .get_or_insert_with(SlideAttrs::default)
            .merge(SlideAttrs::from_parsed(parsed));
        nodes.remove(at);
    }
    Ok(at)
}

/// Reports every `.slide:` comment left on a slide, at any depth: in a top-level HTML block
/// that is not where the slide's attributes are read (or holds more than the comment), in an
/// HTML block inside a list, quote or directive, or inline in a heading or paragraph. It is
/// not applied (reveal's plugin would apply it to the slide), so it stays a plain comment.
fn report_misplaced_slide_comments<'a>(
    nodes: &[&'a AstNode<'a>],
    label: &str,
    diagnostics: &mut Diagnostics,
) {
    for node in nodes.iter().flat_map(|node| node.descendants()) {
        let data = node.data();
        let literal = match &data.value {
            NodeValue::HtmlBlock(block) => &block.literal,
            NodeValue::HtmlInline(literal) => literal,
            _ => continue,
        };
        if !compat::mentions_slide_comment(literal) {
            continue;
        }
        let message = format!(
            "`<!-- .slide: … -->` on slide {label} is ignored (kept as a plain comment): it is \
             read only from a top-level HTML block holding just that comment, among the HTML \
             comments (and `:::notes` blocks) that start the slide or that follow the slide's \
             first heading"
        );
        diagnostics.warn("misplaced-slide-comment", message).span = Some(SourceSpan {
            start_line: data.sourcepos.start.line,
            end_line: data.sourcepos.end.line,
            cell: None,
        });
    }
}

/// comrak options exactly as `docs/spec/markdown-dialect.md` §1 with the config defaults
/// (`smart_punctuation = false`, `sourcepos = false`). Test-only: the production builder is
/// `markdown::comrak_options` (T12).
#[cfg(test)]
pub(crate) fn dialect_options() -> comrak::Options<'static> {
    dialect_options_with(false)
}

/// [`dialect_options`] with `parse.smart = smart` (`markdown.smart_punctuation`).
#[cfg(test)]
pub(crate) fn dialect_options_with(smart: bool) -> comrak::Options<'static> {
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
    o.parse.smart = smart;
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

    /// Which entry point a test runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Via {
        /// [`split_ast`] with the `parse.smart` the text was parsed with (without it, that is
        /// [`split`]): AST only.
        Ast,
        /// [`split_with_source`]: raw source lines.
        Source,
    }

    const BOTH: [Via; 2] = [Via::Ast, Via::Source];

    fn run_with<'a>(arena: &'a Arena<'a>, text: &str, via: Via, smart: bool) -> RawDeck<'a> {
        let root = parse_document(arena, text, &dialect_options_with(smart));
        let deck = match via {
            Via::Ast => split_ast(root, SplitMode::Rules, smart),
            Via::Source => split_with_source(root, text, SplitMode::Rules),
        };
        deck.unwrap_or_else(|e| panic!("{via:?} {text:?}: {e}"))
    }

    fn run<'a>(arena: &'a Arena<'a>, text: &str, via: Via) -> RawDeck<'a> {
        run_with(arena, text, via, false)
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
            for via in BOTH {
                let arena = Arena::new();
                let deck = run(&arena, &text, via);
                assert_eq!(shape(&deck), *expected_shape, "{name} {via:?}");
                assert_eq!(diags(&deck), *expected_diags, "{name} {via:?}");
            }
        }
    }

    /// Per slide: span, node count and attributes.
    fn summary(deck: &RawDeck<'_>) -> Vec<(usize, usize, usize, SlideAttrs)> {
        flatten(deck)
            .iter()
            .map(|s| (s.start_line, s.end_line, s.nodes.len(), s.attrs.clone()))
            .collect()
    }

    #[test]
    fn both_entry_points_agree_on_the_fixtures() {
        for (name, _, _) in FIXTURE_SHAPES {
            let text = fixture(name);
            let (a, b) = (Arena::new(), Arena::new());
            let ast = run(&a, &text, Via::Ast);
            let source = run(&b, &text, Via::Source);
            assert_eq!(summary(&ast), summary(&source), "{name}");
        }
    }

    /// Spans are 1-based and come from comrak's sourcepos: every separator node's line
    /// starts exactly one slide, a slide not opened by a separator starts at its earliest
    /// node, and every slide ends at its latest node (or at its separator when empty).
    #[test]
    fn fixture_spans_match_sourcepos() {
        for (name, _, _) in FIXTURE_SHAPES {
            let text = fixture(name);
            let lines = source_lines(&text);
            let arena = Arena::new();
            let root = parse(&arena, &text);
            let separator_lines: Vec<(usize, usize)> = root
                .children()
                .filter(|node| separator(node, Some(&lines), false).is_some())
                .map(line_range)
                .collect();
            let deck = split_with_source(root, &text, SplitMode::Rules).unwrap();
            let slides = flatten(&deck);
            let opened = slides
                .iter()
                .filter(|s| {
                    separator_lines
                        .iter()
                        .any(|(start, _)| *start == s.start_line)
                })
                .count();
            assert_eq!(opened, separator_lines.len(), "{name}");
            for slide in &slides {
                assert!(
                    slide.start_line >= 1 && slide.end_line >= slide.start_line,
                    "{name}"
                );
                let Some(first) = slide.nodes.iter().map(|n| line_range(n).0).min() else {
                    let sep = separator_lines.iter().find(|(s, _)| *s == slide.start_line);
                    assert_eq!(sep.map(|(_, end)| *end), Some(slide.end_line), "{name}");
                    continue;
                };
                let last_end = slide.nodes.iter().map(|n| line_range(n).1).max();
                assert_eq!(last_end, Some(slide.end_line), "{name}");
                let opened_by_separator =
                    separator_lines.iter().any(|(s, _)| *s == slide.start_line);
                assert!(
                    opened_by_separator || first == slide.start_line,
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
        let deck = run(&arena, &text, Via::Source);
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
            assert!(slide.attrs.invalid_keys.is_empty(), "{}", slide.start_line);
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
        let deck = run(&arena, &text, Via::Source);
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
        for via in BOTH {
            let arena = Arena::new();
            let deck = run(&arena, "# A\n\nText\n--\n\nmore\n", via);
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
            let deck = run(&arena, "# A\n\nText\n\n--\n\nmore\n", via);
            assert_eq!(shape(&deck), "1-3 [5-7]");
        }
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
                "an empty slide with attributes is still reported (dialect §2.2)",
                "# A\n\n--- {background=x.png}\n\n---\n\n# B\n",
                "1-1 | 3-3 | 5-7",
                &[("empty-slide", 3)],
            ),
            (
                "`---` with attributes, then `--`: the empty first slide is reported",
                "# A\n\n--- {background=x.png}\n\n--\n\n# B\n",
                "1-1 | 3-3 [5-7]",
                &[("empty-slide", 3)],
            ),
            (
                "`---` closes the vertical stack",
                "# A\n\n--\n\n# A.1\n\n---\n\n# B\n",
                "1-1 [3-5] | 7-9",
                &[],
            ),
            (
                "code spans and emphasis that do not start the line are not separators",
                "# A\n\n`-- {auto-animate}`\n\n*--- {.x}*\n\n_--_\n",
                "1-7",
                &[],
            ),
            (
                "`--` must be the whole paragraph; a `{…}` line inside one is reported",
                "# A\n\n--\nmore\n\nText\n-- {x}\n",
                "1-7",
                &[("separator-not-recognized", 7)],
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
                &[("separator-not-recognized", 3)],
            ),
            (
                "trailing text after the braces",
                "# A\n\n--- {.x} trailing\n",
                "1-3",
                &[("separator-not-recognized", 3)],
            ),
            (
                "`\\s*` does match a line break",
                "# A\n\n--\n{.x}\n\n# B\n",
                "1-1 [3-6]",
                &[],
            ),
            (
                "separators nested in containers are content (and not reported)",
                "# A\n\n> ---\n\n- item\n\n  --\n\n  -- {.x}\n\n:::note\n---\n:::\n",
                "1-13",
                &[],
            ),
            (
                "escaped or encoded `--` is a literal paragraph, not a separator",
                "# A\n\n\\--\n\n-\\-\n\n&#45;&#45;\n\n\\--- {.x}\n",
                "1-9",
                &[],
            ),
            (
                "indentation and trailing spaces around `--` are fine",
                "# A\n\n   --  \n\n# B\n",
                "1-1 [3-5]",
                &[],
            ),
        ];
        for (what, text, expected_shape, expected_diags) in cases {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run(&arena, text, via);
                assert_eq!(shape(&deck), *expected_shape, "{what} ({via:?})");
                assert_eq!(diags(&deck), *expected_diags, "{what} ({via:?})");
            }
        }
    }

    #[test]
    fn separator_attributes_reach_the_slide_they_open() {
        let text = "-- {.first}\n\n# A\n\n--- {background=https://example.org/bg.png}\n\n# B\n\n\
                    --\n{.x}\n\n# B.1\n\n--- {data-m=me@example.org}\n";
        for via in BOTH {
            let arena = Arena::new();
            let deck = run(&arena, text, via);
            assert_eq!(shape(&deck), "1-3 | 5-7 [9-12] | 14-14", "{via:?}");
            // the orphan `-- {…}` keeps its attributes on slide 1
            assert_eq!(deck.slides[0].0.attrs.classes, ["first"]);
            // an autolinked URL or e-mail address is still part of the separator text
            assert_eq!(
                deck.slides[1].0.attrs.attrs["data-background-image"],
                "https://example.org/bg.png"
            );
            assert_eq!(deck.slides[1].1[0].attrs.classes, ["x"]);
            assert_eq!(deck.slides[2].0.attrs.attrs["data-m"], "me@example.org");
        }
    }

    /// Values with Markdown inline syntax. Read from the source they are exact; read from the
    /// AST the paragraph cannot be trusted, so it stays content and is reported.
    #[test]
    fn inline_syntax_in_separator_values() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "--- {background=img/_draft_.png}",
                "data-background-image",
                "img/_draft_.png",
            ),
            (
                "--- {background=__init__.png}",
                "data-background-image",
                "__init__.png",
            ),
            ("--- {data-x=~a~}", "data-x", "~a~"),
            ("--- {data-x=a*b*c}", "data-x", "a*b*c"),
            ("--- {data-x=\"<b>\"}", "data-x", "<b>"),
            ("--- {data-x=$a$}", "data-x", "$a$"),
            ("--- {data-x=`a`}", "data-x", "`a`"),
            ("--- {data-x=[a](b)}", "data-x", "[a](b)"),
            ("-- {data-x=**b**}", "data-x", "**b**"),
        ];
        for (line, key, value) in cases {
            let text = format!("# A\n\n{line}\n\n# B\n");
            let arena = Arena::new();
            let deck = run(&arena, &text, Via::Source);
            let slides = flatten(&deck);
            assert_eq!(slides.len(), 2, "{line}");
            assert_eq!(slides[1].start_line, 3, "{line}");
            assert_eq!(slides[1].attrs.attrs[*key], *value, "{line}");
            assert!(
                !diags(&deck).contains(&("separator-not-recognized", 3)),
                "{line}"
            );

            let arena = Arena::new();
            let deck = run(&arena, &text, Via::Ast);
            assert_eq!(shape(&deck), "1-5", "{line}");
            assert_eq!(diags(&deck)[0], ("separator-not-recognized", 3), "{line}");
        }
    }

    /// Backslash escapes and entities: from the source the separator agrees with the heading
    /// grammar (§3.1); from the AST they are already resolved (documented limitation).
    #[test]
    fn escapes_in_separator_values() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"--- {title="a \"q\" b"}"#, "title", r#"a "q" b"#),
            (r#"--- {title="a \"b"}"#, "title", r#"a "b"#),
            (r#"--- {data-path="C:\\dir"}"#, "data-path", r"C:\dir"),
            ("--- {data-x=\"&lt;b&gt;\"}", "data-x", "&lt;b&gt;"),
        ];
        for (line, key, value) in cases {
            let text = format!("# A\n\n{line}\n\n# B\n");
            let arena = Arena::new();
            let deck = run(&arena, &text, Via::Source);
            assert_eq!(deck.slides[1].0.attrs.attrs[*key], *value, "{line}");
            // the heading form reads the same block identically
            let heading = format!("# B {}\n", &line[4..]);
            let arena = Arena::new();
            let lifted = run(&arena, &heading, Via::Source);
            assert_eq!(lifted.slides[0].0.attrs.attrs[*key], *value, "{heading}");
        }
        // pinned AST-only behaviour
        let arena = Arena::new();
        let deck = run(&arena, "# A\n\n--- {title=\"a \\\"q\\\" b\"}\n", Via::Ast);
        assert_eq!(deck.slides[1].0.attrs.attrs["title"], "a q b");
        let arena = Arena::new();
        let deck = run(&arena, "# A\n\n--- {data-x=\"&lt;b&gt;\"}\n", Via::Ast);
        assert_eq!(deck.slides[1].0.attrs.attrs["data-x"], "<b>");
        let arena = Arena::new();
        let root = parse(&arena, "# A\n\n--- {title=\"a \\\"b\"}\n");
        assert!(matches!(
            split(root, SplitMode::Rules),
            Err(Error::Parse { line: Some(3), .. })
        ));
    }

    /// `markdown.smart_punctuation = true` turns `--` into `–` and quotes into curly ones;
    /// separators keep working through both entry points.
    #[test]
    fn smart_punctuation_keeps_separators() {
        let text = "# A\n\n--\n\n# A.1\n\n-- {auto-animate}\n\n# A.2\n\n\
                    --- {.dark data-x=\"q\" y='s' z=a...b}\n\n# B\n\n\\--\n\n---- {x}\n";
        for via in BOTH {
            let arena = Arena::new();
            let deck = run_with(&arena, text, via, true);
            assert_eq!(shape(&deck), "1-1 [3-5, 7-9] | 11-17", "{via:?}");
            let slides = flatten(&deck);
            assert_eq!(
                slides[2].attrs.attrs,
                BTreeMap::from([("data-auto-animate".to_string(), String::new())])
            );
            assert_eq!(slides[3].attrs.classes, ["dark"], "{via:?}");
            assert_eq!(
                slides[3].attrs.attrs,
                BTreeMap::from([
                    ("data-x".to_string(), "q".to_string()),
                    ("y".to_string(), "s".to_string()),
                    ("z".to_string(), "a...b".to_string()),
                ]),
                "{via:?}"
            );
            // `\--` and `---- {x}` stay content
            assert_eq!(slides[3].nodes.len(), 3, "{via:?}");
        }
    }

    #[test]
    fn invalid_attribute_names_are_dropped_and_reported() {
        for via in BOTH {
            let arena = Arena::new();
            let text = "# A\n\n--- {\"a b\"=c '>x'=y ok=1 data-z=2}\n\n# B\n\n--- {a} {b}\n\n# C\n";
            let deck = run(&arena, text, via);
            let slides = flatten(&deck);
            assert_eq!(slides[1].attrs.invalid_keys, ["a b", ">x"], "{via:?}");
            assert_eq!(
                slides[1].attrs.attrs,
                BTreeMap::from([
                    ("data-z".to_string(), "2".to_string()),
                    ("ok".to_string(), "1".to_string()),
                ])
            );
            assert_eq!(slides[2].attrs.invalid_keys, ["a}", "{b"], "{via:?}");
            assert!(slides[2].attrs.attrs.is_empty());
            assert_eq!(
                diags(&deck),
                [
                    ("unknown-slide-attr", 3),
                    ("invalid-slide-attr", 3),
                    ("invalid-slide-attr", 3),
                    ("invalid-slide-attr", 7),
                    ("invalid-slide-attr", 7),
                ],
                "{via:?}"
            );
            let messages: Vec<String> =
                deck.diagnostics.iter().map(|d| d.message.clone()).collect();
            assert_eq!(
                messages[1],
                "`a b` on slide 2 is not a valid attribute name (dropped)"
            );
        }
    }

    #[test]
    fn background_shorthand_from_a_later_source_wins() {
        for via in BOTH {
            let arena = Arena::new();
            let text = "# A\n\n--- {background=photo.jpg}\n\n# B {background=\"navy\"}\n\n\
                        --- {background=#000}\n\n# C {background=\"bg.png\"}\n";
            let deck = run(&arena, text, via);
            let slides = flatten(&deck);
            assert_eq!(
                slides[1].attrs.attrs,
                BTreeMap::from([("data-background-color".to_string(), "navy".to_string())]),
                "{via:?}"
            );
            assert_eq!(
                slides[2].attrs.attrs,
                BTreeMap::from([("data-background-image".to_string(), "bg.png".to_string())]),
                "{via:?}"
            );
        }
    }

    #[test]
    fn unbalanced_quote_in_separator_is_a_parse_error_with_line() {
        for via in BOTH {
            let text = "# A\n\n--- {k=\"v}\n";
            let arena = Arena::new();
            let root = parse(&arena, text);
            let err = match via {
                Via::Ast => split(root, SplitMode::Rules),
                Via::Source => split_with_source(root, text, SplitMode::Rules),
            }
            .unwrap_err();
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
    }

    fn run_mode<'a>(arena: &'a Arena<'a>, text: &str, via: Via, mode: SplitMode) -> RawDeck<'a> {
        let root = parse(arena, text);
        let deck = match via {
            Via::Ast => split(root, mode),
            Via::Source => split_with_source(root, text, mode),
        };
        deck.unwrap_or_else(|e| panic!("{via:?} {mode:?} {text:?}: {e}"))
    }

    fn headings(slide_level: u8) -> SplitMode {
        SplitMode::Headings { slide_level }
    }

    #[test]
    fn headings_mode_splits_as_specified() {
        const SECTIONS: &str = "# A\n\n## A.1\n\n### deeper\n\n## A.2\n\n# B\n";
        let cases: &[(&str, &str, u8, &str, Diags)] = &[
            (
                "`#` horizontal, `##` vertical, `###` content",
                SECTIONS,
                2,
                "1-1 [3-5, 7-7] | 9-9",
                &[],
            ),
            (
                "slide_level 3: `#` and `##` horizontal, `###` vertical",
                SECTIONS,
                3,
                "1-1 | 3-3 [5-5] | 7-7 | 9-9",
                &[],
            ),
            (
                "slide_level 1: every `#` is a horizontal slide",
                SECTIONS,
                1,
                "1-7 | 9-9",
                &[],
            ),
            (
                "slide_level 0: headings are content",
                SECTIONS,
                0,
                "1-9",
                &[],
            ),
            (
                "`##` slides without any `#` are horizontal (pandoc)",
                "## A\n\ntext\n\n## B\n\n### c\n",
                2,
                "1-3 | 5-7",
                &[],
            ),
            (
                "consecutive headings",
                "# A\n## B\n## C\n",
                2,
                "1-1 [2-2, 3-3]",
                &[],
            ),
            (
                "content before the first heading is slide 1",
                "Intro\n\n# A\n",
                2,
                "1-1 | 3-3",
                &[],
            ),
            (
                "comments alone before the first heading are not a slide",
                "<!-- licence -->\n\n<!-- more -->\n\n# A\n\ntext\n",
                2,
                "1-7",
                &[],
            ),
            (
                "comments alone before a leading separator go with it",
                "<!-- licence -->\n\n---\n\n# A\n\ntext\n",
                2,
                "1-7",
                &[],
            ),
            (
                "comments alone before a leading `--`: still an orphan",
                "<!-- licence -->\n\n--\n\n# A\n",
                2,
                "1-5",
                &[("orphan-vertical", 3)],
            ),
            (
                "front matter is not content",
                "---\ntitle: x\n---\n\n# A\n",
                2,
                "5-5",
                &[],
            ),
            (
                "an empty document is one empty slide",
                "",
                2,
                "1-1",
                &[("empty-slide", 1)],
            ),
            (
                "mixed: `---` + heading is one slide; `##` after `---` is horizontal",
                "# A\n\n---\n\n## B\n\n## C\n\n# D\n\n--\n\n## E\n",
                2,
                "1-1 | 3-5 | 7-7 | 9-9 [11-13]",
                &[],
            ),
            (
                "mixed: content after `---` keeps the next `##` horizontal",
                "# A\n\n---\n\ntext\n\n## B\n",
                2,
                "1-1 | 3-5 | 7-7",
                &[],
            ),
            (
                "mixed: `--` opens a vertical slide, a later `##` another one",
                "# A\n\ntext\n\n--\n\nmore\n\n## B\n",
                2,
                "1-3 [5-7, 9-9]",
                &[],
            ),
            (
                "mixed: a leading separator still opens slide 1",
                "---\n\n# A\n\n## B\n",
                2,
                "1-3 [5-5]",
                &[],
            ),
            (
                "mixed: a trailing separator is an empty slide",
                "# A\n\n---\n",
                2,
                "1-1 | 3-3",
                &[("empty-slide", 3)],
            ),
            (
                "setext headings count",
                "A\n===\n\nB\n---\n\ntext\n",
                2,
                "1-2 [4-7]",
                &[],
            ),
            (
                "headings in containers are content",
                "# A\n\n> # quoted\n\n- # item\n\n:::note\n# inside\n:::\n",
                2,
                "1-9",
                &[],
            ),
        ];
        for (what, text, slide_level, expected_shape, expected_diags) in cases {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run_mode(&arena, text, via, headings(*slide_level));
                assert_eq!(shape(&deck), *expected_shape, "{what} ({via:?})");
                assert_eq!(diags(&deck), *expected_diags, "{what} ({via:?})");
                // a boundary heading only follows comments on its slide
                for slide in flatten(&deck) {
                    for (i, node) in slide.nodes.iter().enumerate() {
                        if let NodeValue::Heading(h) = node.data().value
                            && h.level <= *slide_level
                        {
                            let before = &slide.nodes[..i];
                            assert!(before.iter().all(|n| compat::is_blank(n)), "{what}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn headings_mode_lifts_boundary_heading_attributes() {
        let text = "# A {#a layout=section}\n\n## B {.dark background=\"navy\"}\n\n\
                    --- {.sep data-x=\"1\"}\n\n## C {data-x=\"2\"}\n";
        for via in BOTH {
            let arena = Arena::new();
            let deck = run_mode(&arena, text, via, headings(2));
            assert_eq!(shape(&deck), "1-1 [3-3] | 5-7", "{via:?}");
            let slides = flatten(&deck);
            assert_eq!(slides[0].attrs.id.as_deref(), Some("a"));
            assert_eq!(slides[0].attrs.layout.as_deref(), Some("section"));
            assert_eq!(slides[1].attrs.classes, ["dark"]);
            assert_eq!(
                slides[1].attrs.attrs["data-background-color"], "navy",
                "{via:?}"
            );
            // the separator's attributes apply, the joining heading's win
            assert_eq!(slides[2].attrs.classes, ["sep"]);
            assert_eq!(slides[2].attrs.attrs["data-x"], "2");
            for slide in &slides {
                assert!(slide.nodes[0].data().attrs.is_none(), "lifted: {via:?}");
            }
        }
    }

    /// A slide whose first nodes are plain comments: they are invisible, so the heading after
    /// them is still the slide's first node for lifting and `.slide:` comments, in both modes
    /// and through both entry points (the heading joined a comment-only slide in headings
    /// mode).
    #[test]
    fn comments_before_the_first_heading_change_nothing() {
        let licence = "<!-- licence -->\n\n# A {#a .x layout=title}\n\
                       <!-- .slide: data-state=\"s\" -->\n\ntext\n\n# B\n";
        let after_rule = "# A\n\ntext\n\n---\n\n<!-- TODO: polish -->\n\n\
                          ## B {.dark background=\"#123\"}\n";
        for mode in [SplitMode::Rules, headings(2)] {
            for via in BOTH {
                let label = format!("{mode:?} {via:?}");
                let arena = Arena::new();
                let deck = run_mode(&arena, licence, via, mode);
                assert_eq!(diags(&deck), [], "{label}");
                let first = &deck.slides[0].0;
                assert_eq!(first.attrs.id.as_deref(), Some("a"), "{label}");
                assert_eq!(first.attrs.classes, ["x"], "{label}");
                assert_eq!(first.attrs.layout.as_deref(), Some("title"), "{label}");
                assert_eq!(first.attrs.attrs["data-state"], "s", "{label}");
                // the licence stays first, the heading is clean, the `.slide:` comment gone
                assert!(compat::is_blank(first.nodes[0]), "{label}");
                assert!(matches!(first.nodes[1].data().value, NodeValue::Heading(_)));
                assert!(first.nodes[1].data().attrs.is_none(), "{label}");
                assert!(
                    first
                        .nodes
                        .iter()
                        .all(|n| compat::slide_comment_node(n).is_none())
                );

                let arena = Arena::new();
                let deck = run_mode(&arena, after_rule, via, mode);
                assert_eq!(shape(&deck), "1-3 | 5-9", "{label}");
                assert_eq!(diags(&deck), [], "{label}");
                let second = &deck.slides[1].0;
                assert_eq!(second.attrs.classes, ["dark"], "{label}");
                assert_eq!(
                    second.attrs.attrs["data-background-color"], "#123",
                    "{label}"
                );
                assert!(second.nodes[1].data().attrs.is_none(), "{label}");
            }
        }
        // headings mode: the heading joined the comment-only slide 1
        for via in BOTH {
            let arena = Arena::new();
            let deck = run_mode(&arena, licence, via, headings(2));
            assert_eq!(shape(&deck), "1-6 | 8-8", "{via:?}");
        }
    }

    /// A3 (F12): `:::notes` before the title is invisible, like a comment: the heading is
    /// still the slide's first visible node, and in headings mode it joins the slide.
    #[test]
    fn notes_before_the_first_heading_change_nothing() {
        let notes_first = ":::notes\nHi.\n:::\n\n<!-- .slide: data-state=\"s\" -->\n\n\
                           # A {#a .x layout=title}\n\ntext\n\n# B\n";
        for mode in [SplitMode::Rules, headings(2)] {
            for via in BOTH {
                let label = format!("{mode:?} {via:?}");
                let arena = Arena::new();
                let deck = run_mode(&arena, notes_first, via, mode);
                assert_eq!(diags(&deck), [], "{label}");
                let first = &deck.slides[0].0;
                assert_eq!(first.attrs.id.as_deref(), Some("a"), "{label}");
                assert_eq!(first.attrs.classes, ["x"], "{label}");
                assert_eq!(first.attrs.layout.as_deref(), Some("title"), "{label}");
                assert_eq!(first.attrs.attrs["data-state"], "s", "{label}");
                assert!(compat::is_invisible(first.nodes[0]), "{label}");
                assert!(first.nodes[1].data().attrs.is_none(), "{label}");
            }
            // notes after a separator, then the heading
            let after_rule = "# A\n\n---\n\n:::notes\nn\n:::\n\n## B {.b}\n\n## C\n";
            for via in BOTH {
                let arena = Arena::new();
                let deck = run_mode(&arena, after_rule, via, mode);
                let expected = match mode {
                    SplitMode::Rules => "1-1 | 3-11",
                    SplitMode::Headings { .. } => "1-1 | 3-9 | 11-11",
                };
                assert_eq!(shape(&deck), expected, "{mode:?} {via:?}");
                assert_eq!(deck.slides[1].0.attrs.classes, ["b"], "{mode:?} {via:?}");
            }
        }
        // headings mode: the heading joined the notes-only slide 1
        for via in BOTH {
            let arena = Arena::new();
            let deck = run_mode(&arena, notes_first, via, headings(2));
            assert_eq!(shape(&deck), "1-9 | 11-11", "{via:?}");
        }
        // a notes block that is not first is content as before: no lifting after a paragraph
        let arena = Arena::new();
        let deck = run_mode(
            &arena,
            "text\n\n:::notes\nn\n:::\n\n# H {.h}\n",
            Via::Source,
            SplitMode::Rules,
        );
        assert!(deck.slides[0].0.attrs.classes.is_empty());
    }

    /// A3 fix round: the heading the splitter lifts at the start of a document, for notebook
    /// cells that must keep its attributes.
    #[test]
    fn lifted_heading_skips_what_the_splitter_skips() {
        let opts = dialect_options();
        for (text, expected) in [
            (
                ":::notes\nn\n:::\n\n<!-- c -->\n\n[^1]: f\n\n## H {.x}\n",
                Some(2),
            ),
            ("# H {.x}\n", Some(1)),
            ("text\n\n# H {.x}\n", None),
            ("---\n\n# H {.x}\n", None),
            ("<!-- c -->\n\n:::center\n# H\n:::\n", None),
            ("", None),
        ] {
            let arena = Arena::new();
            let root = parse_document(&arena, text, &opts);
            let level = lifted_heading(root).map(|node| match &node.data().value {
                NodeValue::Heading(heading) => heading.level,
                _ => 0,
            });
            assert_eq!(level, expected, "{text:?}");
        }
    }

    #[test]
    fn headings_mode_fixture_shapes() {
        let text = fixture("headings-mode.md");
        // the fixture's own front matter sets `split: headings`; `parse` applies it, this
        // test passes the mode directly
        for (level, expected) in [
            (
                2,
                "24-24 | 26-26 [28-35, 37-41] | 43-45 | 47-53 | 55-57 | 59-59 [61-65]",
            ),
            (
                3,
                "24-24 | 26-26 | 28-30 [32-35] | 37-41 | 43-45 | 47-53 | 55-57 | 59-59 [61-65]",
            ),
        ] {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run_mode(&arena, &text, via, headings(level));
                assert_eq!(shape(&deck), expected, "slide_level {level} ({via:?})");
                assert_eq!(diags(&deck), [], "slide_level {level} ({via:?})");
            }
        }
    }

    #[test]
    fn slide_comments_set_attributes_with_precedence() {
        let text = "\
--- {.sep data-a=\"sep\" data-b=\"sep\" data-c=\"sep\"}

<!-- .slide: data-b=\"comment\" data-c=\"comment\" class=\"x\" #cid -->
<!-- .slide: background=navy -->

# T {data-c=\"heading\"}

---

# U {.u}
<!-- .slide: data-background=\"#f00\" .v -->

text
";
        for mode in [SplitMode::Rules, headings(2)] {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run_mode(&arena, text, via, mode);
                let label = format!("{mode:?} {via:?}");
                assert_eq!(shape(&deck), "1-6 | 8-13", "{label}");
                assert_eq!(diags(&deck), [], "{label}");
                let slides = flatten(&deck);
                let first = &slides[0].attrs;
                assert_eq!(first.classes, ["sep", "x"], "{label}");
                assert_eq!(first.id.as_deref(), Some("cid"));
                assert_eq!(
                    first.attrs,
                    BTreeMap::from([
                        ("data-a".to_string(), "sep".to_string()),
                        ("data-b".to_string(), "comment".to_string()),
                        ("data-background-color".to_string(), "navy".to_string()),
                        ("data-c".to_string(), "heading".to_string()),
                    ]),
                    "{label}"
                );
                // the comments are gone, the heading is first and clean
                assert_eq!(slides[0].nodes.len(), 1, "{label}");
                assert!(slides[0].nodes[0].data().attrs.is_none());
                // right after the first heading: also read
                let second = &slides[1].attrs;
                // classes accumulate in precedence order: comment, then heading
                assert_eq!(second.classes, ["v", "u"], "{label}");
                assert_eq!(second.attrs["data-background"], "#f00", "{label}");
                assert_eq!(slides[1].nodes.len(), 2, "{label}");
            }
        }
    }

    #[test]
    fn misplaced_or_malformed_slide_comments() {
        // (text, shape, diagnostics, classes of the last slide)
        let cases: &[(&str, &str, Diags, &[&str])] = &[
            (
                "# A\n\ntext\n\n<!-- .slide: class=\"late\" -->\n",
                "1-5",
                &[("misplaced-slide-comment", 5)],
                &[],
            ),
            (
                "<!-- .slide: class=\"a\" --> trailing text\n\n# A\n",
                "1-3",
                &[("misplaced-slide-comment", 1)],
                &[],
            ),
            // nested in a list, inline in a paragraph or heading: reported, not read
            (
                "- item\n\n  <!-- .slide: class=\"nested\" -->\n",
                "1-3",
                &[("misplaced-slide-comment", 3)],
                &[],
            ),
            (
                "# T <!-- .slide: class=\"in-heading\" -->\n\nText\n<!-- .slide: class=\"x\" --> y\n",
                "1-4",
                &[
                    ("misplaced-slide-comment", 1),
                    ("misplaced-slide-comment", 4),
                ],
                &[],
            ),
            (
                ":::note\n<!-- .slide: class=\"in-directive\" -->\n:::\n",
                "1-3",
                &[("misplaced-slide-comment", 2)],
                &[],
            ),
            // plain comments before or between are skipped: read
            (
                "<!-- c -->\n\n<!-- .slide: class=\"a\" -->\n\n# A {.b}\n<!-- d -->\n\
                 <!-- .slide: class=\"c\" -->\n\ntext\n",
                "1-9",
                &[],
                &["a", "c", "b"],
            ),
            (
                "# A\n\n---\n\n<!-- .slide: class=\"only\" -->\n",
                "1-1 | 3-5",
                &[("empty-slide", 3)],
                &["only"],
            ),
            (
                "# A\n\n<!-- .slide: data-k=v -->\n\n<!-- .slide: data-j=w -->\n\n\
                 <!-- .slide: data-x=y -->\n\npara\n\n<!-- .slide: class=\"no\" -->\n",
                "1-11",
                &[("misplaced-slide-comment", 11)],
                &[],
            ),
        ];
        for (text, expected_shape, expected_diags, classes) in cases {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run(&arena, text, via);
                assert_eq!(shape(&deck), *expected_shape, "{text:?} {via:?}");
                assert_eq!(diags(&deck), *expected_diags, "{text:?} {via:?}");
                let last = flatten(&deck).pop().unwrap();
                assert_eq!(last.attrs.classes, *classes, "{text:?} {via:?}");
            }
        }
        // two comments right after a heading are both read
        let arena = Arena::new();
        let deck = run(
            &arena,
            "# A\n\n<!-- .slide: data-k=v -->\n\n<!-- .slide: data-j=w -->\n\npara\n",
            Via::Source,
        );
        assert_eq!(deck.slides[0].0.attrs.attrs.len(), 2);
        // an unbalanced quote is a parse error on the comment's line
        for via in BOTH {
            let text = "# A\n\n---\n\n<!-- .slide: title=\"open -->\n\n# B\n";
            let arena = Arena::new();
            let root = parse(&arena, text);
            let result = match via {
                Via::Ast => split(root, SplitMode::Rules),
                Via::Source => split_with_source(root, text, SplitMode::Rules),
            };
            assert!(
                matches!(&result, Err(Error::Parse { line: Some(5), message, .. })
                    if message.contains("unbalanced")),
                "{via:?}: {result:?}"
            );
        }
    }

    /// A paragraph whose first line starts with `[` and that hides many line endings (a link
    /// title over thousands of lines) used to re-parse every prefix of itself: quadratic.
    /// Only prefixes that can end a link reference definition are parsed now.
    #[test]
    fn long_link_titles_are_not_parsed_once_per_line() {
        let n = 20_000;
        let lines = |head: &str, tail: &str| -> Vec<String> {
            let mut out = vec![head.to_string()];
            out.extend(std::iter::repeat_n("t".to_string(), n));
            out.push(tail.to_string());
            out
        };
        // no `]:` anywhere: nothing to parse
        let block = lines("[l](u \"t", "t\")");
        let block: Vec<&str> = block.iter().map(String::as_str).collect();
        assert_eq!(definition_candidates(&block, n + 1).count(), 0);
        // a definition, then such a link: two short prefixes
        let mut with_def = vec!["[x]: http://a".to_string()];
        with_def.extend(lines("[l](u \"t", "t\")"));
        let with_def: Vec<&str> = with_def.iter().map(String::as_str).collect();
        assert_eq!(
            definition_candidates(&with_def, n + 1).collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(definition_lines(&with_def, n + 1), 1);
        // a definition whose title spans the lines: the whole prefix, parsed once
        let long_title = lines("[x]: http://a \"t", "t\"");
        let mut long_title: Vec<&str> = long_title.iter().map(String::as_str).collect();
        long_title.push("text");
        assert_eq!(
            definition_candidates(&long_title, n + 2).next(),
            Some(n + 2)
        );
        assert_eq!(definition_lines(&long_title, n + 2), n + 2);
        // crafted: a `]:` (or a title's closing quote) on every hidden line makes every line a
        // candidate; only `MAX_DEFINITION_PARSES` of them are parsed, and the paragraph is then
        // read as if it had no definitions (documented fallback). Quadratic, the 5000 lines
        // took about a minute here (debug build); now a few hundredths of a second.
        let m = 5_000;
        for (head, line, tail) in [
            ("[l](u (t", "x]: \"", "t))"),
            ("[l](u \"t", "]:", "t\")"),
            ("[x]: http://a\n[l](u \"t", "]:", "t\")"),
        ] {
            let text = format!("{head}{}\n{tail}", format!("\n{line}").repeat(m));
            let block: Vec<&str> = text.lines().collect();
            assert!(definition_candidates(&block, block.len()).count() >= m);
            assert_eq!(definition_lines(&block, block.len()), 0, "{head}");
        }
        // end to end (would take minutes if quadratic)
        for text in [
            format!(
                "# A\n\n[l](u \"t{}\")\n\n--- {{.x}}\n\n# B\n",
                "\nt".repeat(n)
            ),
            format!("# A\n\n[x]: http://a\n[l](u \"t{}\")\n", "\nt".repeat(n)),
            format!(
                "# A\n\n[l](u (t{}\nt))\n\n--- {{.x}}\n",
                "\nx]: \"".repeat(m)
            ),
            format!("# A\n\n[l](u \"t{}\nt\")\n\nNote: n\n", "\n]:".repeat(m)),
        ] {
            let arena = Arena::new();
            let root = parse(&arena, &text);
            let deck = split_with_source(root, &text, SplitMode::Rules).unwrap();
            assert!(!deck.slides.is_empty());
        }
    }

    /// The candidates never skip a prefix that is all definitions: compare with trying every
    /// prefix, over the paragraphs of the tests above and some definition shapes.
    #[test]
    fn definition_candidates_keep_every_possible_prefix() {
        let blocks: &[&[&str]] = &[
            &["[x]: http://a", "--- {.a}"],
            &["[x]: http://a", "[y]: http://b", "-- {.a}"],
            &["[x]:", "  http://a", "  \"t\"", "-- {.a}"],
            &["[x]: http://a 'title'  ", "text"],
            &["[x]: http://a (title)\t", "text"],
            &["[x]: http://a", "\"title", "over lines\"", "text"],
            &["[foo", "bar]: /url", "text"],
            &["[x]: <http://a>", "[y]:", "<b> \"t\"", "text"],
            &["[x]: http://a", "text", "[y]: http://b"],
            &["[l](u \"t", "t\")", "[x]: y"],
        ];
        let options = comrak::Options::default();
        for block in blocks {
            let limit = block.len();
            let brute = (1..block.len())
                .rev()
                .find(|&k| {
                    let arena = Arena::new();
                    parse_document(&arena, &block[..k].join("\n"), &options)
                        .first_child()
                        .is_none()
                })
                .unwrap_or(0);
            let brute = if block[0].starts_with('[') { brute } else { 0 };
            assert_eq!(definition_lines(block, limit), brute, "{block:?}");
        }
    }

    #[test]
    fn headings_mode_example_parses_without_warnings() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/headings-mode");
        let config = crate::config::Config::load(&dir.join("presenter.toml")).unwrap();
        assert_eq!(config.slides.split, crate::config::SlideSplit::Headings);
        let path = dir.join("slides.md");
        let text = std::fs::read_to_string(&path).unwrap();
        let source = SourceInfo {
            path,
            kind: SourceKind::Markdown,
            base_dir: dir,
        };
        let parsed = crate::markdown::parse(&source, &text, &config).unwrap();
        assert_eq!(
            parsed.diagnostics.iter().count(),
            0,
            "{:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
        let shape: Vec<usize> = parsed
            .deck
            .slides
            .iter()
            .map(|s| s.children.len())
            .collect();
        assert!(
            shape.len() >= 4 && shape.iter().any(|&n| n > 0),
            "{shape:?}"
        );
    }

    fn footnote_names(slide: &RawSlide<'_>) -> Vec<String> {
        slide
            .nodes
            .iter()
            .filter_map(|node| match &node.data().value {
                NodeValue::FootnoteDefinition(def) => Some(def.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn footnote_definitions_stay_on_their_slide() {
        let cases: &[(&str, &str, &str, &[&[&str]])] = &[
            (
                "definitions after their references",
                "# A\n\nText[^1].\n\n[^1]: The note.\n\n---\n\n# B\n\nMore[^2].\n\n[^2]: Second.\n",
                "1-5 | 7-13",
                &[&["1"], &["2"]],
            ),
            (
                "a definition before its reference widens the span back",
                "[^1]: note\n\nText[^1]\n\n---\n\n# B\n",
                "1-3 | 5-7",
                &[&["1"], &[]],
            ),
            (
                "a definition before the first separator makes slide 1",
                "---\ntitle: x\n---\n\n[^1]: The note.\n\n---\n\n# A[^1]\n",
                "5-5 | 7-9",
                &[&["1"], &[]],
            ),
            (
                "comrak's order (by first reference) is kept on a slide",
                "# A\n\nX[^b] Y[^a]\n\n[^a]: written first\n\n[^b]: written second\n",
                "1-7",
                &[&["b", "a"]],
            ),
            (
                "a definition in a vertical slide",
                "# A\n\n--\n\nZ[^z]\n\n[^z]: zed\n\n---\n\n# B\n",
                "1-1 [3-7] | 9-11",
                &[&[], &["z"], &[]],
            ),
        ];
        for (what, text, expected_shape, expected_notes) in cases {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run(&arena, text, via);
                assert_eq!(shape(&deck), *expected_shape, "{what}");
                let slides = flatten(&deck);
                let notes: Vec<Vec<String>> = slides.iter().map(|s| footnote_names(s)).collect();
                assert_eq!(notes, *expected_notes, "{what}");
                for slide in slides {
                    // definitions come after every other node
                    let first_note = slide
                        .nodes
                        .iter()
                        .position(|n| matches!(n.data().value, NodeValue::FootnoteDefinition(_)));
                    if let Some(at) = first_note {
                        assert_eq!(footnote_names(slide).len(), slide.nodes.len() - at);
                    }
                }
            }
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
            // image alt text is skipped; whitespace runs collapse
            ("# ![logo](x.png) Title\n", Some("Title")),
            ("# ![logo](x.png)\n", None),
            // inline math is its TeX (A3)
            (
                "# Euler $e^{i\\pi}$ identity\n",
                Some("Euler e^{i\\pi} identity"),
            ),
            ("# The $E=mc^2$ law\n", Some("The E=mc^2 law")),
            ("# A <b>bold</b>  move\n", Some("A bold move")),
            // headings inside layout directives count, `:::notes` does not
            (":::center\n# Title\n:::\n", Some("Title")),
            (
                "::::columns\n:::column\n## Left\n:::\n::::\n\n# Later\n",
                Some("Left"),
            ),
            (":::notes\n# Not a title\n:::\n\n# Real\n", Some("Real")),
            ("- # In a list\n", None),
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
        for (line, looks) in [
            ("--- {x}", true),
            ("-- {x", true),
            ("---{", true),
            ("--\t{", true),
            ("---- {x}", false),
            ("- {x}", false),
            ("--", false),
            ("`-- {x}`", false),
        ] {
            assert_eq!(looks_like_attr_separator(line), looks, "{line:?}");
        }
    }

    #[test]
    fn unsmart_undoes_smart_punctuation_only_after_a_smart_dash() {
        for (text, expected) in [
            ("— {.x k=“v” j=‘w’ z=a…}", "--- {.x k=\"v\" j='w' z=a...}"),
            ("– {auto-animate}", "-- {auto-animate}"),
            ("--- {k=“v”}", "--- {k=“v”}"),
            ("plain", "plain"),
        ] {
            assert_eq!(unsmart(text), expected, "{text:?}");
        }
    }

    #[test]
    fn source_lines_match_comrak_line_numbers() {
        assert_eq!(source_lines(""), [""]);
        assert_eq!(source_lines("a\nb\n"), ["a", "b", ""]);
        assert_eq!(source_lines("a\r\nb\rc\n"), ["a", "b", "c", ""]);
        assert_eq!(source_lines("\u{feff}a\n"), ["a", ""]);
        // end to end: CRLF, lone CR and a byte-order mark
        let text = "\u{feff}# A\r\n\r\n--- {.x}\r\n\r\nb\rc\r\r-- {.y}\r\rz\r";
        for via in BOTH {
            let arena = Arena::new();
            let deck = run(&arena, text, via);
            assert_eq!(shape(&deck), "1-1 | 3-6 [8-10]", "{via:?}");
            assert_eq!(deck.slides[1].0.attrs.classes, ["x"]);
            assert_eq!(deck.slides[1].1[0].attrs.classes, ["y"]);
        }
    }

    #[test]
    fn wrong_source_text_falls_back_to_the_ast() {
        let text = "# A\n\n--- {.x}\n\n# B\n";
        let arena = Arena::new();
        let root = parse(&arena, text);
        let deck = split_with_source(root, "", SplitMode::Rules).unwrap();
        assert_eq!(shape(&deck), "1-1 | 3-5");
        assert_eq!(deck.slides[1].0.attrs.classes, ["x"]);
    }
    /// comrak keeps leading link reference definitions in a paragraph's sourcepos but not in
    /// its text: `[x]: http://a` directly followed by a separator line is still a separator,
    /// opened on its own line, with and without smart punctuation.
    #[test]
    fn separator_after_a_link_reference_definition() {
        let cases: &[(&str, &str, &str, Diags)] = &[
            (
                "`{…}` form (the issue's repro)",
                "# A\n\n[x]: http://a\n--- {.a}\n\n# B\n",
                "1-1 | 4-6",
                &[],
            ),
            (
                "two definitions",
                "# A\n\n[x]: http://a\n[y]: http://b\n-- {.a}\n\n# B\n",
                "1-1 [5-7]",
                &[],
            ),
            (
                "a definition continued over three lines",
                "# A\n\n[x]:\n  http://a\n  \"t\"\n-- {.a}\n\n# B\n",
                "1-1 [6-8]",
                &[],
            ),
            (
                "bare `--`",
                "# A\n\n[x]: http://a\n--\n\n# B\n",
                "1-1 [4-6]",
                &[],
            ),
            (
                "text after the definition: content, reported on the right line",
                "# A\n\n[x]: http://a\ntext\n--- {.a}\n",
                "1-5",
                &[("separator-not-recognized", 5)],
            ),
        ];
        for smart in [false, true] {
            for (what, text, expected_shape, expected_diags) in cases {
                for via in BOTH {
                    let arena = Arena::new();
                    let deck = run_with(&arena, text, via, smart);
                    let label = format!("{what} ({via:?}, smart = {smart})");
                    assert_eq!(shape(&deck), *expected_shape, "{label}");
                    assert_eq!(diags(&deck), *expected_diags, "{label}");
                    // The separator's attributes reach the slide it opened.
                    if let Some(opened) = flatten(&deck).get(1) {
                        let expected: &[&str] = if text.contains("{.a}") { &["a"] } else { &[] };
                        assert_eq!(opened.attrs.classes, expected, "{label}");
                    }
                }
            }
        }
        // A code span over the definition's following lines is one inline: never a
        // separator, although its last line alone would match.
        for via in BOTH {
            let arena = Arena::new();
            let deck = run(&arena, "# A\n\n[x]: http://a\n`a\n--- {`}\n", via);
            assert_eq!(shape(&deck), "1-5", "{via:?}");
        }
        // Errors point at the separator's line.
        for via in BOTH {
            let text = "# A\n\n[x]: http://a\n--- {k=\"v}\n";
            let arena = Arena::new();
            let root = parse(&arena, text);
            let result = match via {
                Via::Ast => split(root, SplitMode::Rules),
                Via::Source => split_with_source(root, text, SplitMode::Rules),
            };
            assert!(
                matches!(result, Err(Error::Parse { line: Some(4), .. })),
                "{via:?}: {result:?}"
            );
        }
    }

    #[test]
    fn text_lines_skip_leading_link_reference_definitions() {
        let text = "[x]: http://a\n[y]:\n  http://b\nfirst `code\nspan` *em\nph* $a\nb$ <b\nc=\"d\">\
                    \nlast\n\nplain\npara\n\n[z]: http://c\n[l](u \"t\nt\") text\n";
        let lines = source_lines(text);
        let arena = Arena::new();
        let nodes: Vec<_> = parse(&arena, text).children().collect();
        assert_eq!(line_range(nodes[0]), (1, 9));
        assert_eq!(line_range(nodes[2]), (14, 16));
        for source in [None, Some(&lines[..])] {
            // code spans, emphasis, math and raw HTML over several lines are all numbered
            assert_eq!(text_lines(nodes[0], source), (4, 9), "{source:?}");
            assert_eq!(text_lines(nodes[1], source), (11, 12), "{source:?}");
        }
        // A link whose title spans lines hides a line ending: only the source tells that line
        // 14 alone is a definition; from the AST the whole range is kept.
        assert_eq!(unnumbered_lines(nodes[2]), 2);
        assert_eq!(text_lines(nodes[2], Some(&lines)), (15, 16));
        assert_eq!(text_lines(nodes[2], None), (14, 16));
        // A paragraph without definitions keeps its range, also when such links hide line
        // endings: every paragraph of the fixtures (none starts with a definition), and these.
        let mut texts: Vec<String> = FIXTURE_SHAPES.iter().map(|(n, _, _)| fixture(n)).collect();
        texts.extend(
            [
                "--- {.x}\nSee [d](http://e\n\"T\").\n",
                "[l](u (x\n--- {.a))}\n",
                "[l](u \"x\n--- {\")}\n",
                "[l][x\ny] and ![i](a.png\n'alt')\n\n[x y]: http://a\n",
                "[l](u){.a\n.b} `c`{.d\n.e}\n",
            ]
            .map(String::from),
        );
        for text in &texts {
            let lines = source_lines(text);
            let arena = Arena::new();
            for node in parse(&arena, text).descendants() {
                if matches!(node.data().value, NodeValue::Paragraph) {
                    let range = line_range(node);
                    assert_eq!(text_lines(node, Some(&lines)), range, "{text:?}");
                    assert_eq!(text_lines(node, None), range, "{text:?}");
                }
            }
        }
    }

    /// Links and images whose destination, title or reference label spans lines hide line
    /// endings from comrak's inline line numbers. That is not mistaken for link reference
    /// definitions: a lookalike line next to them is still reported, on its own line, and
    /// text inside such a title never becomes a separator.
    #[test]
    fn multi_line_link_titles_are_not_definitions() {
        // (what, text, shape, diagnostics from the source, diagnostics from the AST)
        let cases: &[(&str, &str, &str, Diags, Diags)] = &[
            (
                "a lookalike, then a link whose title is on the next line",
                "# A\n\n--- {.x}\nSee [d](http://e\n\"T\").\n",
                "1-5",
                &[("separator-not-recognized", 3)],
                &[("separator-not-recognized", 3)],
            ),
            (
                "an image with its title on the next line",
                "# A\n\n-- {.x}\n![i](a.png\n'alt').\n",
                "1-5",
                &[("separator-not-recognized", 3)],
                &[("separator-not-recognized", 3)],
            ),
            (
                "two such links after the lookalike",
                "# A\n\nfirst\n--- {.x}\n[d](http://e\n\"T\") [f](http://g\n\"U\")\n",
                "1-7",
                &[("separator-not-recognized", 4)],
                &[("separator-not-recognized", 4)],
            ),
            (
                "a title spanning two lines",
                "# A\n\n--- {.a}\n[l](u \"t\nt\")\n",
                "1-5",
                &[("separator-not-recognized", 3)],
                &[("separator-not-recognized", 3)],
            ),
            (
                "a wrapped reference label",
                "# A\n\n--- {.a}\n[l][x\ny]\n\n[x y]: http://a\n",
                "1-5",
                &[("separator-not-recognized", 3)],
                &[("separator-not-recognized", 3)],
            ),
            (
                "a separator-like line inside a link title is content (the AST cannot see it)",
                "# A\n\n[l](u (x\n--- {.a))}\n\n# B\n",
                "1-6",
                &[("separator-not-recognized", 4)],
                &[],
            ),
            (
                "an unbalanced quote there is content too, not a parse error",
                "# A\n\n[l](u \"x\n--- {\")}\n\n# B\n",
                "1-6",
                &[("separator-not-recognized", 4)],
                &[],
            ),
            (
                "a definition, then a lookalike before such a link (pinned AST limitation: \
                 reported one line early)",
                "# A\n\n[x]: http://a\n--- {.a}\n[l](u \"t\nt\")\n",
                "1-6",
                &[("separator-not-recognized", 4)],
                &[("separator-not-recognized", 3)],
            ),
        ];
        for smart in [false, true] {
            for (what, text, expected_shape, from_source, from_ast) in cases {
                for (via, expected_diags) in [(Via::Source, from_source), (Via::Ast, from_ast)] {
                    let arena = Arena::new();
                    let deck = run_with(&arena, text, via, smart);
                    let label = format!("{what} ({via:?}, smart = {smart})");
                    assert_eq!(shape(&deck), *expected_shape, "{label}");
                    assert_eq!(diags(&deck), *expected_diags, "{label}");
                }
            }
        }
    }

    /// The AST-only reading undoes smart punctuation only for a document parsed with it:
    /// without it, typed dashes are content; with it, dashes inside values are restored.
    #[test]
    fn ast_reading_follows_the_smart_flag() {
        // smart = false: typed Unicode dashes are text, as in the source reading.
        for text in [
            "# A\n\n— {author}\n\n# B\n",
            "# A\n\n– {.x}\n\n# B\n",
            "# A\n\n–\n\n# B\n",
            "# A\n\nText\n— {.x}\n",
        ] {
            for via in BOTH {
                let arena = Arena::new();
                let deck = run_with(&arena, text, via, false);
                assert_eq!(flatten(&deck).len(), 1, "{text:?} {via:?}");
                assert!(deck.diagnostics.is_empty(), "{text:?} {via:?}");
            }
        }
        // smart = true: values with `--`/`---` come back unchanged.
        let text = "# A\n\n--- {background=bg--v2.png data-x=a---b data-y=c-----d}\n\n# B\n\n\
                    -- {data-z=e--f}\n\n# C\n";
        for via in BOTH {
            let arena = Arena::new();
            let deck = run_with(&arena, text, via, true);
            assert_eq!(shape(&deck), "1-1 | 3-5 [7-9]", "{via:?}");
            assert_eq!(
                deck.slides[1].0.attrs.attrs,
                BTreeMap::from([
                    (
                        "data-background-image".to_string(),
                        "bg--v2.png".to_string()
                    ),
                    ("data-x".to_string(), "a---b".to_string()),
                    ("data-y".to_string(), "c-----d".to_string()),
                ]),
                "{via:?}"
            );
            assert_eq!(deck.slides[1].1[0].attrs.attrs["data-z"], "e--f", "{via:?}");
        }
        // smart = true: dashes that are not comrak's (escaped here) start no separator.
        for via in BOTH {
            let arena = Arena::new();
            let deck = run_with(&arena, "# A\n\n-\\-- {.x}\n\n# B\n", via, true);
            assert_eq!(shape(&deck), "1-5", "{via:?}");
        }
        // Pinned limitation: with smart = true a dash typed as `—` reads as `---` from the AST;
        // only the source reading keeps it content.
        let text = "# A\n\n— {.typed}\n";
        let arena = Arena::new();
        assert_eq!(shape(&run_with(&arena, text, Via::Ast, true)), "1-1 | 3-3");
        assert_eq!(shape(&run_with(&arena, text, Via::Source, true)), "1-3");
        // `split` is `split_ast(…, false)`: on a smart document it needs the flag.
        let text = "# A\n\n-- {.x}\n\n# B\n";
        let arena = Arena::new();
        let root = parse_document(&arena, text, &dialect_options_with(true));
        let smart = split_ast(root, SplitMode::Rules, true).unwrap();
        assert_eq!(shape(&smart), "1-1 [3-5]");
        assert_eq!(smart.slides[0].1[0].attrs.classes, ["x"]);
        let plain = split(root, SplitMode::Rules).unwrap();
        let unsmart = split_ast(root, SplitMode::Rules, false).unwrap();
        assert_eq!(summary(&plain), summary(&unsmart));
        assert_eq!(shape(&plain), "1-5");
    }

    #[test]
    fn unsmart_maps_every_dash_back_and_typed_text_needs_a_smart_dash() {
        for (text, expected) in [
            ("— {background=bg–v2.png}", "--- {background=bg--v2.png}"),
            ("— {data-x=a—b}", "--- {data-x=a---b}"),
            // 8 hyphens: comrak writes two em dashes and an en dash
            ("– {x=a——–b}", "-- {x=a--------b}"),
        ] {
            assert_eq!(unsmart(text), expected, "{text:?}");
        }
        for (text, smart, expected) in [
            ("— {author}", false, Some("— {author}")),
            ("--- {x}", false, Some("--- {x}")),
            ("— {k=“v”}", true, Some("--- {k=\"v\"}")),
            ("--- {x}", true, None),
            ("plain", true, None),
        ] {
            assert_eq!(
                typed_text(text, smart).as_deref(),
                expected,
                "{text:?} {smart}"
            );
        }
    }
}
