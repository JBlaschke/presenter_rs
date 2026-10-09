//! Attribute blocks comrak rejects (dialect §3.1, A3 contract note). Implemented by A3.
//!
//! comrak 0.56 reads the `{…}` after a heading, a fence's language, a link, an image or inline
//! code only when every value is double-quoted or made of letters, digits and `- _ . : %`.
//! Anything else (a single-quoted value, `background=#222`, `background=figures/bg.png`,
//! `lines=1|3`) makes it keep the whole block as text: the literal `{…}` shows on the slide
//! (a URL in it even becomes a broken autolink) and no attribute applies. Dialect §3.1
//! allows such values, as the separator form `--- {…}` does, so the frontend reads these
//! blocks again with [`read_element_attrs`] (the tokenizer of
//! [`parse_attr_block`](crate::markdown::attrs::parse_attr_block); every item must be
//! `#id`, `.class` or `key=value`):
//!
//! - **headings, links, images and inline code** ([`recover_attr_blocks`]): the block is
//!   found in the source text, at the end of a heading's content (after whitespace, inside
//!   the heading's own text) or right after a link's or image's `)`/`]` or inline code's
//!   closing backtick. Each block that reads as attributes is removed from the text, the
//!   text is parsed again and the attributes are put on the node, as if comrak had read them
//!   (lines stay where they were, so spans and slide boundaries do not change). Only the
//!   columns after a removed block on its line move;
//! - **fenced code** ([`fence_attrs`], called by the renderer): the block is the end of the
//!   info string comrak left (`python {lines=1|3}`), so no source text is needed.
//!
//! A block that is not attributes stays text. When it was meant as attributes
//! ([`ElementAttrs::Invalid`]: a key without a value, an unbalanced quote, an invalid key
//! name next to valid items) it is reported as an [`UNPARSED_ATTR_BLOCK`] warning.

use std::borrow::Cow;
use std::ops::Range;

use comrak::nodes::{AstNode, Attributes, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::error::{Diagnostic, Diagnostics};
use crate::ir::SourceSpan;
use crate::markdown::attrs::{ElementAttrs, ParsedAttrs, read_element_attrs};

/// Diagnostic code of an attribute block that is left as text although it was meant as
/// attributes (warning).
pub const UNPARSED_ATTR_BLOCK: &str = "unparsed-attr-block";

/// The nodes whose attribute blocks are recovered from the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Heading,
    Link,
    Image,
    Code,
}

impl Kind {
    fn of(value: &NodeValue) -> Option<Kind> {
        Some(match value {
            NodeValue::Heading(_) => Kind::Heading,
            NodeValue::Link(_) => Kind::Link,
            NodeValue::Image(_) => Kind::Image,
            NodeValue::Code(_) => Kind::Code,
            _ => return None,
        })
    }

    /// The node as named in a warning.
    fn name(self) -> &'static str {
        match self {
            Kind::Heading => "heading",
            Kind::Link => "link",
            Kind::Image => "image",
            Kind::Code => "inline code",
        }
    }
}

/// An attribute block found in the source text after a node.
struct Found<'s> {
    /// 1-based line of the block.
    line: usize,
    /// Byte range in that line to remove when the block is read as attributes (for a
    /// heading, the whitespace before it too).
    remove: Range<usize>,
    /// The block as written.
    block: &'s str,
    /// How it reads.
    reading: ElementAttrs,
}

/// A block to remove and the node to give its attributes to.
struct Fix {
    kind: Kind,
    /// The node's first line and column (byte, 1-based), in the text the block is removed from.
    start: (usize, usize),
    /// Byte range of the block in the whole text.
    range: Range<usize>,
    attrs: ParsedAttrs,
}

/// Recovers the attribute blocks comrak left as text after headings, links, images and
/// inline code of `root`, the document comrak parsed from `text` with `opts` (module docs).
///
/// Returns `root` and `text` unchanged when there is nothing to recover. Otherwise the
/// recovered blocks are removed from `text`, the result is parsed again (into `arena`, with
/// `opts`) and the attributes are set on the new tree's nodes (merged after any that comrak
/// then reads itself); the new root and text are returned, and the caller must use both
/// from then on (the splitter reads separators from the text). Blocks that were meant as
/// attributes but are not valid are reported in `diagnostics` (`unparsed-attr-block`, span
/// on the block's line, no path).
pub fn recover_attr_blocks<'a, 't>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    text: &'t str,
    opts: &Options,
    diagnostics: &mut Diagnostics,
) -> (&'a AstNode<'a>, Cow<'t, str>) {
    let (starts, lines) = split_lines(text);
    let mut fixes = Vec::new();
    for node in root.descendants() {
        let (kind, start) = {
            let data = node.data();
            match Kind::of(&data.value) {
                Some(kind) if data.attrs.is_none() => (
                    kind,
                    (data.sourcepos.start.line, data.sourcepos.start.column),
                ),
                _ => continue,
            }
        };
        let found = match kind {
            Kind::Heading => heading_block(node, &lines),
            _ => inline_block(node, kind, &lines),
        };
        let Some(found) = found else {
            continue;
        };
        match found.reading {
            ElementAttrs::Attrs(attrs) => {
                let Some(&line_start) = starts.get(found.line - 1) else {
                    continue;
                };
                let range = line_start + found.remove.start..line_start + found.remove.end;
                fixes.push(Fix {
                    kind,
                    start,
                    range,
                    attrs,
                });
            }
            ElementAttrs::Invalid(reason) => {
                let span = SourceSpan {
                    start_line: found.line,
                    end_line: found.line,
                    cell: None,
                };
                diagnostics.push(unparsed(kind.name(), found.block, &reason).with_span(span));
            }
            ElementAttrs::Text => {}
        }
    }
    if fixes.is_empty() {
        return (root, Cow::Borrowed(text));
    }
    let (edited, fixes) = remove_blocks(text, fixes, &starts);
    let new_root = parse_document(arena, &edited, opts);
    apply_fixes(new_root, fixes);
    (new_root, Cow::Owned(edited))
}

/// The attribute block at the end of a heading's content: the last `{` in one of the
/// heading's own `Text` children on its last content line, preceded by whitespace (or
/// starting the content), whose block ends where the content does. The rightmost candidate
/// that reads as attributes wins (so `}` inside a quoted value is fine); without one, the
/// first that reads as [`ElementAttrs::Invalid`] is returned for the warning.
fn heading_block<'a, 's>(heading: &'a AstNode<'a>, lines: &[&'s str]) -> Option<Found<'s>> {
    let end = heading.last_child()?.data().sourcepos.end;
    let line = *lines.get(end.line.checked_sub(1)?)?;
    let close = end.column.checked_sub(1)?;
    if line.as_bytes().get(close) != Some(&b'}') {
        return None;
    }
    let content_start = heading
        .first_child()
        .map(|first| first.data().sourcepos.start)
        .filter(|start| start.line == end.line)
        .map(|start| start.column - 1);
    let texts: Vec<Range<usize>> = heading
        .children()
        .filter_map(|child| {
            let data = child.data();
            let pos = data.sourcepos;
            (matches!(data.value, NodeValue::Text(_))
                && pos.start.line == end.line
                && pos.end.line == end.line)
                .then(|| pos.start.column - 1..pos.end.column)
        })
        .collect();
    let content = &line[..=close];
    let mut invalid = None;
    for (at, _) in content.match_indices('{').rev() {
        let separated = Some(at) == content_start
            || at
                .checked_sub(1)
                .is_some_and(|before| matches!(line.as_bytes()[before], b' ' | b'\t'));
        if !separated || !texts.iter().any(|text| text.contains(&at)) {
            continue;
        }
        let block = &content[at..];
        match read_element_attrs(block) {
            ElementAttrs::Attrs(attrs) => {
                let from = line[..at].trim_end_matches([' ', '\t']).len();
                return Some(Found {
                    line: end.line,
                    remove: from..close + 1,
                    block,
                    reading: ElementAttrs::Attrs(attrs),
                });
            }
            ElementAttrs::Invalid(reason) if invalid.is_none() => {
                invalid = Some(Found {
                    line: end.line,
                    remove: at..close + 1,
                    block,
                    reading: ElementAttrs::Invalid(reason),
                });
            }
            _ => {}
        }
    }
    invalid
}

/// The attribute block right after a link's or image's `)`/`]` or inline code's closing
/// backtick, on the same line: a `{` that starts the next `Text` sibling, up to the first
/// `}` outside quotes. Without such a `}` (an unbalanced quote) the block runs to the first
/// `}` and is returned only when it reads as [`ElementAttrs::Invalid`].
fn inline_block<'a, 's>(node: &'a AstNode<'a>, kind: Kind, lines: &[&'s str]) -> Option<Found<'s>> {
    let end = node.data().sourcepos.end;
    let line = *lines.get(end.line.checked_sub(1)?)?;
    let bytes = line.as_bytes();
    let at = end.column;
    let closer = *bytes.get(at.checked_sub(1)?)?;
    let closes = match kind {
        Kind::Code => closer == b'`',
        _ => matches!(closer, b')' | b']'),
    };
    if !closes || bytes.get(at) != Some(&b'{') {
        return None;
    }
    let next = node.next_sibling()?;
    {
        let data = next.data();
        let start = data.sourcepos.start;
        if !matches!(data.value, NodeValue::Text(_))
            || (start.line, start.column) != (end.line, at + 1)
        {
            return None;
        }
    }
    let rest = &line[at..];
    let (len, reading) = match block_len(rest) {
        Some(len) => (len, read_element_attrs(&rest[..len])),
        None => {
            let len = rest.find('}')? + 1;
            match read_element_attrs(&rest[..len]) {
                invalid @ ElementAttrs::Invalid(_) => (len, invalid),
                _ => return None,
            }
        }
    };
    Some(Found {
        line: end.line,
        remove: at..at + len,
        block: &rest[..len],
        reading,
    })
}

/// The byte length of the `{…}` block that `text` starts with: up to the first `}` outside
/// single or double quotes (a backslash inside quotes escapes the next character). `None`
/// when the text ends first or a `{` comes first outside quotes.
fn block_len(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut chars = text.char_indices().skip(1);
    while let Some((at, c)) = chars.next() {
        match (quote, c) {
            (Some(_), '\\') => {
                chars.next();
            }
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '}') => return Some(at + 1),
            (None, '{') => return None,
            (None, _) => {}
        }
    }
    None
}

/// The text with the fixes' ranges removed (a fix overlapping an earlier one is dropped),
/// and the kept fixes with their node's start column moved left by what was removed before
/// it on its line.
fn remove_blocks(text: &str, mut fixes: Vec<Fix>, starts: &[usize]) -> (String, Vec<Fix>) {
    fixes.sort_by_key(|fix| fix.range.start);
    let mut edited = String::with_capacity(text.len());
    let mut kept: Vec<Fix> = Vec::with_capacity(fixes.len());
    let mut copied = 0;
    for fix in fixes {
        if fix.range.start < copied {
            tracing::debug!(
                "overlapping attribute blocks at {:?}; one is kept",
                fix.range
            );
            continue;
        }
        edited.push_str(&text[copied..fix.range.start]);
        copied = fix.range.end;
        kept.push(fix);
    }
    edited.push_str(&text[copied..]);
    let removed: Vec<Range<usize>> = kept.iter().map(|fix| fix.range.clone()).collect();
    for fix in &mut kept {
        let (line, column) = fix.start;
        let Some(&line_start) = starts.get(line - 1) else {
            continue;
        };
        let offset = line_start + column - 1;
        let shift: usize = removed
            .iter()
            .filter(|range| range.start >= line_start && range.end <= offset)
            .map(|range| range.len())
            .sum();
        fix.start = (line, column - shift);
    }
    (edited, kept)
}

/// Sets each fix's attributes on the node of its kind that starts where the fix says.
fn apply_fixes<'a>(root: &'a AstNode<'a>, mut fixes: Vec<Fix>) {
    for node in root.descendants() {
        let mut data = node.data_mut();
        let Some(kind) = Kind::of(&data.value) else {
            continue;
        };
        let start = (data.sourcepos.start.line, data.sourcepos.start.column);
        let Some(at) = fixes
            .iter()
            .position(|fix| fix.kind == kind && fix.start == start)
        else {
            continue;
        };
        let fix = fixes.swap_remove(at);
        let attrs = data.attrs.get_or_insert_with(Default::default);
        merge_into(attrs, fix.attrs);
    }
    for fix in fixes {
        tracing::debug!(
            "attribute block for the {} at {:?} not applied: node not found",
            fix.kind.name(),
            fix.start
        );
    }
}

/// Appends `later` to comrak's attributes (its id wins when set).
fn merge_into(attrs: &mut Attributes, later: ParsedAttrs) {
    if later.id.is_some() {
        attrs.id = later.id;
    }
    attrs.classes.extend(later.classes);
    attrs.pairs.extend(later.pairs);
}

/// The lines of `text` as comrak numbers lines and columns: `\n`, `\r\n` and a lone `\r`
/// end a line, and a leading byte-order mark stays in line 1, since comrak counts its bytes
/// in that line's columns (unlike [`source_lines`](crate::markdown::split::source_lines),
/// which drops it). Returns each line's start offset in `text` and its text.
fn split_lines(text: &str) -> (Vec<usize>, Vec<&str>) {
    let starts = line_starts(text);
    let lines = starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).map_or(text.len(), |&next| next);
            text[start..end].trim_end_matches(['\n', '\r'])
        })
        .collect();
    (starts, lines)
}

/// The byte offset in `text` where each line starts (see [`split_lines`]).
fn line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\n' => starts.push(at + 1),
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => {
                at += 1;
                starts.push(at + 1);
            }
            b'\r' => starts.push(at + 1),
            _ => {}
        }
        at += 1;
    }
    starts
}

/// The [`UNPARSED_ATTR_BLOCK`] warning (no span) for `block` after a `what`.
pub(crate) fn unparsed(what: &str, block: &str, reason: &str) -> Diagnostic {
    let message = format!(
        "`{block}` after this {what} is not read as attributes and stays in the text: {reason}"
    );
    Diagnostic::warning(UNPARSED_ATTR_BLOCK, message)
}

/// The attribute block at the end of a fenced code block's info string, which comrak leaves
/// there when it rejects it (module docs): the info without the block (trimmed) and the
/// block read with [`read_element_attrs`]. The block is the last `{` that starts the info or
/// follows whitespace and reads as attributes, up to the end; without one, the first that
/// reads as [`ElementAttrs::Invalid`]. `None` when the info does not end with `}` or no
/// candidate is attributes or invalid (R Markdown's `{r}` stays the language). Call it only
/// for a fence without comrak attributes.
pub fn fence_attrs(info: &str) -> Option<(&str, &str, ElementAttrs)> {
    let info = info.trim_end();
    if !info.ends_with('}') {
        return None;
    }
    let mut invalid = None;
    for (at, _) in info.match_indices('{').rev() {
        let separated = info[..at].is_empty() || info[..at].ends_with(char::is_whitespace);
        if !separated {
            continue;
        }
        let block = &info[at..];
        match read_element_attrs(block) {
            attrs @ ElementAttrs::Attrs(_) => return Some((info[..at].trim_end(), block, attrs)),
            invalid_block @ ElementAttrs::Invalid(_) if invalid.is_none() => {
                invalid = Some((info, block, invalid_block));
            }
            _ => {}
        }
    }
    invalid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::split::dialect_options;

    /// What [`recover`] returns: every node with attributes as `(kind, attrs)`, the edited
    /// text and the diagnostics as `(code, line)`.
    type Recovered = (Vec<(Kind, Attributes)>, String, Vec<(&'static str, usize)>);

    /// Parses and recovers `text` (see [`Recovered`]).
    fn recover(text: &str) -> Recovered {
        let arena = Arena::new();
        let opts = dialect_options();
        let root = parse_document(&arena, text, &opts);
        let mut diagnostics = Diagnostics::new();
        let (root, edited) = recover_attr_blocks(&arena, root, text, &opts, &mut diagnostics);
        let found = root
            .descendants()
            .filter_map(|node| {
                let data = node.data();
                let kind = Kind::of(&data.value)?;
                Some((kind, data.attrs.as_deref()?.clone()))
            })
            .collect();
        let codes = diagnostics
            .iter()
            .map(|d| (d.code, d.span.as_ref().map_or(0, |s| s.start_line)))
            .collect();
        (found, edited.into_owned(), codes)
    }

    fn attrs(id: Option<&str>, classes: &[&str], pairs: &[(&str, &str)]) -> Attributes {
        Attributes {
            id: id.map(str::to_string),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            pairs: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn heading_blocks_comrak_rejects_are_read() {
        let cases: &[(&str, &str, Attributes)] = &[
            (
                "# Title {background=figures/bg.png}\n",
                "# Title\n",
                attrs(None, &[], &[("background", "figures/bg.png")]),
            ),
            (
                "# T3 {.dark background=#222}\n",
                "# T3\n",
                attrs(None, &["dark"], &[("background", "#222")]),
            ),
            // autolinked by comrak: the block is found in the source, not in the inlines
            (
                "# W {background=https://ex.com/a.png}\n",
                "# W\n",
                attrs(None, &[], &[("background", "https://ex.com/a.png")]),
            ),
            // single quotes, a closing sequence, a block quote, a setext heading
            (
                "# Q {#q .x data-k='a b'} ##\n",
                "# Q ##\n",
                attrs(Some("q"), &["x"], &[("data-k", "a b")]),
            ),
            (
                "> # Q é {title='it\\'s'}\n",
                "> # Q é\n",
                attrs(None, &[], &[("title", "it's")]),
            ),
            (
                "Setext {k='v'}\n===\n",
                "Setext\n===\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            // `}` inside a quoted value, a `{` in the value
            (
                "# B {data-x='a {b}'}\n",
                "# B\n",
                attrs(None, &[], &[("data-x", "a {b}")]),
            ),
            // the block alone
            ("# {k='v'}\n", "#\n", attrs(None, &[], &[("k", "v")])),
            // CRLF line breaks and a byte-order mark
            (
                "# T {k='v'}\r\n\r\ntext\r\n",
                "# T\r\n\r\ntext\r\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            (
                "\u{feff}# T {k='v'}\n",
                "\u{feff}# T\n",
                attrs(None, &[], &[("k", "v")]),
            ),
        ];
        for (text, edited, expected) in cases {
            let (found, after, codes) = recover(text);
            assert_eq!(found, [(Kind::Heading, expected.clone())], "{text}");
            assert_eq!(after, *edited, "{text}");
            assert!(codes.is_empty(), "{text}: {codes:?}");
        }
    }

    #[test]
    fn inline_blocks_comrak_rejects_are_read() {
        let text = "- ![a](img.png){width='50%'} and `c`{data-x='y z'} [l](https://x.org){data-x=a/b}.\n\n\
                    | ![t](t.png){width='5'} | [r][ref]{k='v'} |\n|---|---|\n\n[ref]: https://r.org\n";
        let (found, edited, codes) = recover(text);
        assert_eq!(
            found,
            [
                (Kind::Image, attrs(None, &[], &[("width", "50%")])),
                (Kind::Code, attrs(None, &[], &[("data-x", "y z")])),
                (Kind::Link, attrs(None, &[], &[("data-x", "a/b")])),
                (Kind::Image, attrs(None, &[], &[("width", "5")])),
                (Kind::Link, attrs(None, &[], &[("k", "v")])),
            ]
        );
        assert!(edited.starts_with(
            "- ![a](img.png) and `c` [l](https://x.org).\n\n| ![t](t.png) | [r][ref] |\n"
        ));
        assert!(codes.is_empty(), "{codes:?}");
    }

    #[test]
    fn comrak_attributes_are_kept_and_merged() {
        // comrak reads the earlier block once the rejected one is removed
        let (found, edited, _) = recover("![a](x.png){width='5'}{.b}\n\n# H {.a} {k='v'}\n");
        assert_eq!(edited, "![a](x.png){.b}\n\n# H {.a}\n");
        assert_eq!(
            found,
            [
                (Kind::Image, attrs(None, &["b"], &[("width", "5")])),
                (Kind::Heading, attrs(None, &["a"], &[("k", "v")])),
            ]
        );
        // what comrak reads itself is not touched
        let text = "# H {.a k=v}\n\n![a](x.png){width=50%}\n";
        let (found, edited, codes) = recover(text);
        assert_eq!(edited, text);
        assert_eq!(found.len(), 2);
        assert!(codes.is_empty());
    }

    #[test]
    fn text_in_braces_stays_text() {
        for text in [
            "# The set {1, 2, 3}\n",
            "# Use {curly braces}\n",
            "# Title{k='v'}\n",
            "# `code {k='v'}`\n",
            "# [Title {k='v'}](u)\n",
            "![a](x.png) {k='v'}\n",
            "Text {k='v'} here\n",
            "![a](x.png){it's}\n",
            "<https://a.org>{k='v'}\n",
        ] {
            let (found, edited, codes) = recover(text);
            assert!(found.is_empty(), "{text}: {found:?}");
            assert_eq!(edited, text);
            assert!(codes.is_empty(), "{text}: {codes:?}");
        }
    }

    #[test]
    fn blocks_meant_as_attributes_are_reported() {
        let text = "# A {.dark auto-animate}\n\n# B {auto-animate}\n\n![i](x.png){width='50%}\n\n`c`{k='v' 1x=2}\n";
        let (found, edited, codes) = recover(text);
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(edited, text);
        assert_eq!(
            codes,
            [
                (UNPARSED_ATTR_BLOCK, 1),
                (UNPARSED_ATTR_BLOCK, 3),
                (UNPARSED_ATTR_BLOCK, 5),
                (UNPARSED_ATTR_BLOCK, 7),
            ]
        );
        let arena = Arena::new();
        let opts = dialect_options();
        let root = parse_document(&arena, "# A {.dark auto-animate}\n", &opts);
        let mut diagnostics = Diagnostics::new();
        recover_attr_blocks(
            &arena,
            root,
            "# A {.dark auto-animate}\n",
            &opts,
            &mut diagnostics,
        );
        let message = &diagnostics.iter().next().unwrap().message;
        assert_eq!(
            message,
            "`{.dark auto-animate}` after this heading is not read as attributes and stays in \
             the text: `auto-animate` has no value: write `auto-animate=\"\"` (a key without a \
             value works only in `--- {…}` and `<!-- .slide: -->`)"
        );
    }

    #[test]
    fn several_blocks_on_one_line_keep_their_nodes() {
        let text = "![a](a.png){w='1'} ![b](b.png){w='2'} `c`{k='3'}\n";
        let (found, edited, _) = recover(text);
        assert_eq!(edited, "![a](a.png) ![b](b.png) `c`\n");
        let values: Vec<_> = found
            .iter()
            .map(|(kind, attrs)| (*kind, attrs.pairs[0].1.as_str()))
            .collect();
        assert_eq!(
            values,
            [(Kind::Image, "1"), (Kind::Image, "2"), (Kind::Code, "3")]
        );
    }

    #[test]
    fn lines_are_numbered_like_source_lines() {
        use crate::markdown::split::source_lines;
        for text in ["a\nb\r\nc\rd", "\u{feff}x\n\ny\r", "", "\n", "\r\n\r\n"] {
            let (starts, lines) = split_lines(text);
            let expected = source_lines(text);
            assert_eq!(lines.len(), expected.len(), "{text:?}");
            assert_eq!(starts.len(), lines.len(), "{text:?}");
            for ((start, line), expected) in starts.iter().zip(&lines).zip(&expected) {
                assert!(text[*start..].starts_with(line), "{text:?}");
                // only the byte-order mark differs
                assert_eq!(line.trim_start_matches('\u{feff}'), *expected, "{text:?}");
            }
        }
    }

    #[test]
    fn fence_info_blocks() {
        let read =
            |info: &str| fence_attrs(info).map(|(rest, _, reading)| (rest.to_string(), reading));
        let pairs = |pairs: &[(&str, &str)], classes: &[&str]| {
            ElementAttrs::Attrs(ParsedAttrs {
                id: None,
                classes: classes.iter().map(|c| c.to_string()).collect(),
                pairs: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            })
        };
        assert_eq!(
            read("python {lines=1|3}"),
            Some(("python".into(), pairs(&[("lines", "1|3")], &[])))
        );
        assert_eq!(
            read("{.numbered lines=1,3}"),
            Some((String::new(), pairs(&[("lines", "1,3")], &["numbered"])))
        );
        assert_eq!(
            read("js {data-x='a b'}  "),
            Some(("js".into(), pairs(&[("data-x", "a b")], &[])))
        );
        // R Markdown / Quarto chunk headers and plain info strings stay as they are
        for info in ["{r}", "{python}", "python", "python{lines=1|3}", ""] {
            assert_eq!(read(info), None, "{info}");
        }
        assert!(matches!(
            read("python {numbered lines=1|3}"),
            Some((rest, ElementAttrs::Invalid(_))) if rest == "python {numbered lines=1|3}"
        ));
    }
}
