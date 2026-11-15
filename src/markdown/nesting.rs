//! Nested directives with fences of one length, and `:::` lines that close nothing
//! (dialect §4, contract changes 2026-11-14 and 2026-11-15).
//!
//! comrak 0.56 gives a closing fence to the *outermost* open directive whose fence is not
//! longer than it, and closes everything inside that directive with it; a bare `:::` line
//! that closes nothing opens an unnamed directive. In
//!
//! ```markdown
//! ::::columns
//! :::column
//! :::small
//! Small text.
//! :::
//! :::
//! ::::
//! ```
//!
//! the first `:::` therefore closes the column (and the `small` in it), and the second one
//! opens an unnamed directive. Authors write nested directives the way pandoc reads them,
//! where a closing fence closes the innermost open directive, and the dialect reads them so:
//! a closing fence closes one directive, and a `:::` line that closes none is ignored.
//! [`renest_directives`] makes comrak read them that way:
//!
//! 1. **fences**: the directives comrak found are collected with the lines of their opening
//!    and closing fences ([`collect`]). Only the *directive tree* counts: directives whose
//!    ancestors are all directives (one inside a list item, block quote, footnote, … is left
//!    alone). Every other bare `:::` line as written is *content* in comrak's tree (of a
//!    code block, an HTML block, a list item, …): it closes a directive of the tree where
//!    one it can close is open, as in comrak, which looks for closing fences before it looks
//!    at what is inside a directive, and stays as written otherwise ([`Role::Text`]);
//! 2. **reading**: the fences are read with comrak's rule ([`Rule::Outermost`]), which must
//!    give comrak's own tree (otherwise nothing changes), and with the dialect's
//!    ([`Rule::Innermost`]): a closing fence closes, of the open directives it can close
//!    (fence not longer than it, fence line indented little enough), the innermost one among
//!    those with the longest fence. `::::` around `:::` blocks therefore reads as before, and
//!    `:::` inside `:::` closes the inner block. A fence without a name that can close none
//!    is ignored ([`Role::Ignore`]): a bare `:::`, or a `:::` followed only by something
//!    that reads as nothing (a no-break space, `&#32;`), which never closes one. A directive
//!    that no fence of its own closes ends with the directive around it, or at the end of
//!    the text, as in comrak, and is an `invalid-directive` warning ([`Unclosed`]);
//! 3. **rewrite**: fences are made longer (never shorter) until comrak reads them as the
//!    dialect does: every directive's fence becomes longer than the fences of the directives
//!    inside it, and every closing fence as long as the fence of the directive it closes; a
//!    top-level directive that comrak already reads so keeps its fences. An ignored line
//!    becomes a marker from its colons on ([`IgnoredFence`]): an HTML comment, which ends a
//!    paragraph and keeps a list tight as the fence did, and which [`report_ignored_fences`] reports, and the
//!    renderer and the slide splitter skip. The text is parsed again. When that changes
//!    which lines are fences (a `:::` that comrak's reading took as a closing fence inside a
//!    code block is code again; an indented fence that its early close left as an indented
//!    code block is a fence again), the new fence lines are read again, with the ignored
//!    ones and, past the first line where the new tree's fences are not those meant, the
//!    fences meant that it hides (a code block that a misreading there opens may hide
//!    them), as often as [`attempts`] allows. An ignored line before that first line whose
//!    marker is not an HTML block of its own (a `:::` that closed the unnamed directive
//!    around a code block, in which it is now) is no fence, and keeps its text. When
//!    comrak's tree is not the dialect's reading after all, or the fence lines do not
//!    settle, the original text and tree are kept, and so they are when comrak's reading or
//!    the dialect's nests directives more than [`MAX_DEPTH`] deep: an `invalid-directive`
//!    warning says so ([`Fallback`]);
//! 4. **strays elsewhere**: a `:::` line that comrak still reads as opening an unnamed
//!    directive (in a list item, block quote, footnote or definition, where comrak's rule
//!    stays, or in a text left as it was) becomes a marker too, and the text is parsed
//!    again, until none is left ([`mark_strays`], as often as [`attempts`] allows, since a
//!    marker frees the `:::` that closed the unnamed directive, which may then open one, or
//!    lands in a code block that directive no longer ends, and gets its text back; when
//!    they do not settle, a warning says so).
//!
//! Only colons are added to fence lines, and ignored lines are replaced from their colons
//! on, so every line keeps its number and slide boundaries do not move.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::error::Diagnostics;
use crate::ir::SourceSpan;
use crate::markdown::recover::split_lines;
use crate::markdown::render::{directive_label, report_ignored_fences};

/// How many times the text may be parsed again at least and at most (module docs, steps 3
/// and 4): as often as [`REPARSE_BUDGET`] allows within these bounds. A parse of the
/// rewritten text may uncover fences that comrak's reading hid in a code block (a `:::` that
/// closes a directive in comrak's reading ends a code block early, and the code block's
/// closing fence then opens one); the fences meant that it still hides are read again with
/// those it shows, so that a chain of such code blocks takes a few parses, not one each.
const ATTEMPTS: std::ops::RangeInclusive<usize> = 4..=64;

/// How many bytes of rewritten text [`renest`] and [`mark_strays`] each parse at most, beyond
/// [`ATTEMPTS`]' lower bound.
const REPARSE_BUDGET: usize = 16 << 20;

/// How deeply directives may nest, in comrak's reading or the dialect's, for the fences to
/// be rewritten: the outermost fence of `n` nested directives needs `n + 2` colons, so the
/// rewritten text grows with the square of the depth (and comrak's time with its cube).
const MAX_DEPTH: usize = 32;

/// How a marker ([`IgnoredFence`]) starts.
const MARKER: &str = "<!--presenter:ignored-fence";

/// A fence line of the directive tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence {
    /// 1-based line.
    line: usize,
    /// Number of colons as written.
    colons: usize,
    /// Columns before the colons (a tab advances to the next multiple of four).
    indent: usize,
    /// Byte offset of the colons in the line.
    at: usize,
    /// Whether the line is only the fence (`:::`, then spaces or tabs): only such a line
    /// closes a directive.
    bare: bool,
    /// Whether the directive it opens has a name or attributes (comrak's info, trimmed).
    named: bool,
    /// Whether the line is content in comrak's tree (of a code block, an HTML block, a list
    /// item, …, or a fence of a directive in a list item or block quote) rather than a fence
    /// of the directive tree: a bare fence that closes a directive where one it can close is
    /// open (comrak looks for closing fences before it looks at what is inside a directive),
    /// and stays what it is otherwise ([`Role::Text`]).
    content: bool,
}

/// What a fence does in a reading. Directives are named by the index of their opening fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Opens a directive inside `parent` (`None`: at the top of the tree).
    Open { parent: Option<usize> },
    /// Closes `target` (and the directives still open inside it).
    Close { target: usize },
    /// Closes nothing and opens nothing ([`Rule::Innermost`] for a fence without a name
    /// that can close no open directive): the line becomes a marker. `offset` is its
    /// indentation after the columns that the directives around it strip (comrak's
    /// `fence_offset` for a directive opened there).
    Ignore { offset: usize },
    /// A line of content ([`Fence::content`]) that closes nothing: it stays as written.
    Text,
}

/// Which open directive a closing fence closes, of those it can close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// The outermost one (comrak 0.56); a fence that closes none opens a directive.
    Outermost,
    /// The innermost one among those with the longest fence (the dialect); a fence without
    /// a name that closes none is ignored (one that is content stays as written).
    Innermost,
}

/// A directive open while the fences are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    /// Its opening fence.
    fence: usize,
    colons: usize,
    /// Columns stripped before this directive's fences are looked at.
    base: usize,
}

/// A reading of the fences: each fence's role. A `deep` reading nests directives more than
/// [`MAX_DEPTH`] deep: it stops at the fence that does, so it holds the roles up to that one
/// only, and it is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reading {
    roles: Vec<Role>,
    deep: bool,
}

/// A directive of the directive tree that no fence of its own closes (module docs, step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unclosed {
    /// The line of its opening fence.
    line: usize,
    /// The colons of its opening fence, as written.
    colons: usize,
    /// The fence that closed it with a directive around it: that fence's colons as written,
    /// and the line where that directive opens. `None`: the end of the text closed it.
    closed_by: Option<(usize, usize)>,
}

/// A `:::` line that closes no directive and opens none (dialect §4): the text holds a
/// marker in its place ([`renest_directives`]), which the renderer reports and drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IgnoredFence {
    /// The number of colons.
    pub(crate) colons: usize,
    /// Its indentation after the columns that the directives around it strip.
    pub(crate) offset: usize,
    /// Whether something other than spaces and tabs follows the colons (a no-break space,
    /// `&#32;`, …), which reads as nothing, but makes a line that closes no directive.
    pub(crate) odd: bool,
}

impl IgnoredFence {
    /// The marker that replaces the line from its colons on: an HTML comment, which ends a
    /// paragraph as the fence did, and ends on its own line.
    fn comment(self) -> String {
        let odd = if self.odd { " odd" } else { "" };
        format!(
            "{MARKER} colons={} offset={}{odd}-->",
            self.colons, self.offset
        )
    }

    /// The fence that an HTML block's `literal` marks, if it is a marker
    /// ([`Self::comment`]).
    pub(crate) fn from_html(literal: &str) -> Option<Self> {
        let words = literal
            .trim_matches([' ', '\t', '\r', '\n'])
            .strip_prefix(MARKER)?
            .strip_suffix("-->")?;
        let mut words = words.split(' ').filter(|word| !word.is_empty());
        let colons = words.next()?.strip_prefix("colons=")?.parse().ok()?;
        let offset = words.next()?.strip_prefix("offset=")?.parse().ok()?;
        let odd = match words.next() {
            None => false,
            Some("odd") => true,
            Some(_) => return None,
        };
        if words.next().is_some() {
            return None;
        }
        Some(Self {
            colons,
            offset,
            odd,
        })
    }
}

/// Reads directive fences as the dialect does (see the module docs): returns `root` and
/// `text` unchanged when comrak already reads them that way, else the text with longer
/// fences and with markers for the `:::` lines it ignores, and its tree, parsed again into
/// `arena` with `opts`. The caller must use both from then on (the splitter reads
/// separators from the text). A directive of the directive tree that no fence of its own
/// closes is an `invalid-directive` warning in `diagnostics`, on its opening line.
///
/// The directives of the tree it returns are as written, for the warnings to compare
/// ([`as_written`], which a caller that parses the text again must apply again): each has
/// the fence length of its opening line in `text`, and one opened by a `:::` followed only
/// by character references to ASCII white space (such as `&#32;`, which comrak decodes and
/// trims away) has a one-space info, where comrak's is empty as for a bare
/// `:::`.
pub fn renest_directives<'a, 't>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    text: &'t str,
    opts: &Options,
    diagnostics: &mut Diagnostics,
) -> (&'a AstNode<'a>, Cow<'t, str>) {
    let (root, edited, unclosed) = renest(arena, root, text, opts);
    let (root, edited, strays) = mark_strays(arena, root, edited, text, opts);
    as_written(root, text);
    match unclosed {
        Ok(unclosed) => report_unclosed(root, &unclosed, diagnostics),
        Err(fallback) => report_fallback(root, fallback, diagnostics),
    }
    if let Err(fallback) = strays {
        report_fallback(root, fallback, diagnostics);
    }
    report_ignored_fences(root, diagnostics);
    (root, edited)
}

/// Whether `node` is the marker of a `:::` line the dialect ignores ([`IgnoredFence`]): an
/// HTML block that is nothing else.
pub(crate) fn is_marker<'a>(node: &'a AstNode<'a>) -> bool {
    matches!(&node.data().value,
        NodeValue::HtmlBlock(block) if IgnoredFence::from_html(&block.literal).is_some())
}

/// Gives the directives of `root` the fence lengths and odd infos [`renest_directives`]
/// describes. `root` is a tree of `text`, or of a text that differs from it only within
/// lines, after every directive's first colon (longer fences, attribute blocks rewritten by
/// [`recover_attr_blocks`](crate::markdown::recover::recover_attr_blocks)), whose tree must
/// be given them again.
pub fn as_written<'a>(root: &'a AstNode<'a>, text: &str) {
    let mut lines = None;
    for node in root.descendants() {
        let mut data = node.data_mut();
        let start = data.sourcepos.start;
        let NodeValue::BlockDirective(directive) = &mut data.value else {
            continue;
        };
        // comrak's column is the 1-based byte of the first colon, a leading BOM included,
        // after the markers of the list items, block quotes, … the directive is in
        let lines = lines.get_or_insert_with(|| split_lines(text).1);
        let Some(fence) = start
            .line
            .checked_sub(1)
            .and_then(|index| lines.get(index))
            .and_then(|line| line.get(start.column.saturating_sub(1)..))
            .and_then(fence_line)
        else {
            continue;
        };
        directive.fence_length = fence.colons;
        if directive.info.is_empty() && !fence.bare {
            directive.info = " ".to_string();
        }
    }
}

/// Why the text is left as comrak reads it ([`renest`], [`mark_strays`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    /// comrak's reading or the dialect's nests directives more than [`MAX_DEPTH`] deep.
    Deep,
    /// comrak's tree is not the one expected, or the fence lines do not settle.
    Unread,
    /// The markers of the `:::` lines that open directives without a name do not settle.
    Strays,
}

/// How many times the text may be parsed again ([`ATTEMPTS`], [`REPARSE_BUDGET`]).
fn attempts(text: &str) -> usize {
    (REPARSE_BUDGET / text.len().max(1)).clamp(*ATTEMPTS.start(), *ATTEMPTS.end())
}

/// Steps 1 to 3 of the module docs: the tree and its text (unchanged, or rewritten and
/// parsed again), and the directives of the directive tree that no fence of their own
/// closes, or why the text is left as comrak reads it.
fn renest<'a, 't>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    text: &'t str,
    opts: &Options,
) -> (
    &'a AstNode<'a>,
    Cow<'t, str>,
    Result<Vec<Unclosed>, Fallback>,
) {
    let unchanged = |result| (root, Cow::Borrowed(text), result);
    let (starts, lines) = text_lines(text);
    // the bare fence lines as written: those that a tree does not have as fences of the
    // directive tree are content there
    let bare: Vec<Fence> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            fence_line(line)
                .filter(|fence| fence.bare)
                .map(|fence| Fence {
                    line: index + 1,
                    content: true,
                    ..fence
                })
        })
        .collect();
    // the tree the fence lines are taken from, and its text: the original, then the last
    // rewrite's when comrak found other fence lines in it
    let (mut tree, mut tree_text) = (root, Cow::Borrowed(text));
    // the fences that the last rewrite made markers of, which are not in its tree
    let mut ignored: Vec<Fence> = Vec::new();
    // the fences of the last reading that its rewrite's tree hides, past the first line where
    // that tree has other fences than those meant
    let mut carried: Vec<Fence> = Vec::new();
    for attempt in 0..attempts(text) {
        let (_, tree_lines) = text_lines(&tree_text);
        let Some((mut fences, comrak_roles)) = collect(tree, &tree_lines) else {
            tracing::debug!("directive fences: a fence is not where comrak's tree has it");
            return unchanged(Err(Fallback::Unread));
        };
        if attempt == 0 {
            if depth(&comrak_roles) > MAX_DEPTH {
                tracing::debug!("directive fences: nested more than {MAX_DEPTH} deep; left as is");
                return unchanged(Err(Fallback::Deep));
            }
            if read(&fences, Rule::Outermost).roles != comrak_roles {
                tracing::debug!(
                    "directive fences: comrak's tree is not the expected one; left as is"
                );
                return unchanged(Err(Fallback::Unread));
            }
            if fences.iter().all(|fence| fence.named) {
                // no fence closes a directive, and none is ignored
                return unchanged(Ok(unclosed(&fences, &comrak_roles)));
            }
        } else {
            // the fences as written, and those the last rewrite ignored
            for fence in &mut fences {
                let written = lines
                    .get(fence.line - 1)
                    .and_then(|line| fence_line(line))
                    .filter(|written| (written.at, written.bare) == (fence.at, fence.bare));
                let Some(written) = written else {
                    tracing::debug!("directive fences: a fence is not as written; left as is");
                    return unchanged(Err(Fallback::Unread));
                };
                fence.colons = written.colons;
            }
            fences.append(&mut ignored);
            fences.append(&mut carried);
            fences.sort_by_key(|fence| fence.line);
        }
        let fences = with_content(fences, &bare);
        let dialect = read(&fences, Rule::Innermost);
        let comrak = read(&fences, Rule::Outermost);
        if dialect.deep || comrak.deep {
            tracing::debug!("directive fences: nested more than {MAX_DEPTH} deep; left as is");
            return unchanged(Err(Fallback::Deep));
        }
        if attempt == 0
            && fences
                .iter()
                .zip(&comrak.roles)
                .any(|(fence, role)| fence.content && *role != Role::Text)
        {
            // comrak would have closed a directive with that line
            tracing::debug!("directive fences: comrak's tree is not the expected one; left as is");
            return unchanged(Err(Fallback::Unread));
        }
        let roles = dialect.roles;
        let colons = fence_lengths(&fences, &roles, &changed(&roles, &comrak.roles));
        let Some(edited) = rewrite(text, &starts, &fences, &roles, &colons) else {
            // comrak reads the text as the dialect does (known for the original tree only)
            if attempt == 0 {
                return unchanged(Ok(unclosed(&fences, &roles)));
            }
            tracing::debug!("directive fences: the fence lines do not settle; left as is");
            return unchanged(Err(Fallback::Unread));
        };
        let new_root = parse_document(arena, &edited, opts);
        let (_, new_lines) = text_lines(&edited);
        let Some((new_fences, new_roles)) = collect(new_root, &new_lines) else {
            tracing::debug!("directive fences: a fence is not where comrak's tree has it");
            return unchanged(Err(Fallback::Unread));
        };
        let (kept, expected) = without_ignored(&fences, &roles);
        let new_lines: Vec<usize> = new_fences.iter().map(|fence| fence.line).collect();
        // comrak reads the rewritten text as meant up to the first line where its fences
        // differ; past it, a code block that a misreading there opens may hide fences meant,
        // which the next reading takes too, so that the next rewrite does not undo them (one
        // misreading after the other would otherwise take one parse each)
        let first = first_difference(&kept, &new_lines);
        let hidden = |line: usize| first.is_some_and(|first| line > first);
        // the ignored lines whose markers are HTML blocks of their own; one that is not (in a
        // code block or an HTML block that no closing fence ends now) is no fence in the
        // dialect's reading, and the next rewrite leaves its text as written, unless it may be
        // hidden
        let markers = marker_lines(new_root);
        let mut misplaced = false;
        ignored = fences
            .iter()
            .zip(&roles)
            .filter(|(_, role)| matches!(role, Role::Ignore { .. }))
            .map(|(fence, _)| *fence)
            .filter(|fence| {
                let placed = markers.contains(&fence.line);
                misplaced |= !placed;
                placed || hidden(fence.line)
            })
            .collect();
        if !misplaced && first.is_none() {
            if new_roles == expected {
                return (new_root, Cow::Owned(edited), Ok(unclosed(&fences, &roles)));
            }
            tracing::debug!("directive fences: comrak does not read the rewritten text as meant");
            return unchanged(Err(Fallback::Unread));
        }
        carried = fences
            .iter()
            .zip(&roles)
            .filter(|&(fence, role)| {
                !fence.content
                    && matches!(role, Role::Open { .. } | Role::Close { .. })
                    && hidden(fence.line)
                    && new_lines.binary_search(&fence.line).is_err()
            })
            .map(|(fence, _)| *fence)
            .collect();
        tree = new_root;
        tree_text = Cow::Owned(edited);
    }
    tracing::debug!("directive fences: the fence lines do not settle; left as is");
    unchanged(Err(Fallback::Unread))
}

/// The first line in one of `a` and `b` (lines in order) but not in the other.
fn first_difference(a: &[usize], b: &[usize]) -> Option<usize> {
    let common = a.iter().zip(b).take_while(|(a, b)| a == b).count();
    match (a.get(common), b.get(common)) {
        (Some(&a), Some(&b)) => Some(a.min(b)),
        (Some(&line), None) | (None, Some(&line)) => Some(line),
        (None, None) => None,
    }
}

/// `fences` (in line order) with the lines of `bare` (the bare fence lines as written, in
/// line order, each [`Fence::content`]) that are not among them.
fn with_content(fences: Vec<Fence>, bare: &[Fence]) -> Vec<Fence> {
    let mut merged = Vec::with_capacity(fences.len() + bare.len());
    let mut bare = bare.iter().peekable();
    for fence in fences {
        while let Some(content) = bare.next_if(|content| content.line <= fence.line) {
            if content.line < fence.line {
                merged.push(*content);
            }
        }
        merged.push(fence);
    }
    merged.extend(bare.copied());
    merged
}

/// The lines of `text` and their byte offsets ([`split_lines`]), the first without the
/// byte order mark that comrak skips.
fn text_lines(text: &str) -> (Vec<usize>, Vec<&str>) {
    let (mut starts, mut lines) = split_lines(text);
    if let Some(first) = lines.first_mut()
        && let Some(rest) = first.strip_prefix('\u{feff}')
    {
        *first = rest;
        starts[0] += '\u{feff}'.len_utf8();
    }
    (starts, lines)
}

/// The fences of `root`'s directive tree in line order and the role comrak gave each, or
/// `None` when a fence line is not what comrak's tree says it is. A directive's closing
/// fence is the line it ends on when that line is a bare fence at least as long as its
/// opening fence, indented little enough to close it, and not where another directive opens
/// (a directive closed with its parent, or by the end of the text, ends on its last content
/// line instead, or on the parent's fence when it is empty or ends with blank lines).
fn collect<'a>(root: &'a AstNode<'a>, lines: &[&str]) -> Option<(Vec<Fence>, Vec<Role>)> {
    // (fence, opening line of the parent or target, whether it closes)
    let mut found: Vec<(Fence, Option<usize>, bool)> = Vec::new();
    // (node, opening line of the parent, columns comrak strips before the node's fences)
    let mut pending: Vec<(&'a AstNode<'a>, Option<usize>, usize)> = directive_children(root)
        .map(|node| (node, None, 0))
        .collect();
    pending.reverse();
    while let Some((node, parent, base)) = pending.pop() {
        let data = node.data();
        let NodeValue::BlockDirective(directive) = &data.value else {
            continue;
        };
        let (start, end) = (data.sourcepos.start.line, data.sourcepos.end.line);
        let opening = fence_line(lines.get(start.checked_sub(1)?)?)?;
        let named = !directive.info.trim().is_empty();
        if opening.colons != directive.fence_length || named && opening.bare {
            return None;
        }
        let fence = Fence {
            line: start,
            named,
            ..opening
        };
        found.push((fence, parent, false));
        if end > start
            && let Some(closing) = lines.get(end - 1).and_then(|line| fence_line(line))
            && closing.bare
            && closing.colons >= directive.fence_length
            && closing.indent.saturating_sub(base) <= 3
        {
            let fence = Fence {
                line: end,
                ..closing
            };
            found.push((fence, Some(start), true));
        }
        let inner = base + opening.indent.saturating_sub(base);
        let children: Vec<_> = directive_children(node).collect();
        pending.extend(
            children
                .into_iter()
                .rev()
                .map(|child| (child, Some(start), inner)),
        );
    }
    // A "closing fence" where a directive opens is the last line of a directive closed by
    // the end of the text, whose last child opens there. A directive closed with its parent
    // can end on the parent's closing fence: of the directives ending on one fence, the
    // outermost (opened first) is the one it closes.
    let opened: std::collections::HashSet<usize> = found
        .iter()
        .filter(|(_, _, closes)| !closes)
        .map(|(fence, _, _)| fence.line)
        .collect();
    found.retain(|(fence, _, closes)| !closes || !opened.contains(&fence.line));
    found.sort_by_key(|&(fence, other, _)| (fence.line, other));
    found.dedup_by_key(|(fence, _, _)| fence.line);
    let index_of = |line: usize| {
        found
            .binary_search_by_key(&line, |(fence, _, _)| fence.line)
            .ok()
    };
    let mut fences = Vec::with_capacity(found.len());
    let mut roles = Vec::with_capacity(found.len());
    for (fence, other, closes) in &found {
        let other = match other {
            Some(line) => Some(index_of(*line)?),
            None => None,
        };
        roles.push(match (closes, other) {
            (true, Some(target)) => Role::Close { target },
            (true, None) => return None,
            (false, parent) => Role::Open { parent },
        });
        fences.push(*fence);
    }
    Some((fences, roles))
}

/// The directive children of a node.
fn directive_children<'a>(node: &'a AstNode<'a>) -> impl Iterator<Item = &'a AstNode<'a>> {
    node.children()
        .filter(|child| matches!(child.data().value, NodeValue::BlockDirective(_)))
}

/// A line read as a directive fence: indentation, three or more colons, then anything
/// without a colon (`bare` when that is only spaces and tabs). `None` for anything else.
fn fence_line(line: &str) -> Option<Fence> {
    let rest = line.trim_start_matches([' ', '\t']);
    let at = line.len() - rest.len();
    let indent = line[..at].chars().fold(0, |column, c| match c {
        '\t' => column + 4 - column % 4,
        _ => column + 1,
    });
    let colons = rest.len() - rest.trim_start_matches(':').len();
    let info = &rest[colons..];
    if colons < 3 || info.contains(':') {
        return None;
    }
    Some(Fence {
        line: 0,
        colons,
        indent,
        at,
        bare: info.trim_matches([' ', '\t']).is_empty(),
        named: false,
        content: false,
    })
}

/// Reads `fences` with `rule` (module docs, step 2). A fence can close an open directive
/// when it is bare, at least as long as the directive's fence, and indented at most three
/// columns more than comrak strips there: each enclosing directive strips as many columns as
/// its own fence was indented relative to it.
fn read(fences: &[Fence], rule: Rule) -> Reading {
    let mut open: Vec<Open> = Vec::new();
    let mut roles = Vec::with_capacity(fences.len());
    for (i, fence) in fences.iter().enumerate() {
        // the outermost directive it can close, and the innermost of those with the longest
        // fence
        let (mut first, mut longest) = (None, None::<usize>);
        for (depth, directive) in open.iter().enumerate() {
            let closable = fence.bare
                && fence.colons >= directive.colons
                && fence.indent.saturating_sub(directive.base) <= 3;
            if !closable {
                continue;
            }
            first.get_or_insert(depth);
            if longest.is_none_or(|longest| directive.colons >= open[longest].colons) {
                longest = Some(depth);
            }
        }
        let target = match rule {
            Rule::Outermost => first,
            Rule::Innermost => longest,
        };
        if let Some(depth) = target {
            roles.push(Role::Close {
                target: open[depth].fence,
            });
            open.truncate(depth);
            continue;
        }
        if fence.content {
            roles.push(Role::Text);
            continue;
        }
        // the columns stripped inside the innermost open directive
        let base = open.last().map_or(0, |directive| {
            directive.base
                + fences[directive.fence]
                    .indent
                    .saturating_sub(directive.base)
        });
        if rule == Rule::Innermost && !fence.named {
            roles.push(Role::Ignore {
                offset: fence.indent.saturating_sub(base),
            });
            continue;
        }
        roles.push(Role::Open {
            parent: open.last().map(|directive| directive.fence),
        });
        open.push(Open {
            fence: i,
            colons: fence.colons,
            base,
        });
        // the reading is not used: reading on would take time quadratic in the depth
        if open.len() > MAX_DEPTH {
            return Reading { roles, deep: true };
        }
    }
    Reading { roles, deep: false }
}

/// How deeply the directives nest in `roles`.
fn depth(roles: &[Role]) -> usize {
    // the depth of each directive (by its opening fence), and the depth now
    let mut depths = vec![0; roles.len()];
    let (mut now, mut deepest) = (0, 0);
    for (i, role) in roles.iter().enumerate() {
        match *role {
            Role::Open { .. } => {
                depths[i] = now;
                now += 1;
                deepest = deepest.max(now);
            }
            Role::Close { target } => now = depths[target],
            Role::Ignore { .. } | Role::Text => {}
        }
    }
    deepest
}

/// The directives that no fence of their own closes in `roles` (module docs, step 2), in
/// line order.
fn unclosed(fences: &[Fence], roles: &[Role]) -> Vec<Unclosed> {
    let mut found = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, role) in roles.iter().enumerate() {
        match *role {
            Role::Open { .. } => open.push(i),
            Role::Close { target } => {
                let depth = open.iter().rposition(|&fence| fence == target).unwrap_or(0);
                found.extend(open[depth + 1..].iter().map(|&inner| Unclosed {
                    line: fences[inner].line,
                    colons: fences[inner].colons,
                    closed_by: Some((fences[i].colons, fences[target].line)),
                }));
                open.truncate(depth);
            }
            Role::Ignore { .. } | Role::Text => {}
        }
    }
    found.extend(open.iter().map(|&inner| Unclosed {
        line: fences[inner].line,
        colons: fences[inner].colons,
        closed_by: None,
    }));
    found.sort_unstable_by_key(|directive| directive.line);
    found
}

/// For each fence, whether the top-level directive it belongs to is read differently in
/// `roles` than in `comrak`: only those directives get longer fences.
fn changed(roles: &[Role], comrak: &[Role]) -> Vec<bool> {
    let mut changed = vec![false; roles.len()];
    let mut open: Vec<usize> = Vec::new();
    let mut start = 0;
    for (i, role) in roles.iter().enumerate() {
        match *role {
            Role::Open { .. } => open.push(i),
            Role::Close { target } => {
                let depth = open.iter().position(|&fence| fence == target).unwrap_or(0);
                open.truncate(depth);
            }
            Role::Ignore { .. } | Role::Text => {}
        }
        if open.is_empty() || i + 1 == roles.len() {
            if roles[start..=i] != comrak[start..=i] {
                changed[start..=i].fill(true);
            }
            start = i + 1;
        }
    }
    changed
}

/// The number of colons each fence needs for comrak to read `roles` (module docs, step 3):
/// for the fences marked in `changed`, a directive's fence is at least as long as before and
/// as its closing fence, and longer than the fence of every directive inside it; its closing
/// fence is as long as that. Other fences keep their length.
fn fence_lengths(fences: &[Fence], roles: &[Role], changed: &[bool]) -> Vec<usize> {
    let mut colons: Vec<usize> = fences.iter().map(|fence| fence.colons).collect();
    // in reverse order, every directive inside a directive and its closing fence come first
    for i in (0..fences.len()).rev().filter(|&i| changed[i]) {
        match roles[i] {
            Role::Close { target } => colons[target] = colons[target].max(fences[i].colons),
            Role::Open {
                parent: Some(parent),
            } => {
                colons[parent] = colons[parent].max(colons[i] + 1);
            }
            Role::Open { parent: None } | Role::Ignore { .. } | Role::Text => {}
        }
    }
    for i in (0..fences.len()).filter(|&i| changed[i]) {
        if let Role::Close { target } = roles[i] {
            colons[i] = colons[target];
        }
    }
    colons
}

/// `text` (whose lines start at `starts`) with each fence line given `colons[i]` colons, and
/// each fence that `roles` ignores replaced by its marker from its colons on; `None` when
/// nothing changes.
fn rewrite(
    text: &str,
    starts: &[usize],
    fences: &[Fence],
    roles: &[Role],
    colons: &[usize],
) -> Option<String> {
    let mut edited = String::with_capacity(text.len() + fences.len());
    let mut copied = 0;
    for ((fence, role), &want) in fences.iter().zip(roles).zip(colons) {
        let ignored = match *role {
            Role::Ignore { offset } => Some(IgnoredFence {
                colons: fence.colons,
                offset,
                odd: !fence.bare,
            }),
            _ if want == fence.colons => continue,
            _ => None,
        };
        let at = starts.get(fence.line - 1)? + fence.at;
        if !text.get(at..)?.starts_with(&":".repeat(fence.colons)) {
            return None;
        }
        edited.push_str(&text[copied..at]);
        match ignored {
            Some(ignored) => {
                edited.push_str(&ignored.comment());
                copied = text[at..]
                    .find(['\n', '\r'])
                    .map_or(text.len(), |end| at + end);
            }
            None => {
                edited.push_str(&":".repeat(want));
                copied = at + fence.colons;
            }
        }
    }
    if edited.is_empty() {
        return None;
    }
    edited.push_str(&text[copied..]);
    Some(edited)
}

/// The lines of the fences that `roles` neither ignores nor leaves as content, and their roles
/// with the fences numbered among them: what comrak should find in the rewritten text.
fn without_ignored(fences: &[Fence], roles: &[Role]) -> (Vec<usize>, Vec<Role>) {
    let mut index = vec![0; fences.len()];
    let mut lines = Vec::with_capacity(fences.len());
    let mut kept = Vec::with_capacity(fences.len());
    for (i, (fence, role)) in fences.iter().zip(roles).enumerate() {
        index[i] = kept.len();
        kept.push(match *role {
            Role::Ignore { .. } | Role::Text => continue,
            Role::Open { parent } => Role::Open {
                parent: parent.map(|parent| index[parent]),
            },
            Role::Close { target } => Role::Close {
                target: index[target],
            },
        });
        lines.push(fence.line);
    }
    (lines, kept)
}

/// Step 4 of the module docs: `root` and its `text` with the line of every directive
/// without a name that comrak opens replaced by a marker from its colons on, parsed again
/// into `arena` with `opts`, as many times as [`attempts`] allows at most (a round marks
/// every directive without a name left, and so frees the `:::` lines that closed them: a run
/// of `n` bare `:::` lines in a block quote takes about `log2(n)` rounds). A marker that is
/// not an HTML block of its own in the next tree (in a code block that the directive it marks
/// no longer ends, say) gets the line's text in `original` (the text as written) back in the
/// next round: the line is no fence there. When the markers do not settle, `root` and `text`
/// are kept, with [`Fallback::Strays`].
fn mark_strays<'a, 't>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    text: Cow<'t, str>,
    original: &str,
    opts: &Options,
) -> (&'a AstNode<'a>, Cow<'t, str>, Result<(), Fallback>) {
    let rounds = attempts(original);
    let (_, original) = split_lines(original);
    let (mut tree, mut edited) = (root, None::<String>);
    let mut settled = false;
    for round in 0..=rounds {
        let now = edited.as_deref().unwrap_or(&text);
        let strays = unnamed_directives(tree);
        let restore = misplaced_markers(tree, now, &original);
        if restore.is_empty() {
            settled = true;
            if strays.is_empty() {
                break;
            }
        } else {
            settled = false;
        }
        if round == rounds {
            break;
        }
        let Some(next) = mark(now, &strays, &restore, &original) else {
            tracing::debug!("directive fences: an unnamed directive is not where comrak says");
            settled = false;
            break;
        };
        tree = parse_document(arena, &next, opts);
        edited = Some(next);
        settled = false;
    }
    match edited {
        Some(edited) if settled => (tree, Cow::Owned(edited), Ok(())),
        Some(_) => {
            tracing::debug!("directive fences: the markers do not settle; left as is");
            (root, text, Err(Fallback::Strays))
        }
        None if settled => (root, text, Ok(())),
        None => (root, text, Err(Fallback::Strays)),
    }
}

/// The directives without a name in `root`: (line, 1-based byte of the first colon,
/// offset), in line order, one per line.
fn unnamed_directives<'a>(root: &'a AstNode<'a>) -> Vec<(usize, usize, usize)> {
    let mut found: Vec<(usize, usize, usize)> = root
        .descendants()
        .filter_map(|node| {
            let data = node.data();
            match &data.value {
                NodeValue::BlockDirective(directive) if directive.info.trim().is_empty() => {
                    let start = data.sourcepos.start;
                    Some((start.line, start.column, directive.fence_offset))
                }
                _ => None,
            }
        })
        .collect();
    found.sort_unstable();
    found.dedup_by_key(|stray| stray.0);
    found
}

/// The lines where `root` has a marker ([`IgnoredFence`]) as an HTML block of its own.
fn marker_lines<'a>(root: &'a AstNode<'a>) -> HashSet<usize> {
    root.descendants()
        .filter(|node| is_marker(node))
        .map(|node| node.data().sourcepos.start.line)
        .collect()
}

/// The 1-based lines of `text` (whose tree is `root`) that hold a marker where `original`
/// (the lines as written) does not, and that `root` does not have as an HTML block of its own.
fn misplaced_markers<'a>(root: &'a AstNode<'a>, text: &str, original: &[&str]) -> Vec<usize> {
    let (_, lines) = split_lines(text);
    let mut markers = None;
    lines
        .iter()
        .enumerate()
        .filter(|&(index, line)| line.contains(MARKER) && original.get(index) != Some(line))
        .map(|(index, _)| index + 1)
        .filter(|line| {
            !markers
                .get_or_insert_with(|| marker_lines(root))
                .contains(line)
        })
        .collect()
}

/// `text` with the line of each of `strays` (line, 1-based byte of its first colon as comrak
/// counts it, a leading BOM included, and offset) replaced by its marker from its colons on,
/// and each of the 1-based lines `restore` given its text in `original` back; `None` when a
/// stray's line holds no fence there.
fn mark(
    text: &str,
    strays: &[(usize, usize, usize)],
    restore: &[usize],
    original: &[&str],
) -> Option<String> {
    let (starts, lines) = split_lines(text);
    // (line, the stray's column and offset, or `None` to restore the line), in line order
    let mut edits: Vec<(usize, Option<(usize, usize)>)> = strays
        .iter()
        .map(|&(line, column, offset)| (line, Some((column, offset))))
        .chain(restore.iter().map(|&line| (line, None)))
        .collect();
    edits.sort_unstable_by_key(|edit| edit.0);
    edits.dedup_by_key(|edit| edit.0);
    let mut edited = String::with_capacity(text.len() + strays.len() * (MARKER.len() + 16));
    let mut copied = 0;
    for (line, edit) in edits {
        let index = line.checked_sub(1)?;
        let current = lines.get(index)?;
        let (at, replacement) = match edit {
            Some((column, offset)) => {
                let rest = current.get(column.checked_sub(1)?..)?;
                let fence = fence_line(rest).filter(|fence| fence.at == 0)?;
                let ignored = IgnoredFence {
                    colons: fence.colons,
                    offset,
                    odd: !fence.bare,
                };
                (starts[index] + column - 1, ignored.comment())
            }
            None => (starts[index], original.get(index)?.to_string()),
        };
        edited.push_str(&text[copied..at]);
        edited.push_str(&replacement);
        copied = starts[index] + current.len();
    }
    edited.push_str(&text[copied..]);
    Some(edited)
}

/// Reports each of `unclosed` as an `invalid-directive` warning on its opening line, named
/// as `root` (the tree [`renest_directives`] returns) has it. The message names no lines: in
/// a notebook cell they would be the cell's own.
fn report_unclosed<'a>(
    root: &'a AstNode<'a>,
    unclosed: &[Unclosed],
    diagnostics: &mut Diagnostics,
) {
    if unclosed.is_empty() {
        return;
    }
    let labels: HashMap<usize, String> = root
        .descendants()
        .filter_map(|node| {
            let data = node.data();
            match &data.value {
                NodeValue::BlockDirective(directive) => {
                    Some((data.sourcepos.start.line, directive_label(&directive.info)))
                }
                _ => None,
            }
        })
        .collect();
    let label = |line: usize| labels.get(&line).map_or("directive", String::as_str);
    for directive in unclosed {
        let how = match directive.closed_by {
            Some((colons, outer)) => format!(
                "has no closing fence of its own: the `{}` line that closes the {} around it \
                 ends it too",
                ":".repeat(colons),
                label(outer)
            ),
            None => "has no closing fence: it runs to the end of the document (or notebook \
                     cell), and `---` lines inside it start no slides"
                .to_string(),
        };
        let message = format!(
            "the {} {how}. Add one (`{}`) where it ends",
            label(directive.line),
            ":".repeat(directive.colons)
        );
        diagnostics.warn("invalid-directive", message).span = Some(SourceSpan {
            start_line: directive.line,
            end_line: directive.line,
            cell: None,
            file: None,
        });
    }
}

/// The `invalid-directive` warning for a text left as comrak reads it, on the line of its
/// first directive (or, for [`Fallback::Strays`], its first directive without a name).
fn report_fallback<'a>(root: &'a AstNode<'a>, fallback: Fallback, diagnostics: &mut Diagnostics) {
    let line = root.descendants().find_map(|node| {
        let data = node.data();
        match &data.value {
            NodeValue::BlockDirective(directive)
                if fallback != Fallback::Strays || directive.info.trim().is_empty() =>
            {
                Some(data.sourcepos.start.line)
            }
            _ => None,
        }
    });
    let comrak = "so they are read as comrak reads them: a closing fence closes the outermost \
                  directive it can, with everything in it, and a directive whose own closing \
                  fence is missing is not reported";
    let message = match fallback {
        Fallback::Deep => format!(
            "directives nest more than {MAX_DEPTH} deep in this document (or notebook cell), \
             {comrak}. Nest them less deeply"
        ),
        Fallback::Unread => format!(
            "presenter could not rewrite the `:::` fences of this document (or notebook cell) \
             for comrak to read them as the dialect does, {comrak}. Outer directives with \
             longer fences than those inside them read the same either way"
        ),
        Fallback::Strays => "presenter could not take the `:::` lines with nothing to close \
                             out of this document (or notebook cell), so each opens a directive \
                             without a name, as comrak reads it, which runs to the next `:::` \
                             line that can close it (`---` lines inside it start no slides). \
                             Delete the `:::` lines reported as ignored"
            .to_string(),
    };
    diagnostics.warn("invalid-directive", message).span = line.map(|line| SourceSpan {
        start_line: line,
        end_line: line,
        cell: None,
        file: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::split::dialect_options;

    /// The text as the dialect reads it (rewritten or not), its tree ([`tree`]), and the
    /// lines and messages of the warnings, without those for ignored `:::` lines
    /// ([`ignored_lines`]).
    fn renest_warned(text: &str) -> (String, String, Vec<(usize, String)>) {
        let (edited, tree, warnings) = renest_all(text);
        let warnings = warnings
            .into_iter()
            .filter(|(_, message)| !is_ignored_line(message))
            .collect();
        (edited, tree, warnings)
    }

    /// Whether a warning's message is that of an ignored `:::` line.
    fn is_ignored_line(message: &str) -> bool {
        message.starts_with("this `:::` line")
    }

    /// The lines of the warnings for ignored `:::` lines in `text`.
    fn ignored_lines(text: &str) -> Vec<usize> {
        renest_all(text)
            .2
            .into_iter()
            .filter(|(_, message)| is_ignored_line(message))
            .map(|(line, _)| line)
            .collect()
    }

    /// [`renest_warned`] with all the warnings.
    fn renest_all(text: &str) -> (String, String, Vec<(usize, String)>) {
        let arena = Arena::new();
        let opts = dialect_options();
        let root = parse_document(&arena, text, &opts);
        let mut diagnostics = Diagnostics::new();
        let (root, edited) = renest_directives(&arena, root, text, &opts, &mut diagnostics);
        let warnings = diagnostics
            .iter()
            .map(|d| {
                assert_eq!(d.code, "invalid-directive");
                let span = d.span.as_ref().expect("a span");
                assert_eq!(span.start_line, span.end_line);
                (span.start_line, d.message.clone())
            })
            .collect();
        (edited.into_owned(), tree(root), warnings)
    }

    /// [`renest_warned`] without the warnings.
    fn renest(text: &str) -> (String, String) {
        let (edited, tree, _) = renest_warned(text);
        (edited, tree)
    }

    /// The lines of the warnings [`renest_warned`] finds.
    fn warned(text: &str) -> Vec<usize> {
        renest_warned(text)
            .2
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    /// The marker of a bare `:::` line, indented by `offset`.
    fn marker(offset: usize) -> String {
        IgnoredFence {
            colons: 3,
            offset,
            odd: false,
        }
        .comment()
    }

    /// A node's children as `info[children]` for a directive (`[…]` for a bare one), the
    /// text of a paragraph, `#text` for a heading, `---` for a thematic break, `code` for a
    /// code block, `~` for the marker of an ignored `:::` line, `html` for other HTML,
    /// `>[children]` for a block quote, separated by spaces.
    fn tree<'a>(node: &'a AstNode<'a>) -> String {
        let text = |node: &'a AstNode<'a>| -> String {
            node.descendants()
                .filter_map(|n| match &n.data().value {
                    NodeValue::Text(t) => Some(t.to_string()),
                    _ => None,
                })
                .collect()
        };
        node.children()
            .map(|child| match &child.data().value {
                NodeValue::BlockDirective(d) => format!("{}[{}]", d.info, tree(child)),
                NodeValue::Paragraph => text(child),
                NodeValue::Heading(_) => format!("#{}", text(child)),
                NodeValue::ThematicBreak => "---".to_string(),
                NodeValue::CodeBlock(_) => "code".to_string(),
                NodeValue::HtmlBlock(html) => match IgnoredFence::from_html(&html.literal) {
                    Some(_) => "~".to_string(),
                    None => "html".to_string(),
                },
                NodeValue::BlockQuote => format!(">[{}]", tree(child)),
                _ => tree(child),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn same_length_fences_nest_like_pandoc() {
        let text = "\
:::columns
:::column
:::small
Small.
:::
:::
:::column
Right.
:::
:::
";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(
            edited,
            "\
:::::columns
::::column
:::small
Small.
:::
::::
:::column
Right.
:::
:::::
"
        );
        assert_eq!(tree, "columns[column[small[Small.]] column[Right.]]");
        assert_eq!(warnings, []);
    }

    #[test]
    fn a_third_level_inside_the_documented_columns_form() {
        // the guide's `::::columns` / `:::column` form, with `:::small` in a column
        let text = "\
::::columns
:::column
:::small
Small.
:::
:::
:::column
Right.
:::
::::
";
        let (edited, tree) = renest(text);
        assert_eq!(
            edited,
            ":::::columns\n::::column\n:::small\nSmall.\n:::\n::::\n:::column\nRight.\n:::\n:::::\n"
        );
        assert_eq!(tree, "columns[column[small[Small.]] column[Right.]]");
    }

    #[test]
    fn longer_outer_fences_read_as_before() {
        for text in [
            "::::columns\n:::column\nA\n:::\n:::column\nB\n:::\n::::\n",
            ":::::columns\n::::column\n:::small\nA\n:::\n::::\n:::::\n",
            // a missing `:::` before a longer closing fence: it closes both, as before
            "::::columns\n:::column\nA\n::::\n\nAfter.\n",
            "Just text.\n\n:::fragment\nA\n:::\n",
        ] {
            let (edited, _) = renest(text);
            assert_eq!(edited, text);
        }
        let (_, tree, warnings) = renest_warned("::::columns\n:::column\nA\n::::\n\nAfter.\n");
        assert_eq!(tree, "columns[column[A]] After.");
        // the column has no closing fence of its own
        assert_eq!(
            warnings,
            [(
                2,
                "the `column` directive has no closing fence of its own: the `::::` line that \
                 closes the `columns` directive around it ends it too. Add one (`:::`) where it \
                 ends"
                    .to_string()
            )]
        );
    }

    #[test]
    fn a_block_runs_to_its_own_fence() {
        // the last `:::` closes the `small` only; the column runs to the end of the text
        let text = ":::column\n:::small\nText.\n:::\n";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(edited, "::::column\n:::small\nText.\n:::\n");
        assert_eq!(tree, "column[small[Text.]]");
        assert_eq!(
            warnings,
            [(
                1,
                "the `column` directive has no closing fence: it runs to the end of the \
                 document (or notebook cell), and `---` lines inside it start no slides. Add \
                 one (`:::`) where it ends"
                    .to_string()
            )]
        );
        // so the next column is inside it, and so are the next slides
        let text = "\
:::column
:::small
A
:::

:::column
B
:::

---

Next.
";
        let (_, tree, warnings) = renest_warned(text);
        assert_eq!(tree, "column[small[A] column[B] --- Next.]");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, 1);
    }

    #[test]
    fn a_missing_closing_fence_leaves_its_directive_open() {
        let text = ":::fragment\n:::small\nA\n:::\n\nAfter.\n";
        let (edited, tree) = renest(text);
        assert_eq!(edited, "::::fragment\n:::small\nA\n:::\n\nAfter.\n");
        assert_eq!(tree, "fragment[small[A] After.]");
        assert_eq!(warned(text), [1]);
        // the columns' own `:::` is missing: they hold the rest of the text
        let text = "\
:::columns
:::column
:::small
A
:::
:::
:::column
B
:::

---

Next.
";
        let (_, tree) = renest(text);
        assert_eq!(tree, "columns[column[small[A]] column[B] --- Next.]");
        assert_eq!(warned(text), [1]);
        // a later missing `:::` leaves an earlier slide alone
        let text = "\
:::columns
:::column
:::small
A
:::
:::
:::column
B
:::
:::

---

:::fragment
:::small
C
:::
";
        let (_, tree) = renest(text);
        assert_eq!(
            tree,
            "columns[column[small[A]] column[B]] --- fragment[small[C]]"
        );
        assert_eq!(warned(text), [14]);
    }

    #[test]
    fn top_level_directives_that_read_as_written_keep_their_fences() {
        let text = "\
::::columns
:::column
:::small
A
:::
:::
::::

::::fragment
:::small
B
:::
::::
";
        let (edited, tree) = renest(text);
        assert_eq!(
            edited,
            "\
:::::columns
::::column
:::small
A
:::
::::
:::::

::::fragment
:::small
B
:::
::::
"
        );
        assert_eq!(tree, "columns[column[small[A]]] fragment[small[B]]");
    }

    #[test]
    fn a_fence_too_many_is_ignored() {
        // the stray `:::` closes nothing and opens nothing: the line becomes a marker, and
        // the notes of the later slides stay theirs, and so do the separators
        let text = "\
# One

:::notes
Say one.
:::
:::

---

# Two

:::notes
Say two.
:::

---

# Three

:::notes
Say three.
:::
";
        let (edited, tree, warnings) = renest_warned(text);
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[5] = marker(0);
        assert_eq!(edited, lines.join("\n") + "\n");
        assert_eq!(
            tree,
            "#One notes[Say one.] ~ --- #Two notes[Say two.] --- #Three notes[Say three.]"
        );
        // the renderer reports the marker
        assert_eq!(warnings, []);
        // within a slide too
        let (_, tree) = renest(":::fragment\nA\n:::\n:::\nB\n:::small\nC\n:::\nD\n");
        assert_eq!(tree, "fragment[A] ~ B small[C] D");
        // before a longer directive
        let (edited, tree) = renest(":::\n::::fragment\nA\n::::\n\n---\n\nB\n");
        assert_eq!(
            edited,
            format!("{}\n::::fragment\nA\n::::\n\n---\n\nB\n", marker(0))
        );
        assert_eq!(tree, "~ fragment[A] --- B");
        // with `::::` around the second slide's notes
        let text = "# One\n\n:::notes\nSay one.\n:::\n:::\n\n---\n\n# Two\n\n::::notes\nSay two.\n\
                    ::::\n\n---\n\n# Three\n\n:::notes\nSay three.\n:::\n";
        let (_, tree) = renest(text);
        assert_eq!(
            tree,
            "#One notes[Say one.] ~ --- #Two notes[Say two.] --- #Three notes[Say three.]"
        );
    }

    #[test]
    fn a_fence_too_many_does_not_undo_the_nesting() {
        // the last `:::` is one too many: the nesting above it stays
        let text = "\
:::columns
:::column
:::small
A
:::
:::
:::column
B
:::
:::
:::
C
";
        let (_, tree, warnings) = renest_warned(text);
        assert_eq!(tree, "columns[column[small[A]] column[B]] ~ C");
        assert_eq!(warnings, []);
    }

    #[test]
    fn a_fence_that_cannot_close_the_directive_around_it_is_ignored() {
        // too short, and indented too far where the directive's fence is looked for
        let (edited, tree, warnings) = renest_warned("::::note\nA\n:::\nB\n::::\n");
        assert_eq!(edited, format!("::::note\nA\n{}\nB\n::::\n", marker(0)));
        assert_eq!(tree, "note[A ~ B]");
        assert_eq!(warnings, []);
        let (edited, tree) = renest(":::note\n  :::small\n  A\n\n      :::\n  :::\n:::\n");
        // `      :::` is indented 4 more than the `small` it would close: inside the
        // small, whose content is stripped of 2 columns, it is code
        assert_eq!(
            edited,
            "::::note\n  :::small\n  A\n\n      :::\n  :::\n::::\n"
        );
        assert_eq!(tree, "note[small[A code]]");
        // after a paragraph line it continues the paragraph
        let (_, tree) = renest(":::note\n  :::small\n  A\n      :::\n  :::\n:::\n");
        assert_eq!(tree, "note[small[A:::]]");
    }

    #[test]
    fn a_missing_and_a_stray_fence_pair_up() {
        // slide 1 misses a `:::`, slide 2 has one too many: that one closes the notes, which
        // hold slide 2 (`presenter check` reports the separator inside them)
        let text = "\
# Intro

:::notes
:::small
Say hello slowly.
:::

---

# Results

:::center
Numbers went up.
:::
:::
";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(
            edited,
            text.replacen(":::notes", "::::notes", 1)
                .replace(":::\n:::\n", ":::\n::::\n")
        );
        assert_eq!(
            tree,
            "#Intro notes[small[Say hello slowly.] --- #Results center[Numbers went up.]]"
        );
        assert_eq!(warnings, []);
        // a closing fence as long as the outer one on the next slide
        let text = "\
# Intro

::::notes
::::small
Say hello slowly.
::::

---

# Results

:::fragment
Numbers went up.
::::
";
        let (_, tree) = renest(text);
        assert_eq!(
            tree,
            "#Intro notes[small[Say hello slowly.] --- #Results fragment[Numbers went up.]]"
        );
        assert_eq!(warned(text), [12]);
    }

    #[test]
    fn a_separator_inside_a_directive_is_kept() {
        let text = ":::notes\nA\n\n---\n\nB\n:::\n";
        let (edited, tree) = renest(text);
        assert_eq!(edited, text);
        assert_eq!(tree, "notes[A --- B]");
    }

    #[test]
    fn deep_nesting_is_left_as_comrak_reads_it() {
        // the rewrite would need `n + 2` colons on the outermost of `n` nested directives
        let deep = |n: usize| format!("{}x\n{}", ":::a\n".repeat(n), ":::\n".repeat(n));
        // without [`tree`], which recurses as deep as the directives nest
        let edited = |text: &str| {
            let arena = Arena::new();
            let opts = dialect_options();
            let root = parse_document(&arena, text, &opts);
            let mut diagnostics = Diagnostics::new();
            let (root, edited) = renest_directives(&arena, root, text, &opts, &mut diagnostics);
            let unnamed = root.descendants().any(|node| {
                matches!(&node.data().value,
                    NodeValue::BlockDirective(directive) if directive.info.is_empty())
            });
            let warnings: Vec<_> = diagnostics
                .iter()
                .filter(|d| !is_ignored_line(&d.message))
                .map(|d| {
                    (
                        d.span.as_ref().map(|span| span.start_line),
                        d.message.clone(),
                    )
                })
                .collect();
            let ignored = diagnostics.len() - warnings.len();
            (edited.into_owned(), unnamed, warnings, ignored)
        };
        // as deep as the reading goes: each level one colon longer than the one inside it
        let n = MAX_DEPTH;
        let expected: String = (0..n)
            .map(|level| format!("{}a\n", ":".repeat(n + 2 - level)))
            .chain(std::iter::once("x\n".to_string()))
            .chain((0..n).map(|level| format!("{}\n", ":".repeat(3 + level))))
            .collect();
        assert_eq!(edited(&deep(n)), (expected, false, Vec::new(), 0));
        let started = std::time::Instant::now();
        let text = deep(3000);
        let (edited, unnamed, warnings, ignored) = edited(&text);
        // no fence is made longer; comrak's reading stays, and its `:::` lines that close
        // nothing are ignored all the same; a warning says why the nesting is comrak's, and
        // none is about what stays open
        assert!(!edited.contains("::::"));
        assert_eq!(edited.lines().count(), text.lines().count());
        assert!(edited.contains(&marker(0)));
        assert!(!unnamed);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, Some(1));
        assert!(
            warnings[0]
                .1
                .starts_with("directives nest more than 32 deep in this document"),
            "{warnings:?}"
        );
        assert_eq!(ignored, 2999);
        // took tens of seconds when the depth was not bounded (now well under one, in a debug
        // build; the bound leaves room for slow machines)
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    #[test]
    fn many_directives_take_linear_time() {
        // collect and the reading were quadratic in the number of fences: a debug build takes
        // about 2 s here, and took about 40 s with a quadratic lookup in collect
        let started = std::time::Instant::now();
        let text = ":::fragment\nA\n:::\n\n---\n\n".repeat(80_000);
        let (edited, _) = renest(&text);
        assert_eq!(edited, text);
        let text = ":::fragment\nA\n:::\n\n".repeat(80_000);
        let (edited, _) = renest(&text);
        assert_eq!(edited, text);
        // the dialect's reading nests as deep as there are `a`s (comrak's, two deep): reading
        // stops past MAX_DEPTH, and the text is left as comrak reads it
        let text = ":::a\n:::b\n:::\n".repeat(40_000);
        let (edited, _) = renest(&text);
        assert_eq!(edited, text);
        let text = format!(
            "::::a\n{}::::\n",
            format!("{}\n---\n\n", "\n:::c\n:::d\n:::\n".repeat(1000)).repeat(31)
        );
        let (edited, _) = renest(&text);
        assert_eq!(edited, text);
        // strays, all made markers in one rewrite
        let text = ":::fragment\nA\n:::\n:::\n\n".repeat(40_000);
        let (edited, _) = renest(&text);
        assert_eq!(edited.matches(&marker(0)).count(), 40_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    #[test]
    fn fences_in_code_blocks_do_not_count() {
        // the `:::` in the code block is shorter than the fence around it, so it is code
        // (comrak closes a directive with a long enough fence line even inside code)
        let text = "\
::::columns
```markdown
:::
```
:::column
:::small
A
:::
:::
::::
";
        let (edited, tree) = renest(text);
        assert_eq!(
            edited,
            ":::::columns\n```markdown\n:::\n```\n::::column\n:::small\nA\n:::\n::::\n:::::\n"
        );
        assert_eq!(tree, "columns[code column[small[A]]]");
    }

    #[test]
    fn a_later_fence_in_code_does_not_undo_the_nesting() {
        // comrak's own reading closes the unnamed directive its early close opened with the
        // `:::` in the later code block; read as the dialect does, that line is code again
        let text = "\
:::columns
:::column
:::small
A
:::
:::
:::column
B
:::
:::

---

```markdown
:::fragment
Appears on the next click.
:::
```
";
        let (_, tree, warnings) = renest_warned(text);
        assert_eq!(tree, "columns[column[small[A]] column[B]] --- code");
        assert_eq!(warnings, []);
        // a later directive in a list item, or a `:::` in an HTML block
        for later in ["- item\n\n  :::small\n  C\n  :::\n", "<div>\n:::\n</div>\n"] {
            let text = format!(":::fragment\n:::small\nA\n:::\n:::\n\n---\n\n{later}");
            let (_, tree) = renest(&text);
            assert!(tree.starts_with("fragment[small[A]] ---"), "{text}\n{tree}");
        }
    }

    #[test]
    fn chains_of_fences_in_code_take_a_few_parses() {
        // each slide's columns, read as comrak does, end early and leave a `:::` that opens
        // a directive around the next code block, whose `:::` ends it early, so that the
        // code block's closing fence opens one that hides the next slide: one parse per
        // slide, unless the next reading takes the fences and ignored lines the rewrite hides
        // too (all of them settle in a few parses then, far fewer than the attempts allowed)
        let columns = ":::columns\n:::column\nA\n:::\n:::column\nB\n:::\n:::\n";
        let sample = "```markdown\n:::note\nText\n:::\n```\n";
        let mut text = format!("{columns}\n---\n\n{sample}\n---\n\n").repeat(100);
        text.push_str("End\n");
        assert!(attempts(&text) > 50);
        let (_, tree, warnings) = renest_all(&text);
        let expected = "columns[column[A] column[B]] --- code --- ".repeat(100) + "End";
        assert_eq!(tree, expected);
        assert_eq!(warnings, []);
        // a `:::` too many before each code block
        let mut text = format!(":::\n\n{sample}\n---\n\n").repeat(100);
        text.push_str(columns);
        let (_, tree, warnings) = renest_warned(&text);
        let expected = "~ code --- ".repeat(100) + "columns[column[A] column[B]]";
        assert_eq!(tree, expected);
        assert_eq!(warnings, []);
        let lines: Vec<usize> = (0..100).map(|slide| slide * 10 + 1).collect();
        assert_eq!(ignored_lines(&text), lines);
    }

    #[test]
    fn a_fence_that_was_code_is_read_again_with_the_ignored_ones() {
        // comrak's early close leaves `  :::` as text of a paragraph; once the column holds
        // the small, the second `:::` in the column is read again and closes it
        let text = ":::column\n:::small\nA\n:::\n:::\n:::\n";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(
            edited,
            format!("::::column\n:::small\nA\n:::\n::::\n{}\n", marker(0))
        );
        assert_eq!(tree, "column[small[A]] ~");
        assert_eq!(warnings, []);
    }

    #[test]
    fn indented_inner_fences_nest() {
        let (_, tree) = renest(":::columns\n  :::column\n  A\n  :::\n:::\n\nAfter.\n");
        assert_eq!(tree, "columns[column[A]] After.");
        // comrak's early close leaves the deeper fences as an indented code block
        let text = ":::a\n  :::b\n  :::c\n  x\n  :::\n    :::d\n    y\n    :::\n  :::\n:::\n";
        let (_, tree) = renest(text);
        assert_eq!(tree, "a[b[c[x] d[y]]]");
    }

    #[test]
    fn pandoc_attribute_blocks_nest() {
        let (_, tree) = renest("::: {.columns}\n::: {.column}\n::: {.small}\nA\n:::\n:::\n:::\n");
        assert_eq!(tree, "{.columns}[{.column}[{.small}[A]]]");
    }

    #[test]
    fn a_byte_order_mark_does_not_matter() {
        let text = "\u{feff}:::columns\n:::column\n:::small\nA\n:::\n:::\n:::column\nB\n:::\n:::\n";
        let (edited, tree) = renest(text);
        assert!(edited.starts_with("\u{feff}:::::columns\n"), "{edited:?}");
        assert_eq!(tree, "columns[column[small[A]] column[B]]");
        // a stray on the first line, at the top and in a block quote
        let (edited, tree) = renest("\u{feff}:::\nA\n");
        assert_eq!(edited, format!("\u{feff}{}\nA\n", marker(0)));
        assert_eq!(tree, "~ A");
        let (edited, tree) = renest("\u{feff}> :::\n> A\n");
        assert_eq!(edited, format!("\u{feff}> {}\n> A\n", marker(0)));
        assert_eq!(tree, ">[~ A]");
    }

    #[test]
    fn an_odd_fence_is_ignored() {
        // `:::` followed by a no-break space, an ideographic space or `&#32;` closes nothing
        // (comrak trims ASCII whitespace only), and opens nothing either
        for odd in [":::\u{a0}", ":::\u{3000}", ":::&#32;"] {
            let text = format!(
                ":::columns\n:::column\n:::small\nA\n:::\n:::\n:::\n\n---\n\n:::fragment\nB\n{odd}\n"
            );
            let (edited, tree, warnings) = renest_warned(&text);
            assert_eq!(
                tree, "columns[column[small[A]]] --- fragment[B ~]",
                "{text:?}"
            );
            let odd = IgnoredFence {
                colons: 3,
                offset: 0,
                odd: true,
            };
            assert!(
                edited.ends_with(&format!("\nB\n{}\n", odd.comment())),
                "{edited:?}"
            );
            // the fragment is never closed
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].0, 11);
        }
    }

    #[test]
    fn strays_in_containers_are_ignored() {
        // comrak's rule stays in a block quote: the second `:::` closes the unnamed directive
        // the first one opens; once that one is a marker, the second opens one, and is a
        // marker in the next round
        let (edited, tree) = renest("> :::\n> x\n> :::\n");
        assert_eq!(edited, format!("> {m}\n> x\n> {m}\n", m = marker(0)));
        assert_eq!(tree, ">[~ x ~]");
        // in a list item, which stays tight
        let text = "- a\n  :::\n- b\n";
        let arena = Arena::new();
        let opts = dialect_options();
        let root = parse_document(&arena, text, &opts);
        let (root, edited) = renest_directives(&arena, root, text, &opts, &mut Diagnostics::new());
        assert_eq!(edited, format!("- a\n  {}\n- b\n", marker(0)));
        let tight = root
            .descendants()
            .find_map(|node| match &node.data().value {
                NodeValue::List(list) => Some(list.tight),
                _ => None,
            });
        assert_eq!(tight, Some(true));
        // indented in a definition, with what follows
        let (edited, tree) = renest("Term\n\n: def\n   :::\n  more\n");
        assert_eq!(edited, format!("Term\n\n: def\n   {}\n  more\n", marker(1)));
        assert!(tree.contains('~') && tree.ends_with("more"), "{tree}");
        // a run of them takes about log2(n) rounds
        let text = "> :::\n".repeat(1000);
        let (edited, _) = renest(&text);
        assert_eq!(edited, format!("> {}\n", marker(0)).repeat(1000));
    }

    #[test]
    fn a_line_that_is_code_once_the_line_above_is_ignored_keeps_its_text() {
        // comrak's unnamed directive ends at the second `:::`, and the code block in it with
        // it; once the first `:::` is ignored, the code block runs on, and the second is code
        let text = "w\n\n   :::\n~~~\n   :::\nTerm\n";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(
            edited,
            format!("w\n\n   {}\n~~~\n   :::\nTerm\n", marker(3))
        );
        assert_eq!(tree, "w ~ code");
        assert_eq!(warnings, []);
        // inside a directive, where the second one closes it
        let (edited, tree) = renest(":::a\nx\n:::\n:::\n~~~\n:::\n");
        assert_eq!(edited, format!(":::a\nx\n:::\n{}\n~~~\n:::\n", marker(0)));
        assert_eq!(tree, "a[x] ~ code");
        // in a block quote, where both bare lines are marked in one round
        let text = "> :::\n> ~~~\n> :::\n> :::\n";
        let (edited, tree) = renest(text);
        assert_eq!(edited, format!("> {}\n> ~~~\n> :::\n> :::\n", marker(0)));
        assert_eq!(tree, ">[~ code]");
    }

    #[test]
    fn a_marker_reads_back() {
        for fence in [
            IgnoredFence {
                colons: 3,
                offset: 0,
                odd: false,
            },
            IgnoredFence {
                colons: 12,
                offset: 3,
                odd: true,
            },
        ] {
            let comment = fence.comment();
            assert_eq!(IgnoredFence::from_html(&comment), Some(fence));
            // as comrak gives an HTML block: indented, and with the line's end
            assert_eq!(
                IgnoredFence::from_html(&format!("  {comment}\n")),
                Some(fence)
            );
        }
        for literal in [
            "<!--presenter:ignored-fence colons=3-->",
            "<!--presenter:ignored-fence colons=3 offset=0 odd odd-->",
            "<!--presenter:ignored-fence colons=x offset=0-->",
            "<!--presenter:ignored-fence colons=3 offset=0--> x",
            "<!-- a comment -->",
        ] {
            assert_eq!(IgnoredFence::from_html(literal), None, "{literal}");
        }
    }

    #[test]
    fn the_tree_has_the_fences_as_written() {
        // the warnings compare fence lengths, and say what follows the colons
        let directives = |text: &str| {
            let arena = Arena::new();
            let opts = dialect_options();
            let root = parse_document(&arena, text, &opts);
            let (root, edited) =
                renest_directives(&arena, root, text, &opts, &mut Diagnostics::new());
            (edited != text, infos(root))
        };
        let owned = |found: &[(&str, usize)]| -> Vec<(String, usize)> {
            found
                .iter()
                .map(|&(info, length)| (info.to_string(), length))
                .collect()
        };
        // rewritten: `::::note` … `::::`, and a marker for the odd line
        assert_eq!(
            directives(":::note\n:::small\nx\n:::\n:::&#32;\ny\n:::\n"),
            (true, owned(&[("note", 3), ("small", 3)]))
        );
        assert_eq!(
            directives("::::fragment\nA\n:::&#9;\n::::\n"),
            (true, owned(&[("fragment", 4)]))
        );
        assert_eq!(
            directives(":::fragment\nA\n:::\n:::\n:::\u{a0}\n"),
            (true, owned(&[("fragment", 3)]))
        );
        assert_eq!(
            directives("\u{feff}- ::::note\n  x\n"),
            (false, owned(&[("note", 4)]))
        );
        // a directive without a name, as comrak reads it after the markers of a container
        // and a BOM on the line (no markers here)
        for text in [
            "> :::&#32;\n> x\n",
            "- :::&#32;\n  x\n",
            "a[^1]\n\n[^1]: :::&#32;\n    x\n",
            "Term\n\n: :::&#32;\n  x\n",
            "\u{feff}> :::&#32;\n",
            "\u{feff}:::&#32;\n",
        ] {
            let arena = Arena::new();
            let root = parse_document(&arena, text, &dialect_options());
            as_written(root, text);
            assert_eq!(infos(root), owned(&[(" ", 3)]), "{text:?}");
            assert_eq!(directives(text), (true, Vec::new()), "{text:?}");
        }
    }

    /// The info and fence length of every directive in `root`.
    fn infos<'a>(root: &'a AstNode<'a>) -> Vec<(String, usize)> {
        root.descendants()
            .filter_map(|node| match &node.data().value {
                NodeValue::BlockDirective(directive) => {
                    Some((directive.info.clone(), directive.fence_length))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_fences_as_written_survive_a_parse_with_attribute_blocks_rewritten() {
        // recover_attr_blocks parses the text again, with comrak's lengths and infos
        let text = "# T {background='#222'}\n\n:::note\n:::small\nx\n:::\n:::&#32;\ny\n:::\n";
        let arena = Arena::new();
        let opts = dialect_options();
        let root = parse_document(&arena, text, &opts);
        let mut diagnostics = Diagnostics::new();
        let (root, nested) = renest_directives(&arena, root, text, &opts, &mut diagnostics);
        let (root, recovered) = crate::markdown::recover::recover_attr_blocks(
            &arena,
            root,
            &nested,
            &opts,
            &mut diagnostics,
        );
        assert!(matches!(recovered, Cow::Owned(_)));
        assert_ne!(infos(root)[0], ("note".to_string(), 3));
        as_written(root, text);
        let expected: Vec<(String, usize)> = [("note", 3), ("small", 3)]
            .iter()
            .map(|&(info, length)| (info.to_string(), length))
            .collect();
        assert_eq!(infos(root), expected);
        // and so does the marker
        assert_eq!(tree(root), "#T note[small[x] ~ y]");
    }

    #[test]
    fn tab_indented_fences() {
        // a tab is four columns: inside the directive, the indented `:::` is code
        let (_, tree) = renest(":::fragment\n:::small\nA\n:::\n\t:::\n:::\n");
        assert_eq!(tree, "fragment[small[A] code]");
        let (_, tree) = renest(":::fragment\n \t:::small\nA\n:::\n:::\n");
        assert_eq!(tree, "fragment[code A] ~");
    }

    #[test]
    fn lines_keep_their_numbers() {
        let text = "# T\n\n:::fragment\n:::small\nA\n:::\n:::\n\n---\n\n\
                    # U\r\n:::a\r\n:::b\r\nB\r\n:::\r\n:::\r\n:::\r\nC\r\n";
        let (edited, _) = renest(text);
        assert_eq!(edited.lines().count(), text.lines().count());
        assert!(
            edited.contains(&format!(
                "::::a\r\n:::b\r\nB\r\n:::\r\n::::\r\n{}\r\nC\r\n",
                marker(0)
            )),
            "{edited:?}"
        );
    }

    #[test]
    fn directives_outside_the_tree_are_left_alone() {
        // inside a list item or a block quote comrak's reading stays, also for fences of one
        // length that the dialect would read otherwise at the top level
        let (edited, tree, warnings) = renest_warned("- item\n\n  :::small\n  A\n  :::\n");
        assert_eq!(edited, "- item\n\n  :::small\n  A\n  :::\n");
        assert_eq!(tree, "item small[A]");
        assert_eq!(warnings, []);
        // the first `:::` closes the columns, and the last one, which closes nothing there,
        // is ignored (the renderer says so)
        for (text, ignored) in [
            (
                "- item\n\n  :::columns\n  :::column\n  A\n  :::\n  :::column\n  B\n  :::\n  :::\n",
                format!("  {}\n", marker(0)),
            ),
            (
                "> :::columns\n> :::column\n> A\n> :::\n> :::column\n> B\n> :::\n> :::\n",
                format!("> {}\n", marker(0)),
            ),
        ] {
            let (edited, tree, warnings) = renest_warned(text);
            let kept = text.trim_end_matches(":::\n").trim_end_matches([' ', '>']);
            assert_eq!(edited, format!("{kept}{ignored}"), "{text}");
            assert!(tree.contains("columns[column[A]] column[B] ~"), "{tree}");
            assert_eq!(warnings, []);
        }
    }

    #[test]
    fn a_stray_fence_is_ignored() {
        let text = ":::fragment\nA\n:::\n:::\nB\n";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(edited, format!(":::fragment\nA\n:::\n{}\nB\n", marker(0)));
        assert_eq!(tree, "fragment[A] ~ B");
        assert_eq!(warnings, []);
        // indented
        let (edited, tree) = renest(":::fragment\nA\n:::\n  :::  \nB\n");
        assert_eq!(edited, format!(":::fragment\nA\n:::\n  {}\nB\n", marker(2)));
        assert_eq!(tree, "fragment[A] ~ B");
    }

    #[test]
    fn named_directives_that_nothing_closes_are_reported() {
        let text = ":::notes\nA\n\n:::fragment\nB\n";
        let (edited, tree, warnings) = renest_warned(text);
        assert_eq!(edited, text);
        assert_eq!(tree, "notes[A fragment[B]]");
        assert_eq!(
            warnings.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            [1, 4]
        );
    }

    /// A small deterministic generator (64-bit LCG), as in `attrs.rs`.
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

    /// Writes random content (paragraphs and directives, up to `depth` levels) to `text`,
    /// every directive closed by a fence as long as its opening one, and returns its tree.
    fn random_content(rng: &mut Lcg, depth: usize, text: &mut String) -> String {
        let mut parts = Vec::new();
        for _ in 0..1 + rng.below(3) {
            if depth > 0 && rng.below(3) > 0 {
                let name = ["columns", "column", "small", "fragment", "center"][rng.below(5)];
                let fence = ":".repeat(3 + rng.below(2) * rng.below(3));
                text.push_str(&format!("{fence}{name}\n"));
                let inner = random_content(rng, depth - 1, text);
                text.push_str(&format!("{fence}\n"));
                parts.push(format!("{name}[{inner}]"));
            } else {
                let word = format!("w{}", rng.below(100));
                text.push_str(&format!("{word}\n\n"));
                parts.push(word);
            }
        }
        parts.join(" ")
    }

    /// Whatever the fence lengths, directives whose fences pair up read as written, on one
    /// slide or on several, with no warning.
    #[test]
    fn paired_fences_always_nest_as_written() {
        let mut rng = Lcg(0x6e57);
        let mut rewritten = 0;
        for case in 0..3_000 {
            let mut text = String::new();
            let mut expected = Vec::new();
            for slide in 0..1 + case % 3 {
                if slide > 0 {
                    text.push_str("---\n\n");
                    expected.push("---".to_string());
                }
                expected.push(random_content(&mut rng, 4, &mut text));
            }
            let expected = expected.join(" ");
            let (edited, tree, warnings) = renest_warned(&text);
            assert_eq!(tree, expected, "{text}");
            assert_eq!(warnings, [], "{text}");
            assert_eq!(edited.lines().count(), text.lines().count());
            rewritten += usize::from(edited != text);
        }
        assert!(
            rewritten > 500,
            "only {rewritten} texts needed longer fences"
        );
    }

    #[test]
    fn readings() {
        let fence = |line, colons, bare| Fence {
            line,
            colons,
            indent: 0,
            at: 0,
            bare,
            named: !bare,
            content: false,
        };
        // columns(4) column(3) small(3) ::: ::: ::::
        let fences = [
            fence(1, 4, false),
            fence(2, 3, false),
            fence(3, 3, false),
            fence(4, 3, true),
            fence(5, 3, true),
            fence(6, 4, true),
        ];
        let open = |parent| Role::Open { parent };
        let close = |target| Role::Close { target };
        let outer = read(&fences, Rule::Outermost);
        assert_eq!(
            outer.roles,
            [
                open(None),
                open(Some(0)),
                open(Some(1)),
                close(1),
                open(Some(0)),
                close(0)
            ]
        );
        let inner = read(&fences, Rule::Innermost);
        assert_eq!(
            inner.roles,
            [
                open(None),
                open(Some(0)),
                open(Some(1)),
                close(2),
                close(1),
                close(0)
            ]
        );
        assert_eq!(unclosed(&fences, &inner.roles), []);
        let changed = changed(&inner.roles, &outer.roles);
        assert_eq!(changed, [true; 6]);
        assert_eq!(
            fence_lengths(&fences, &inner.roles, &changed),
            [5, 4, 3, 3, 4, 5]
        );
        // without the second `:::`: the `::::` closes the columns and the column with them
        let fences = [fences[0], fences[1], fences[2], fences[3], fences[5]];
        let inner = read(&fences, Rule::Innermost);
        assert_eq!(
            inner.roles,
            [open(None), open(Some(0)), open(Some(1)), close(2), close(0)]
        );
        assert_eq!(
            unclosed(&fences, &inner.roles),
            [Unclosed {
                line: 2,
                colons: 3,
                closed_by: Some((4, 1)),
            }]
        );
        // a `:::` with nothing to close is ignored, with its indentation inside the
        // directive around it; nothing closes that one
        let indented = Fence {
            indent: 2,
            ..fence(3, 3, true)
        };
        let fences = [fence(1, 4, false), fence(2, 3, true), indented];
        let inner = read(&fences, Rule::Innermost);
        assert_eq!(
            inner.roles,
            [
                open(None),
                Role::Ignore { offset: 0 },
                Role::Ignore { offset: 2 }
            ]
        );
        assert_eq!(
            read(&fences, Rule::Outermost).roles,
            [open(None), open(Some(0)), close(1)]
        );
        assert_eq!(
            unclosed(&fences, &inner.roles),
            [Unclosed {
                line: 1,
                colons: 4,
                closed_by: None,
            }]
        );
    }
}
