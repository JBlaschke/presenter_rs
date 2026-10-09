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
//!   closing backtick. Each block that reads as attributes is rewritten in the text in the
//!   form comrak reads itself, every value double-quoted (`{background='#222' .dark}` →
//!   `{."dark" background="#222"}`), and the text is parsed again, so comrak sets the
//!   attributes exactly as for that form: the result is what the double-quoted block gives
//!   (also next to other inlines and other blocks: `# H {.a} {k='v'}` keeps `{.a}` as text,
//!   as `# H {.a} {k="v"}` does). Lines stay where they were, so spans and slide boundaries
//!   do not change; only the columns after a rewritten block on its line move. A block
//!   comrak still does not read in its place is put back and reported;
//! - **fenced code** ([`fence_attrs`], called by the renderer): the block is the end of the
//!   info string comrak left (`python {lines=1|3}`), so no source text is needed.
//!
//! A block that is not attributes stays text. When it was meant as attributes
//! ([`ElementAttrs::Invalid`]: a key without a value, an unbalanced quote, an invalid key
//! name next to valid items, items separated by commas) it is reported as an
//! [`UNPARSED_ATTR_BLOCK`] warning.

use std::borrow::Cow;
use std::ops::Range;

use comrak::nodes::{AstNode, Attributes, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::error::{Diagnostic, Diagnostics};
use crate::ir::SourceSpan;
use crate::markdown::attrs::{ElementAttrs, read_element_attrs};

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
    /// Byte range of the block in that line.
    range: Range<usize>,
    /// The block as written.
    block: &'s str,
    /// How it reads.
    reading: ElementAttrs,
}

/// A block to rewrite in comrak's own form, and the node that must then have its attributes.
struct Fix<'t> {
    kind: Kind,
    /// The node's first line and column (byte, 1-based).
    start: (usize, usize),
    /// Byte range of the block in the whole text.
    range: Range<usize>,
    /// The block as written and its line, for the warning when it cannot be applied.
    block: &'t str,
    line: usize,
    /// The attributes it reads as, and the block comrak reads as them ([`native_block`]).
    attrs: Attributes,
    native: String,
}

/// Where a fix's node must be in the rewritten text and the attributes it must have.
struct Target<'f> {
    kind: Kind,
    start: (usize, usize),
    attrs: &'f Attributes,
}

/// How many times [`recover_attr_blocks`] parses a rewritten text before it gives up.
const MAX_ROUNDS: usize = 3;

/// Why a block is not recovered when comrak does not read its rewritten form either.
const NOT_READ_THERE: &str = "comrak does not read it in this place, even with every value \
     in double quotes";

/// Recovers the attribute blocks comrak left as text after headings, links, images and
/// inline code of `root`, the document comrak parsed from `text` with `opts` (module docs).
///
/// Returns `root` and `text` unchanged when there is nothing to recover. Otherwise each
/// block that reads as attributes is rewritten in comrak's own syntax, every value
/// double-quoted (`{."dark" background="#222"}`), the result is parsed again (into `arena`,
/// with `opts`) and the new root and text are returned; the caller must use both from then
/// on (the splitter reads separators from the text). Each node must then carry exactly the
/// block's attributes; a block for which that is not so is put back as written and
/// reported, and the text is parsed again without it (at most three times, then
/// nothing is recovered). Blocks that were meant as attributes but are not read are reported
/// in `diagnostics` (`unparsed-attr-block`, span on the block's line, no path).
pub fn recover_attr_blocks<'a, 't>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    text: &'t str,
    opts: &Options,
    diagnostics: &mut Diagnostics,
) -> (&'a AstNode<'a>, Cow<'t, str>) {
    let (starts, lines) = split_lines(text);
    let mut fixes = find_fixes(root, &starts, &lines, diagnostics);
    for _ in 0..MAX_ROUNDS {
        if fixes.is_empty() {
            return (root, Cow::Borrowed(text));
        }
        let (edited, targets) = rewrite_blocks(text, &fixes, &starts);
        let new_root = parse_document(arena, &edited, opts);
        let read = read_by_comrak(new_root, &targets);
        if read.iter().all(|&read| read) {
            return (new_root, Cow::Owned(edited));
        }
        let (kept, missed): (Vec<_>, Vec<_>) =
            fixes.into_iter().zip(read).partition(|&(_, read)| read);
        report_missed(missed.into_iter().map(|(fix, _)| fix), diagnostics);
        fixes = kept.into_iter().map(|(fix, _)| fix).collect();
    }
    report_missed(fixes, diagnostics);
    (root, Cow::Borrowed(text))
}

/// The blocks after `root`'s headings, links, images and inline code that read as
/// attributes, without overlaps, ordered by position; blocks meant as attributes that are
/// not valid are reported in `diagnostics`.
fn find_fixes<'a, 't>(
    root: &'a AstNode<'a>,
    starts: &[usize],
    lines: &[&'t str],
    diagnostics: &mut Diagnostics,
) -> Vec<Fix<'t>> {
    let mut fixes: Vec<Fix<'t>> = Vec::new();
    for node in root.descendants() {
        let (kind, start) = {
            let data = node.data();
            let pos = data.sourcepos.start;
            match Kind::of(&data.value) {
                Some(kind) if data.attrs.is_none() => (kind, (pos.line, pos.column)),
                _ => continue,
            }
        };
        let found = match kind {
            Kind::Heading => heading_block(node, lines),
            _ => inline_block(node, kind, lines),
        };
        let Some(found) = found else {
            continue;
        };
        match found.reading {
            ElementAttrs::Attrs(parsed) => {
                let Some(&line_start) = starts.get(found.line - 1) else {
                    continue;
                };
                let attrs = Attributes {
                    id: parsed.id,
                    classes: parsed.classes,
                    pairs: parsed.pairs,
                };
                fixes.push(Fix {
                    kind,
                    start,
                    range: line_start + found.range.start..line_start + found.range.end,
                    block: found.block,
                    line: found.line,
                    native: native_block(&attrs),
                    attrs,
                });
            }
            ElementAttrs::Invalid(reason) => {
                diagnostics.push(unparsed_at(kind, found.block, &reason, found.line));
            }
            ElementAttrs::Text => {}
        }
    }
    fixes.sort_by_key(|fix| fix.range.start);
    let mut kept: Vec<Fix<'t>> = Vec::with_capacity(fixes.len());
    for fix in fixes {
        if kept
            .last()
            .is_some_and(|last| fix.range.start < last.range.end)
        {
            tracing::debug!(
                "overlapping attribute blocks at {:?}; one is kept",
                fix.range
            );
            continue;
        }
        kept.push(fix);
    }
    kept
}

/// `attrs` as an attribute block comrak reads itself: `#id`, `.class` and `key=value` items
/// separated by spaces, every value double-quoted with `\` and `"` escaped (comrak takes any
/// character there; keys are valid attribute names, which is comrak's key grammar).
fn native_block(attrs: &Attributes) -> String {
    let mut out = String::from("{");
    let mut item = |prefix: &str, value: &str| {
        if out.len() > 1 {
            out.push(' ');
        }
        out.push_str(prefix);
        out.push('"');
        for c in value.chars() {
            if matches!(c, '\\' | '"') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
    };
    if let Some(id) = &attrs.id {
        item("#", id);
    }
    for class in &attrs.classes {
        item(".", class);
    }
    for (key, value) in &attrs.pairs {
        item(&format!("{key}="), value);
    }
    out.push('}');
    out
}

/// The `unparsed-attr-block` warning for `block` after a node of `kind`, on `line`.
fn unparsed_at(kind: Kind, block: &str, reason: &str, line: usize) -> Diagnostic {
    unparsed(kind.name(), block, reason).with_span(SourceSpan {
        start_line: line,
        end_line: line,
        cell: None,
        file: None,
    })
}

/// Reports fixes whose rewritten block comrak did not read as their attributes.
fn report_missed<'t>(fixes: impl IntoIterator<Item = Fix<'t>>, diagnostics: &mut Diagnostics) {
    for fix in fixes {
        tracing::debug!(
            "attribute block for the {} at {:?} not recovered: comrak does not read {}",
            fix.kind.name(),
            fix.start,
            fix.native
        );
        diagnostics.push(unparsed_at(fix.kind, fix.block, NOT_READ_THERE, fix.line));
    }
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
        // as comrak: the block starts the content or follows whitespace (the first child of
        // `# \{…}` starts at the escaped `{`, which is not the content's start)
        let before = at.checked_sub(1).map(|before| line.as_bytes()[before]);
        let separated = matches!(before, None | Some(b' ' | b'\t'))
            || (Some(at) == content_start && before != Some(b'\\'));
        if !separated || !texts.iter().any(|text| text.contains(&at)) {
            continue;
        }
        let block = &content[at..];
        match read_element_attrs(block) {
            ElementAttrs::Attrs(attrs) => {
                return Some(Found {
                    line: end.line,
                    range: at..close + 1,
                    block,
                    reading: ElementAttrs::Attrs(attrs),
                });
            }
            ElementAttrs::Invalid(reason) if invalid.is_none() => {
                invalid = Some(Found {
                    line: end.line,
                    range: at..close + 1,
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
/// `}` and is returned only when it reads as [`ElementAttrs::Invalid`]. A block that runs
/// past the end of the node's parent (a table cell ended by a `|` in the block, an emphasis
/// closed inside it) is [`ElementAttrs::Invalid`] too.
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
    let (len, mut reading) = match block_len(rest) {
        Some(len) => (len, read_element_attrs(&rest[..len])),
        None => {
            let len = rest.find('}')? + 1;
            match read_element_attrs(&rest[..len]) {
                invalid @ ElementAttrs::Invalid(_) => (len, invalid),
                _ => return None,
            }
        }
    };
    // the block must end inside the node's parent (a `|` in it may end a table cell, a `*`
    // an emphasis), or the element would not be where the block is
    let parent_end = node.parent().map(|parent| parent.data().sourcepos.end);
    let crosses =
        parent_end.is_some_and(|parent| parent.line == end.line && at + len > parent.column);
    if crosses && matches!(reading, ElementAttrs::Attrs(_)) {
        reading = ElementAttrs::Invalid(
            "it runs past the end of the table cell, emphasis or link the element is in"
                .to_string(),
        );
    }
    Some(Found {
        line: end.line,
        range: at..at + len,
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

/// The text with each fix's block replaced by its [`native_block`] (the fixes are ordered and
/// do not overlap), and where each fix's node must start in it: moved along its line by
/// what the rewritten blocks before it on that line added or removed.
fn rewrite_blocks<'f>(
    text: &str,
    fixes: &'f [Fix<'_>],
    starts: &[usize],
) -> (String, Vec<Target<'f>>) {
    let mut edited = String::with_capacity(text.len() + fixes.len() * 8);
    let mut copied = 0;
    for fix in fixes {
        edited.push_str(&text[copied..fix.range.start]);
        edited.push_str(&fix.native);
        copied = fix.range.end;
    }
    edited.push_str(&text[copied..]);
    let targets = fixes
        .iter()
        .map(|fix| {
            let (line, column) = fix.start;
            let line_start = starts.get(line - 1).copied().unwrap_or_default();
            let offset = line_start + column - 1;
            let (added, removed) = fixes
                .iter()
                .filter(|other| other.range.start >= line_start && other.range.end <= offset)
                .fold((0, 0), |(added, removed), other| {
                    (added + other.native.len(), removed + other.range.len())
                });
            Target {
                kind: fix.kind,
                start: (line, column + added - removed),
                attrs: &fix.attrs,
            }
        })
        .collect();
    (edited, targets)
}

/// For each target, whether `root` has a node of its kind that starts where it says and
/// carries exactly its attributes (comrak read the rewritten block).
fn read_by_comrak<'a>(root: &'a AstNode<'a>, targets: &[Target<'_>]) -> Vec<bool> {
    let mut read = vec![false; targets.len()];
    for node in root.descendants() {
        let data = node.data();
        let Some(kind) = Kind::of(&data.value) else {
            continue;
        };
        let start = (data.sourcepos.start.line, data.sourcepos.start.column);
        let Some(at) = targets
            .iter()
            .position(|target| target.kind == kind && target.start == start)
        else {
            continue;
        };
        read[at] = data.attrs.as_deref() == Some(targets[at].attrs);
    }
    read
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
    use crate::markdown::attrs::ParsedAttrs;
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
                "# Title {background=\"figures/bg.png\"}\n",
                attrs(None, &[], &[("background", "figures/bg.png")]),
            ),
            (
                "# T3 {.dark background=#222}\n",
                "# T3 {.\"dark\" background=\"#222\"}\n",
                attrs(None, &["dark"], &[("background", "#222")]),
            ),
            // autolinked by comrak: the block is found in the source, not in the inlines
            (
                "# W {background=https://ex.com/a.png}\n",
                "# W {background=\"https://ex.com/a.png\"}\n",
                attrs(None, &[], &[("background", "https://ex.com/a.png")]),
            ),
            // single quotes, a closing sequence, a block quote, a setext heading
            (
                "# Q {#q .x data-k='a b'} ##\n",
                "# Q {#\"q\" .\"x\" data-k=\"a b\"} ##\n",
                attrs(Some("q"), &["x"], &[("data-k", "a b")]),
            ),
            (
                "> # Q é {title='it\\'s \"x\" \\\\'}\n",
                "> # Q é {title=\"it's \\\"x\\\" \\\\\"}\n",
                attrs(None, &[], &[("title", "it's \"x\" \\")]),
            ),
            (
                "Setext {k='v'}\n===\n",
                "Setext {k=\"v\"}\n===\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            // `}` inside a quoted value, a `{` in the value
            (
                "# B {data-x='a {b}'}\n",
                "# B {data-x=\"a {b}\"}\n",
                attrs(None, &[], &[("data-x", "a {b}")]),
            ),
            // the block alone, also as a setext heading's only line
            (
                "# {k='v'}\n",
                "# {k=\"v\"}\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            (
                "Intro\n\n{k='v'}\n===\n",
                "Intro\n\n{k=\"v\"}\n===\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            // the last line of a multi-line setext heading
            (
                "Line one\n {k='v'}\n===\n",
                "Line one\n {k=\"v\"}\n===\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            // CRLF line breaks and a byte-order mark
            (
                "# T {k='v'}\r\n\r\ntext\r\n",
                "# T {k=\"v\"}\r\n\r\ntext\r\n",
                attrs(None, &[], &[("k", "v")]),
            ),
            (
                "\u{feff}# T {k='v'}\n",
                "\u{feff}# T {k=\"v\"}\n",
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
            "- ![a](img.png){width=\"50%\"} and `c`{data-x=\"y z\"} \
             [l](https://x.org){data-x=\"a/b\"}.\n\n\
             | ![t](t.png){width=\"5\"} | [r][ref]{k=\"v\"} |\n"
        ));
        assert!(codes.is_empty(), "{codes:?}");
    }

    /// A3 fix round: the result is what comrak gives for the block with double-quoted
    /// values, also where the text around the block matters.
    #[test]
    fn recovered_blocks_read_as_their_double_quoted_form() {
        let cases = [
            // two blocks on a heading: comrak reads the last one, the first stays text
            "# H {.a} {k='v'}\n",
            "![a](x.png){width='5'}{.b}\n",
            // adjacent code spans stay two code spans
            "Keys `Ctrl`{data-k='a'}`C`{data-k='b'} copy\n",
            "Keys `Ctrl`{data-k='a/b'}`C` copy\n",
            // a reference link keeps its target
            "[ref]{data-x='y'}(note)\n\n[ref]: https://r.org\n",
            "[a]{k='v'}[b]\n\n[a]: https://a.org\n[b]: https://b.org\n",
            // a setext heading whose only line is the block
            "Intro\n\n{k='v'}\n===\n",
        ];
        for text in cases {
            let quoted = text.replace('\'', "\"");
            let (found, edited, codes) = recover(text);
            assert_eq!(edited, quoted, "{text}");
            assert!(codes.is_empty(), "{text}: {codes:?}");
            let arena = Arena::new();
            let opts = dialect_options();
            let root = parse_document(&arena, &quoted, &opts);
            let native: Vec<_> = root
                .descendants()
                .filter_map(|node| {
                    let data = node.data();
                    Some((Kind::of(&data.value)?, data.attrs.as_deref()?.clone()))
                })
                .collect();
            assert_eq!(found, native, "{text}");
            let recovered = {
                let arena = Arena::new();
                let root = parse_document(&arena, text, &opts);
                let mut diagnostics = Diagnostics::new();
                let (root, _) = recover_attr_blocks(&arena, root, text, &opts, &mut diagnostics);
                html(root, &opts)
            };
            assert_eq!(recovered, html(root, &opts), "{text}");
        }
        let (_, edited, _) = recover("Keys `Ctrl`{data-k=a/b}`C` copy\n");
        assert_eq!(edited, "Keys `Ctrl`{data-k=\"a/b\"}`C` copy\n");
        let (found, _, _) = recover("Keys `Ctrl`{data-k='a'}`C`{data-k='b'} copy\n");
        assert_eq!(
            found,
            [
                (Kind::Code, attrs(None, &[], &[("data-k", "a")])),
                (Kind::Code, attrs(None, &[], &[("data-k", "b")])),
            ]
        );
        let (found, _, _) = recover("# H {.a} {k='v'}\n");
        assert_eq!(found, [(Kind::Heading, attrs(None, &[], &[("k", "v")]))]);
    }

    fn html<'a>(root: &'a AstNode<'a>, opts: &Options) -> String {
        let mut out = String::new();
        comrak::format_html(root, opts, &mut out).unwrap();
        out
    }

    /// A block that would end a table cell or an emphasis early is reported, not read.
    #[test]
    fn blocks_crossing_their_parent_are_reported() {
        for (text, line) in [
            ("| a | b |\n|---|---|\n| `c`{lines=1|3} | d |\n", 3),
            ("*[l](u){k='v*'}\n", 1),
        ] {
            let (found, edited, codes) = recover(text);
            assert!(found.is_empty(), "{text}: {found:?}");
            assert_eq!(edited, text);
            assert_eq!(codes, [(UNPARSED_ATTR_BLOCK, line)], "{text}");
        }
    }

    #[test]
    fn what_comrak_reads_itself_is_not_touched() {
        let text = "# H {.a k=v}\n\n![a](x.png){width=50%}\n";
        let (found, edited, codes) = recover(text);
        assert_eq!(edited, text);
        assert_eq!(found.len(), 2);
        assert!(codes.is_empty());
    }

    #[test]
    fn native_blocks_escape_quotes_and_backslashes() {
        let block = native_block(&attrs(
            Some("a b"),
            &["c", "d\"e"],
            &[("k", "x\\y"), ("data-z", "")],
        ));
        assert_eq!(block, r#"{#"a b" ."c" ."d\"e" k="x\\y" data-z=""}"#);
        assert_eq!(native_block(&attrs(None, &[], &[])), "{}");
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

    /// Wave-8 review of the A3 fixes: two blocks written without a space, or a stray `}`,
    /// were read as one block whose items kept the braces (`class="dark}{background=#222"`)
    /// and the text vanished. They stay text, with a warning; quoted braces still work.
    #[test]
    fn unquoted_braces_inside_a_block_keep_it_text() {
        for text in [
            "## Title {.dark}{background='#222'}\n",
            "## Title {.dark}{background=\"#222\"}\n",
            "## Title {k='v'}}\n",
        ] {
            let (found, edited, codes) = recover(text);
            assert!(found.is_empty(), "{text}: {found:?}");
            assert_eq!(edited, text);
            assert_eq!(codes, [(UNPARSED_ATTR_BLOCK, 1)], "{text}");
        }
        let (found, _, codes) = recover("## Title {data-x='a {b}' .c}\n");
        assert_eq!(
            found,
            [(Kind::Heading, attrs(None, &["c"], &[("data-x", "a {b}")]))]
        );
        assert!(codes.is_empty(), "{codes:?}");
        let info = "python {.a}{lines='1|3'}";
        assert!(matches!(
            fence_attrs(info),
            Some((rest, _, ElementAttrs::Invalid(_))) if rest == info
        ));
    }

    #[test]
    fn several_blocks_on_one_line_keep_their_nodes() {
        let text = "![a](a.png){w='1'} ![b](b.png){w='2'} `c`{k='3'}\n";
        let (found, edited, _) = recover(text);
        assert_eq!(edited, text.replace('\'', "\""));
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

    /// Random texts around attribute blocks: recovery never panics and keeps every line; a
    /// block comrak does not read even rewritten is reported on a line of the text.
    #[test]
    fn random_texts_keep_their_lines() {
        struct Lcg(u64);
        impl Lcg {
            fn below(&mut self, n: usize) -> usize {
                self.0 = self
                    .0
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((self.0 >> 33) % n as u64) as usize
            }
        }
        let pieces = [
            "`c`",
            "`",
            "{k='v'}",
            "{k=a/b}",
            "{.x}",
            "{k=v}",
            "[r]",
            "(u)",
            "![i](p.png)",
            "[l](u)",
            "# ",
            "\n",
            "\n\n",
            "===",
            "---",
            " ",
            "*",
            "|",
            "{",
            "}",
            "'",
            "\"",
            "a",
            "\\",
            "{k='v}'}",
            "[r]: https://r.org\n",
            "> ",
            "- ",
            "{#i}",
            "{lines=1|3}",
        ];
        let mut rng = Lcg(0xa3);
        let mut rewritten = 0;
        for _ in 0..20_000 {
            let text: String = (0..1 + rng.below(10))
                .map(|_| pieces[rng.below(pieces.len())])
                .collect();
            let (_, edited, codes) = recover(&text);
            let lines = crate::markdown::split::source_lines(&text).len();
            assert_eq!(
                crate::markdown::split::source_lines(&edited).len(),
                lines,
                "{text:?}"
            );
            assert!(codes.iter().all(|&(_, line)| (1..=lines).contains(&line)));
            rewritten += usize::from(edited != text);
        }
        assert!(rewritten > 500, "only {rewritten} texts rewritten");
    }
}
