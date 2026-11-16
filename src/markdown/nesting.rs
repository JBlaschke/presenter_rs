//! Nested directives with fences of one length, `:::` lines that close nothing, and
//! directives in containers (dialect §4, contract changes 2026-11-14 and 2026-11-15).
//!
//! comrak 0.56 gives a closing fence to the *outermost* open directive whose fence is not
//! longer than it, and closes everything inside that directive with it, even from inside a
//! list item or definition (it looks for a directive's closing fence before it looks at what
//! is inside the directive); a bare `:::` line that closes nothing opens an unnamed
//! directive. In
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
//! where a closing fence closes the innermost open directive, and the dialect reads them so,
//! in the document and in every list item, block quote, footnote and definition alike: a
//! closing fence closes one directive opened in its own container, and a `:::` line that
//! closes none is ignored. [`renest_directives`] makes comrak read them that way:
//!
//! 1. **scopes**: the document and each list item, block quote, footnote and definition of
//!    comrak's tree is a scope ([`Scopes`]). A fence line is in the innermost one whose
//!    lines include it, and its indentation counts from where that container's content
//!    starts on the line ([`Scopes::content`]). A line in a container that does not continue
//!    its prefixes continues a paragraph in it lazily, and is text of it (or of the heading,
//!    table or link reference definition that comrak makes of it), and no fence, as in comrak
//!    ([`Scopes::place`]). A line that comrak took as the closing fence of a
//!    directive around a list item or definition is in that container when the line
//!    continues it ([`Scopes::culprit`]): comrak ended the container there, and the dialect
//!    does not;
//! 2. **fences**: the directives comrak found are collected with the lines of their opening
//!    and closing fences, and with comrak's roles ([`collect`]). Every other bare `:::` line
//!    as written is *content* in comrak's tree (of a code block, an HTML block, …): it closes
//!    a directive of its scope where one it can close is open, as in comrak, which looks for
//!    closing fences before it looks at what is inside a directive, and stays as written
//!    otherwise ([`Role::Text`]);
//! 3. **reading**: the fences are read in line order ([`read`]). A closing fence closes, of
//!    the directives open in its scope that it can close (fence not longer than it, fence
//!    line indented little enough), the innermost one among those with the longest fence.
//!    `::::` around `:::` blocks therefore reads as before, and `:::` inside `:::` closes
//!    the inner block. A fence without a name that can close none is ignored
//!    ([`Role::Ignore`]): a bare `:::`, or a `:::` followed only by something that reads as
//!    nothing (a no-break space, `&#32;`), which never closes one. A directive that no fence
//!    of its own closes ends with the directive around it in its scope, with its container,
//!    or at the end of the text, and is an `invalid-directive` warning ([`Unclosed`]);
//! 4. **rewrite**: fences are made longer (never shorter) until comrak reads them as the
//!    dialect does ([`fence_lengths`]): every directive's fence becomes longer than the
//!    fences of the directives inside it and than the bare lines left as content in the
//!    containers inside it, and every closing fence as long as the fence of the directive it
//!    closes; an outermost directive that comrak already reads so keeps its fences
//!    ([`changed`]). An ignored line becomes a marker from its colons on ([`IgnoredFence`]):
//!    an HTML comment, which ends a paragraph and keeps a list tight as the fence did, and
//!    which [`report_ignored_fences`] reports, and the renderer and the slide splitter skip.
//!    The text is parsed again. When that changes which lines are fences (a `:::` that
//!    comrak's reading took as a closing fence inside a code block is code again; an
//!    indented fence that its early close left as an indented code block is a fence again),
//!    the new fence lines are read again, with the ignored ones and, when the first line
//!    where the new tree's fences are not those meant is a fence not meant, the fences meant
//!    past it that the tree hides (a code block that this misreading opens may hide them).
//!    When it changes which containers the fences are in (a list item that a `:::`
//!    ended in comrak's reading goes on past it), the new tree's fences are read again;
//!    when that tree still has such a fence (comrak's reading of a list item's `:::` ends
//!    the item, and a code block the item held runs on and hides the next item's fences),
//!    the next rewrite makes every bare fence from that line on a marker, so that the tree
//!    after it shows the containers and code blocks as written. All as often as
//!    [`REPARSE_BUDGET`] allows, which counts the bytes parsed and the lines of the
//!    directives in their trees. An ignored line before that first line whose marker is not an
//!    HTML block of its own (a `:::` that closed the unnamed directive around a code block,
//!    in which it is now) is no fence, and keeps its text. When comrak's tree is not the
//!    dialect's reading after all, or the fence lines do not settle, the original text and
//!    tree are kept, and so they are when comrak's reading or the dialect's nests
//!    directives more than [`MAX_DEPTH`] deep: an `invalid-directive` warning says so
//!    ([`Fallback`]);
//! 5. **strays in a text left as it was**: a `:::` line that comrak still reads as opening an
//!    unnamed directive becomes a marker too, and the text is parsed again, until none is
//!    left ([`mark_strays`], as often as [`attempts`] allows, since a marker frees the `:::`
//!    that closed the unnamed directive, which may then open one, or lands in a code block
//!    that directive no longer ends, and gets its text back; when they do not settle, a
//!    warning says so).
//!
//! Only colons are added to fence lines, and ignored lines are replaced from their colons
//! on, so every line keeps its number, every container keeps its prefixes, and slide
//! boundaries do not move.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use comrak::arena_tree::NodeEdge;
use comrak::nodes::{AstNode, NodeHeading, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::error::Diagnostics;
use crate::ir::SourceSpan;
use crate::markdown::recover::split_lines;
use crate::markdown::render::{directive_label, report_ignored_fences};

/// How many times the text may be parsed again at least and at most (module docs, steps 4
/// and 5): as often as [`REPARSE_BUDGET`] allows within these bounds. A parse of the
/// rewritten text may uncover fences that comrak's reading hid in a code block (a `:::` that
/// closes a directive in comrak's reading ends a code block early, and the code block's
/// closing fence then opens one); the fences meant that it still hides are read again with
/// those it shows, and in list items, the rewrite after a tree that still has a fence that
/// ends one early makes every bare fence from there on a marker, so that a chain of such code
/// blocks takes a few parses, not one each. (Dozens of code blocks interleaved with `:::`
/// lines may still take one parse each; the upper bound keeps short the time that a small
/// text whose readings do not settle takes.)
const ATTEMPTS: std::ops::RangeInclusive<usize> = 4..=1024;

/// How many bytes of rewritten text [`renest`] and [`mark_strays`] each parse at most, beyond
/// [`ATTEMPTS`]' lower bound. For [`renest`], the lines of the directives in a parse's tree
/// count too ([`DIRECTIVE_LINES_PER_BYTE`]).
const REPARSE_BUDGET: usize = 16 << 20;

/// How many lines of directives count as one byte of [`REPARSE_BUDGET`] (a line in `n`
/// directives counts `n` times): comrak checks every directive open on a line for its closing
/// fence, so that a text whose tree nests deeply takes longer to parse than its bytes say. (A
/// rewrite that makes markers of the fences from a line on may leave every directive past it
/// open, thousands deep.)
const DIRECTIVE_LINES_PER_BYTE: usize = 4;

/// How deeply directives may nest, in comrak's reading or the dialect's, for the fences to
/// be rewritten: the outermost fence of `n` nested directives needs `n + 2` colons, so the
/// rewritten text grows with the square of the depth (and comrak's time with its cube).
const MAX_DEPTH: usize = 32;

/// How a marker ([`IgnoredFence`]) starts.
const MARKER: &str = "<!--presenter:ignored-fence";

/// The columns of a tab stop, as comrak counts them.
const TAB_STOP: usize = 4;

/// A fence line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence {
    /// 1-based line.
    line: usize,
    /// Number of colons as written.
    colons: usize,
    /// Columns before the colons, from where the content of its scope starts on the line (a
    /// tab advances to the next multiple of four columns of the line).
    indent: usize,
    /// Byte offset of the colons in the line.
    at: usize,
    /// Whether the line is only the fence (`:::`, then spaces or tabs): only such a line
    /// closes a directive.
    bare: bool,
    /// Whether the directive it opens has a name or attributes (comrak's info, trimmed).
    named: bool,
    /// Whether the line is content in comrak's tree (of a code block, an HTML block, …)
    /// rather than a fence: a bare fence that closes a directive of its scope where one it
    /// can close is open (comrak looks for closing fences before it looks at what is inside
    /// a directive), and stays what it is otherwise ([`Role::Text`]).
    content: bool,
    /// The scope it is in ([`Scopes`]).
    scope: usize,
}

/// What a fence does in a reading. Directives are named by the index of their opening fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Opens a directive inside `parent`, a directive of the same scope (`None`: at the top
    /// of its scope).
    Open { parent: Option<usize> },
    /// Closes `target` (and the directives still open inside it).
    Close { target: usize },
    /// Closes nothing and opens nothing (a fence without a name that can close no open
    /// directive of its scope): the line becomes a marker. `offset` is its indentation after
    /// the columns that the directives around it in its scope strip (comrak's `fence_offset`
    /// for a directive opened there).
    Ignore { offset: usize },
    /// A line of content ([`Fence::content`]) that closes nothing: it stays as written.
    Text,
}

/// A directive open while the fences are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    /// Its opening fence.
    fence: usize,
    colons: usize,
    /// Columns stripped before this directive's fences are looked at.
    base: usize,
    /// The scope it is opened in.
    scope: usize,
}

/// The dialect's reading of the fences ([`read`]). A `deep` reading nests directives more
/// than [`MAX_DEPTH`] deep: it stops at the fence that does, so it holds the roles up to that
/// one only, and it is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reading {
    /// Each fence's role.
    roles: Vec<Role>,
    /// For each fence, the outermost directive open around it, or that it opens: the
    /// directives of one group are rewritten together ([`changed`]).
    groups: Vec<Option<usize>>,
    /// For each fence, the innermost directive open around it in another scope (around the
    /// container the fence is in): its fence must be longer than this one's
    /// ([`fence_lengths`]).
    outer: Vec<Option<usize>>,
    /// The directives that no fence of their own closes, in line order.
    unclosed: Vec<Unclosed>,
    deep: bool,
}

/// What a scope is ([`Scopes`]): the document, or the container a directive opened in it
/// ends with at the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Document,
    /// A list item or task item.
    Item,
    /// A block quote or alert.
    Quote,
    Footnote,
    /// The details of a description list item.
    Definition,
}

impl Kind {
    /// The kind of scope a node of `value` is, if any.
    pub(crate) fn of(value: &NodeValue) -> Option<Self> {
        match value {
            NodeValue::Document => Some(Self::Document),
            NodeValue::Item(_) | NodeValue::TaskItem(_) => Some(Self::Item),
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) | NodeValue::Alert(_) => {
                Some(Self::Quote)
            }
            NodeValue::FootnoteDefinition(_) => Some(Self::Footnote),
            NodeValue::DescriptionDetails => Some(Self::Definition),
            _ => None,
        }
    }

    /// What an author calls it in a warning.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Item => "list item",
            Self::Quote => "block quote",
            Self::Footnote => "footnote",
            Self::Definition => "definition",
        }
    }
}

/// What ends a directive that no fence of its own closes ([`Unclosed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosedBy {
    /// The fence that closes a directive around it in its scope: that fence's colons as
    /// written, and the line where that directive opens.
    Outer { colons: usize, line: usize },
    /// The end of the container it is opened in.
    Container(Kind),
    /// The end of the text.
    End,
}

/// A directive that no fence of its own closes (module docs, step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unclosed {
    /// The line of its opening fence.
    line: usize,
    /// The colons of its opening fence, as written.
    colons: usize,
    closed_by: ClosedBy,
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
/// separators from the text). A directive that no fence of its own closes is an
/// `invalid-directive` warning in `diagnostics`, on its opening line.
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
    // where comrak's reading has its first directive, before the strays are marked
    let unread = unclosed
        .as_ref()
        .err()
        .map(|&fallback| (fallback, fallback_line(root, text, fallback)));
    let (root, edited, strays) = mark_strays(arena, root, edited, text, opts);
    as_written(root, text);
    match unclosed {
        Ok(unclosed) => report_unclosed(root, &unclosed, diagnostics),
        Err(_) => {
            if let Some((fallback, line)) = unread {
                report_fallback(line, fallback, diagnostics);
            }
        }
    }
    if let Err(fallback) = strays {
        report_fallback(fallback_line(root, text, fallback), fallback, diagnostics);
    }
    // the scopes of the tree, for the directive around a footnote definition (built when a
    // warning needs one)
    let mut scopes: Option<Scopes<'a>> = None;
    let mut around_footnote = |footnote: &'a AstNode<'a>| {
        let scopes = scopes
            .get_or_insert_with(|| Scopes::new(root, &text_lines(&edited).1, bom_length(text)));
        let path = &scopes.paths[scopes.id(footnote)?];
        path.iter().rev().find_map(|node| match &node.data().value {
            NodeValue::BlockDirective(directive) => Some(directive_label(&directive.info)),
            _ => None,
        })
    };
    report_ignored_fences(root, diagnostics, &mut around_footnote);
    (root, edited)
}

/// The length of the byte order mark that `text` starts with, which comrak's columns count.
fn bom_length(text: &str) -> usize {
    text.len()
        - text
            .trim_start_matches('\u{feff}')
            .len()
            .max(text.len().saturating_sub(3))
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

/// Steps 1 to 4 of the module docs: the tree and its text (unchanged, or rewritten and
/// parsed again), and the directives that no fence of their own closes, or why the text is
/// left as comrak reads it.
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
    if !text.contains(":::") {
        return unchanged(Ok(Vec::new()));
    }
    let (starts, lines) = text_lines(text);
    let bom = bom_length(text);
    // the lines that may hold a fence, after the prefixes of their containers
    let candidates: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(":::"))
        .map(|(index, _)| index + 1)
        .collect();
    // the tree the fence lines are taken from, its text, its scopes and its fences: the
    // original, then the last rewrite's when comrak found other fence lines in it, or put
    // them in other containers
    let (mut tree, mut tree_text) = (root, Cow::Borrowed(text));
    let mut current: Option<(Scopes<'a>, Collected)> = None;
    // the fences that the last rewrite made markers of, which are not in its tree
    let mut ignored: Vec<Fence> = Vec::new();
    // the fences of the last reading that its rewrite's tree hides, past the first line where
    // that tree has other fences than those meant when that line is a fence not meant
    let mut carried: Vec<Fence> = Vec::new();
    // the lines of the fences of every group rewritten so far: the next rewrite starts from
    // the text as written again, and must rewrite them again
    let mut rewritten: HashSet<usize> = HashSet::new();
    // what the parses so far cost of the budget
    let mut spent = 0;
    for attempt in 0..*ATTEMPTS.end() {
        if attempt >= *ATTEMPTS.start() && spent >= REPARSE_BUDGET {
            break;
        }
        let (scopes, found) = match current.take() {
            Some(current) => current,
            None => {
                let (_, tree_lines) = text_lines(&tree_text);
                let scopes = Scopes::new(tree, &tree_lines, bom);
                let Some(found) = collect(&scopes, &tree_lines) else {
                    tracing::debug!("directive fences: a fence is not where comrak's tree has it");
                    return unchanged(Err(Fallback::Unread));
                };
                (scopes, found)
            }
        };
        if attempt == 0 && found.depth > MAX_DEPTH {
            tracing::debug!("directive fences: nested more than {MAX_DEPTH} deep; left as is");
            return unchanged(Err(Fallback::Deep));
        }
        let placed = scopes.place(&candidates, &lines);
        let earlier: Vec<Fence> = ignored.drain(..).chain(carried.drain(..)).collect();
        let Some((fences, comrak)) = fences_to_read(&found, &earlier, &placed, &lines) else {
            tracing::debug!("directive fences: a fence is not as written; left as is");
            return unchanged(Err(Fallback::Unread));
        };
        let dialect = read(&fences, &scopes);
        if dialect.deep {
            tracing::debug!("directive fences: nested more than {MAX_DEPTH} deep; left as is");
            return unchanged(Err(Fallback::Deep));
        }
        let changed = changed(&fences, &dialect, &comrak, &rewritten);
        rewritten.extend(
            fences
                .iter()
                .zip(&changed)
                .filter(|&(_, &changed)| changed)
                .map(|(fence, _)| fence.line),
        );
        let colons = fence_lengths(&fences, &dialect, &changed);
        // In a rewrite's tree, a fence that closes a directive in comrak's reading from inside
        // a container in the directive's scope ended that container early again, and the
        // tree past it may be misread (a code block that the container held may run on and
        // hide the next one's fences: one parse per container). The next rewrite then makes
        // every bare fence from there on a marker, so that its tree shows the containers and
        // code blocks as written, for the reading after it.
        let cut = (attempt > 0)
            .then(|| {
                fences
                    .iter()
                    .zip(&comrak)
                    .filter(|&(fence, role)| {
                        matches!(*role, Role::Close { target } if fences[target].scope != fence.scope)
                    })
                    .map(|(fence, _)| fence.line)
                    .min()
            })
            .flatten();
        let muted: Vec<Role>;
        let roles = match cut {
            Some(cut) => {
                muted = fences
                    .iter()
                    .zip(&dialect.roles)
                    .map(|(fence, &role)| match role {
                        _ if fence.bare && fence.line >= cut => Role::Ignore {
                            offset: fence.indent,
                        },
                        role => role,
                    })
                    .collect();
                &muted
            }
            None => &dialect.roles,
        };
        let Some(edited) = rewrite(text, &starts, &fences, roles, &colons) else {
            // comrak reads the text as the dialect does (known for the original tree only)
            if attempt == 0 && dialect.roles == comrak {
                return unchanged(Ok(dialect.unclosed));
            }
            tracing::debug!("directive fences: the fence lines do not settle; left as is");
            return unchanged(Err(Fallback::Unread));
        };
        let new_root = parse_document(arena, &edited, opts);
        let (_, new_lines) = text_lines(&edited);
        let new_scopes = Scopes::new(new_root, &new_lines, bom);
        let Some(new_found) = collect(&new_scopes, &new_lines) else {
            tracing::debug!("directive fences: a fence is not where comrak's tree has it");
            return unchanged(Err(Fallback::Unread));
        };
        spent += edited.len() + new_found.area / DIRECTIVE_LINES_PER_BYTE;
        let expected = without_ignored(&fences, &dialect.roles, &scopes);
        let new_fence_lines: Vec<usize> = new_found.fences.iter().map(|fence| fence.line).collect();
        // comrak reads the rewritten text as meant up to the first line where its fences
        // differ; when that line is a fence not meant, a code block that this misreading
        // opens may hide fences meant past it, which the next reading takes too, so that the
        // next rewrite does not undo them (one misreading after the other would otherwise
        // take one parse each)
        let first = first_difference(&expected.lines, &new_fence_lines);
        let found_first = first.filter(|line| expected.lines.binary_search(line).is_err());
        let hidden = |line: usize| found_first.is_some_and(|first| line > first);
        // the ignored lines whose markers are HTML blocks of their own; one that is not (in a
        // code block or an HTML block that no closing fence ends now) is no fence in the
        // dialect's reading, and the next rewrite leaves its text as written, unless it may be
        // hidden
        let markers = marker_lines(new_root);
        if cut.is_some() {
            // the markers that are HTML blocks of their own are fences in the next reading,
            // and the others content (of the code block or HTML block they are in)
            ignored = fences
                .iter()
                .zip(roles)
                .filter(|&(fence, role)| {
                    matches!(role, Role::Ignore { .. }) && markers.contains(&fence.line)
                })
                .map(|(fence, _)| Fence {
                    content: false,
                    ..*fence
                })
                .collect();
            tree = new_root;
            tree_text = Cow::Owned(edited);
            current = Some((new_scopes, new_found));
            continue;
        }
        let mut misplaced = false;
        ignored = fences
            .iter()
            .zip(&dialect.roles)
            .filter(|(_, role)| matches!(role, Role::Ignore { .. }))
            .map(|(fence, _)| *fence)
            .filter(|fence| {
                let placed = markers.contains(&fence.line);
                misplaced |= !placed;
                placed || hidden(fence.line)
            })
            .collect();
        if !misplaced && first.is_none() {
            let new_keys: Vec<(usize, usize)> = new_found
                .fences
                .iter()
                .map(|fence| new_scopes.keys[fence.scope])
                .collect();
            if new_found.roles == expected.roles
                && new_keys == expected.keys
                && same_places(
                    &fences,
                    &dialect.roles,
                    &scopes,
                    &new_scopes,
                    &candidates,
                    &lines,
                )
            {
                return (new_root, Cow::Owned(edited), Ok(dialect.unclosed));
            }
            if edited == *tree_text {
                // the next reading would be this one
                tracing::debug!(
                    "directive fences: comrak does not read the rewritten text as meant"
                );
                return unchanged(Err(Fallback::Unread));
            }
            // the fences are in other containers than this reading assumed (a list item that
            // a `:::` ended in comrak's reading goes on past it): the new tree is read again
        } else {
            carried = fences
                .iter()
                .zip(&dialect.roles)
                .filter(|&(fence, role)| {
                    !fence.content
                        && matches!(role, Role::Open { .. } | Role::Close { .. })
                        && hidden(fence.line)
                        && new_fence_lines.binary_search(&fence.line).is_err()
                })
                .map(|(fence, _)| *fence)
                .collect();
        }
        tree = new_root;
        tree_text = Cow::Owned(edited);
        current = Some((new_scopes, new_found));
    }
    tracing::debug!("directive fences: the fence lines do not settle; left as is");
    unchanged(Err(Fallback::Unread))
}

/// Whether the lines of `fences` that `roles` ignores or leaves as content are where the
/// reading had them in the scopes `scopes` also in `new` (the scopes of the rewritten text),
/// and `new` puts no other bare fence among `candidates` (lines of `lines`, the text as
/// written): a fence that closed containers early in comrak's reading may leave the lines
/// after it outside them in its tree, and the reading of those lines then holds only if the
/// rewritten text leaves them there too.
fn same_places(
    fences: &[Fence],
    roles: &[Role],
    scopes: &Scopes,
    new: &Scopes,
    candidates: &[usize],
    lines: &[&str],
) -> bool {
    let placed = new.place(candidates, lines);
    let place = |line: usize| {
        placed
            .binary_search_by_key(&line, |fence| fence.line)
            .ok()
            .map(|index| placed[index])
    };
    let read: HashSet<usize> = fences.iter().map(|fence| fence.line).collect();
    fences.iter().zip(roles).all(|(fence, role)| match role {
        Role::Ignore { .. } | Role::Text => place(fence.line).is_some_and(|now| {
            new.keys[now.scope] == scopes.keys[fence.scope] && now.indent == fence.indent
        }),
        Role::Open { .. } | Role::Close { .. } => true,
    }) && placed
        .iter()
        .all(|fence| !fence.bare || read.contains(&fence.line))
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

/// A position in a line, as comrak tracks it while it reads the prefixes of containers: a
/// byte, and the column there (tabs advance to the next multiple of [`TAB_STOP`]). The column
/// may lie inside a tab at the byte, which then counts the rest of its columns only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Cursor {
    byte: usize,
    column: usize,
}

/// The first byte from `at` on that is not a space or tab, and the columns before it.
fn nonspace(line: &str, at: Cursor) -> (Cursor, usize) {
    let mut cursor = at;
    loop {
        match line.as_bytes().get(cursor.byte) {
            Some(b' ') => cursor.column += 1,
            Some(b'\t') => cursor.column += TAB_STOP - cursor.column % TAB_STOP,
            _ => break,
        }
        cursor.byte += 1;
    }
    (cursor, cursor.column - at.column)
}

/// `at` moved on by `columns` columns, as comrak advances over a prefix: one column for a
/// byte that is not a tab, and a tab that it goes only partly into stays where it is.
fn advance(line: &str, at: Cursor, mut columns: usize) -> Cursor {
    let mut cursor = at;
    while columns > 0 {
        match line.as_bytes().get(cursor.byte) {
            Some(b'\t') => {
                let width = TAB_STOP - cursor.column % TAB_STOP;
                if width > columns {
                    cursor.column += columns;
                    break;
                }
                cursor.column += width;
                columns -= width;
            }
            Some(_) => {
                cursor.column += 1;
                columns -= 1;
            }
            None => break,
        }
        cursor.byte += 1;
    }
    cursor
}

/// `at` moved on by `bytes` bytes (a tab to the next tab stop).
fn advance_bytes(line: &str, at: Cursor, bytes: usize) -> Cursor {
    let mut cursor = at;
    for _ in 0..bytes {
        match line.as_bytes().get(cursor.byte) {
            Some(b'\t') => cursor.column += TAB_STOP - cursor.column % TAB_STOP,
            Some(_) => cursor.column += 1,
            None => break,
        }
        cursor.byte += 1;
    }
    cursor
}

/// `at` moved over up to `columns` columns of spaces and tabs, one at a time, as comrak
/// strips a directive's indentation from its content lines.
fn strip(line: &str, at: Cursor, columns: usize) -> Cursor {
    let mut cursor = at;
    for _ in 0..columns {
        if !matches!(line.as_bytes().get(cursor.byte), Some(b' ' | b'\t')) {
            break;
        }
        cursor = advance(line, cursor, 1);
    }
    cursor
}

/// Whether `value` is a node that holds blocks, and so may hold directives.
fn holds_blocks(value: &NodeValue) -> bool {
    matches!(
        value,
        NodeValue::Document
            | NodeValue::BlockDirective(_)
            | NodeValue::List(_)
            | NodeValue::Item(_)
            | NodeValue::TaskItem(_)
            | NodeValue::BlockQuote
            | NodeValue::MultilineBlockQuote(_)
            | NodeValue::Alert(_)
            | NodeValue::FootnoteDefinition(_)
            | NodeValue::DescriptionList
            | NodeValue::DescriptionItem(_)
            | NodeValue::DescriptionDetails
    )
}

/// The scopes of a tree (dialect §4): the document (scope 0) and its containers (list items,
/// block quotes, footnotes and definitions), in which the fences are read
/// independently ([`read`]). A footnote definition is in the container its lines are in,
/// though comrak moves it to the end of the document.
struct Scopes<'a> {
    /// The node of each.
    nodes: Vec<&'a AstNode<'a>>,
    kinds: Vec<Kind>,
    /// The scope each is in.
    parents: Vec<Option<usize>>,
    /// Where each starts (line, comrak's column; the document at `(0, 0)`), which names the
    /// same container in another tree of the text.
    keys: Vec<(usize, usize)>,
    /// The first and last line of each ([`Scopes::end`]).
    spans: Vec<(usize, usize)>,
    /// The last line of every node of the tree ([`last_lines`]).
    ends: HashMap<*const AstNode<'a>, usize>,
    /// The lines that continue a paragraph (its lines after the first), as ranges in order
    /// that do not overlap.
    continued: Vec<(usize, usize)>,
    /// The nodes from the document down to each, whose prefixes its lines continue.
    paths: Vec<Vec<&'a AstNode<'a>>>,
    /// For a list item, the columns its content lines are indented by from where its parent's
    /// content starts (comrak's `marker_offset + padding`, which a task item no longer has).
    rels: Vec<usize>,
    /// The scope of each container node.
    ids: HashMap<*const AstNode<'a>, usize>,
    /// The children of the document that hold blocks, in the order of their start (a footnote
    /// definition that comrak moved to the end of the document where its lines are).
    roots: Vec<&'a AstNode<'a>>,
    /// For each node, the footnote definitions in it ([`Scopes::ends_in_paragraph`]; built
    /// when needed).
    footnotes: std::cell::OnceCell<HashMap<*const AstNode<'a>, Vec<usize>>>,
    /// The length of the byte order mark that the first line starts with, which comrak's
    /// columns count, and the lines given to the walks do not hold.
    bom: usize,
}

impl<'a> Scopes<'a> {
    /// The scopes of `root`, a tree of the text with `lines` (without its byte order mark,
    /// `bom` bytes long).
    fn new(root: &'a AstNode<'a>, lines: &[&str], bom: usize) -> Self {
        let ends = last_lines(root);
        let mut paragraphs: Vec<(usize, usize)> = root
            .descendants()
            .filter(|node| matches!(node.data().value, NodeValue::Paragraph))
            .filter_map(|node| {
                let start = node.data().sourcepos.start.line;
                let end = ends.get(&(node as *const _)).copied()?;
                (end > start).then_some((start + 1, end))
            })
            .collect();
        paragraphs.sort_unstable();
        let mut continued: Vec<(usize, usize)> = Vec::with_capacity(paragraphs.len());
        for (start, end) in paragraphs {
            match continued.last_mut() {
                Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
                _ => continued.push((start, end)),
            }
        }
        let mut roots: Vec<&'a AstNode<'a>> = root
            .children()
            .filter(|child| holds_blocks(&child.data().value))
            .collect();
        roots.sort_by_key(|child| {
            let start = child.data().sourcepos.start;
            (start.line, start.column)
        });
        let mut scopes = Scopes {
            nodes: vec![root],
            kinds: vec![Kind::Document],
            parents: vec![None],
            keys: vec![(0, 0)],
            spans: vec![(1, usize::MAX)],
            paths: vec![vec![root]],
            rels: vec![0],
            ids: HashMap::new(),
            roots,
            footnotes: std::cell::OnceCell::new(),
            ends,
            continued,
            bom,
        };
        // in tree order, so that every container comes after the one it is in (a footnote
        // definition comes after the whole document, and after a footnote definition it is
        // in: comrak moves them in the order of their references, and they go in the order of
        // their lines)
        let mut children: Vec<&'a AstNode<'a>> = root.children().collect();
        children.sort_by_key(|child| {
            let data = child.data();
            matches!(data.value, NodeValue::FootnoteDefinition(_))
                .then_some((data.sourcepos.start.line, data.sourcepos.start.column))
        });
        let mut stack: Vec<&'a AstNode<'a>> = children.into_iter().rev().collect();
        while let Some(node) = stack.pop() {
            let data = node.data();
            if !holds_blocks(&data.value) {
                continue;
            }
            if let Some(kind) = Kind::of(&data.value) {
                let id = scopes.nodes.len();
                let start = data.sourcepos.start;
                scopes.nodes.push(node);
                scopes.kinds.push(kind);
                scopes.keys.push((start.line, start.column));
                scopes.spans.push((start.line, scopes.end(node)));
                scopes.ids.insert(node as *const _, id);
                // the nodes up to the nearest container it is in
                let mut between = vec![node];
                let mut up = scopes.parent_of(root, node, lines);
                let parent = loop {
                    let Some(next) = up else { break 0 };
                    if let Some(&parent) = scopes.ids.get(&(next as *const _)) {
                        break parent;
                    }
                    if matches!(next.data().value, NodeValue::Document) {
                        break 0;
                    }
                    between.push(next);
                    up = scopes.parent_of(root, next, lines);
                };
                let mut path = scopes.paths[parent].clone();
                path.extend(between.into_iter().rev());
                if kind == Kind::Footnote {
                    // the nodes a footnote definition is in end with it at the earliest,
                    // though comrak moved it out of them
                    let end = scopes.end(node);
                    for &around in &path[1..path.len() - 1] {
                        if scopes.end(around) < end {
                            scopes.ends.insert(around as *const _, end);
                            if let Some(&id) = scopes.ids.get(&(around as *const _)) {
                                scopes.spans[id].1 = end;
                            }
                        }
                    }
                }
                scopes.parents.push(Some(parent));
                scopes.paths.push(path);
                // a list item's content lines are indented as far as its first line's
                // content, from where its parent's content starts there
                let rel = match kind {
                    Kind::Item => lines
                        .get(start.line.wrapping_sub(1))
                        .and_then(|line| {
                            let path = &scopes.paths[id];
                            let at = scopes.walk(&path[..path.len() - 1], start.line, line)?;
                            item_marker(line, at).map(|(_, rel)| rel)
                        })
                        .unwrap_or(usize::MAX),
                    _ => 0,
                };
                scopes.rels.push(rel);
            }
            stack.extend(node.reverse_children());
        }
        scopes
    }

    /// The node `node` is in: its parent, or for a footnote definition that comrak moved to
    /// the end of the document, the innermost node that holds blocks, starts before it, and
    /// whose prefixes its first line (in `lines`) continues (the one that starts last; for
    /// another footnote definition that comrak moved, the prefixes of the containers it is
    /// in too, which come before it). (comrak counts the line that ends a footnote definition
    /// or a definition among its lines.)
    fn parent_of(
        &self,
        root: &'a AstNode<'a>,
        node: &'a AstNode<'a>,
        lines: &[&str],
    ) -> Option<&'a AstNode<'a>> {
        let data = node.data();
        let parent = node.parent();
        if !matches!(data.value, NodeValue::FootnoteDefinition(_))
            || !parent.is_some_and(|parent| std::ptr::eq(parent, root))
        {
            return parent;
        }
        let start = data.sourcepos.start;
        let Some(line) = lines.get(start.line.wrapping_sub(1)) else {
            return parent;
        };
        let before = (start.line, start.column);
        // where the content of `child`, a child of `parent` whose content starts at `at` on
        // the line, starts there, when it holds blocks, starts before the footnote definition
        // and goes on to its line, and the line continues it
        let enters = |parent: &'a AstNode<'a>, child: &'a AstNode<'a>, at: Cursor| {
            let span = child.data().sourcepos;
            if std::ptr::eq(child, node)
                || !holds_blocks(&child.data().value)
                || (span.start.line, span.start.column) >= before
                || start.line > self.end(child)
            {
                return None;
            }
            let moved = std::ptr::eq(parent, root)
                && matches!(child.data().value, NodeValue::FootnoteDefinition(_));
            match (moved, self.id(child)) {
                (true, Some(scope)) => self.content(scope, start.line, line),
                (true, None) => None,
                (false, _) => self.prefix(child, start.line, line, at),
            }
        };
        // of the document's children, the last one before the line that it continues: those
        // before a child that ends before the line end before it too, and those between a
        // footnote definition that comrak moved (which the line does not continue) and the
        // innermost of them it is in are in that one, and end before it, or contain it
        let index_of = |key: (usize, usize)| {
            self.roots.partition_point(|child| {
                let start = child.data().sourcepos.start;
                (start.line, start.column) < key
            })
        };
        let mut index = index_of(before);
        let mut next = None;
        while index > 0 {
            index -= 1;
            let child = self.roots[index];
            if let Some(after) = enters(root, child, Cursor::default()) {
                next = Some((child, after));
                break;
            }
            if !matches!(child.data().value, NodeValue::FootnoteDefinition(_)) {
                if self.end(child) < start.line {
                    break;
                }
                continue;
            }
            let Some(scope) = self.id(child) else {
                continue;
            };
            let path = &self.paths[scope];
            let Some(&around) = path[1..path.len() - 1]
                .iter()
                .rev()
                .find(|around| around.parent().is_some_and(|up| std::ptr::eq(up, root)))
            else {
                break;
            };
            let start = around.data().sourcepos.start;
            index = index.min(index_of((start.line, start.column)) + 1);
        }
        let Some((mut parent, mut at)) = next else {
            return Some(root);
        };
        // below it, the child that starts last of those the line continues
        loop {
            let mut next: Option<(&'a AstNode<'a>, Cursor)> = None;
            for child in parent.children() {
                if let Some(after) = enters(parent, child, at)
                    && next.is_none_or(|(last, _)| {
                        let (last, now) = (last.data().sourcepos, child.data().sourcepos);
                        (last.start.line, last.start.column) < (now.start.line, now.start.column)
                    })
                {
                    next = Some((child, after));
                }
            }
            match next {
                Some((child, after)) => (parent, at) = (child, after),
                None => return Some(parent),
            }
        }
    }

    /// The last line of `node` ([`last_lines`]).
    fn end(&self, node: &'a AstNode<'a>) -> usize {
        self.ends
            .get(&(node as *const _))
            .copied()
            .unwrap_or_else(|| node.data().sourcepos.end.line)
    }

    /// Whether the last block in `node`, at the end of its last children or of a footnote
    /// definition in it that comrak moved out of it, is a paragraph (or a setext heading that
    /// comrak made of one) that ends on `line` (a line that continues it lazily continues
    /// `node`).
    fn ends_in_paragraph(&self, node: &'a AstNode<'a>, line: usize) -> bool {
        let last_paragraph_ends = |mut tip: &'a AstNode<'a>| {
            while let Some(last) = tip.last_child() {
                if matches!(
                    last.data().value,
                    NodeValue::Paragraph | NodeValue::Heading(NodeHeading { setext: true, .. })
                ) {
                    return self.end(last) == line;
                }
                tip = last;
            }
            false
        };
        last_paragraph_ends(node)
            || self
                .footnotes
                .get_or_init(|| {
                    let mut footnotes: HashMap<*const AstNode<'a>, Vec<usize>> = HashMap::new();
                    for scope in
                        (0..self.nodes.len()).filter(|&scope| self.kinds[scope] == Kind::Footnote)
                    {
                        let path = &self.paths[scope];
                        for &around in &path[..path.len() - 1] {
                            footnotes.entry(around as *const _).or_default().push(scope);
                        }
                    }
                    footnotes
                })
                .get(&(node as *const _))
                .is_some_and(|scopes| {
                    scopes
                        .iter()
                        .any(|&scope| last_paragraph_ends(self.nodes[scope]))
                })
    }

    /// Whether line `number` continues a paragraph: it is one of its lines after the first.
    fn continues_paragraph(&self, number: usize) -> bool {
        let after = self
            .continued
            .partition_point(|&(start, _)| start <= number);
        after > 0 && self.continued[after - 1].1 >= number
    }

    /// The scope of the container node `node`, if it is one.
    fn id(&self, node: &'a AstNode<'a>) -> Option<usize> {
        self.ids.get(&(node as *const _)).copied()
    }

    /// Whether scope `inner` is `outer` or in it.
    fn contains(&self, outer: usize, inner: usize) -> bool {
        let mut scope = Some(inner);
        while let Some(now) = scope {
            if now == outer {
                return true;
            }
            scope = self.parents[now];
        }
        false
    }

    /// Where the content of `scope` starts on `line` (1-based `number`): after the prefixes
    /// of the containers it is in and its own, and the indentation of the directives around
    /// it that they strip; `None` when the line does not continue them.
    fn content(&self, scope: usize, number: usize, line: &str) -> Option<Cursor> {
        self.walk(&self.paths[scope], number, line)
    }

    /// Where `line` (1-based `number`) is after the prefixes of the nodes of `path`, from the
    /// document down.
    fn walk(&self, path: &[&'a AstNode<'a>], number: usize, line: &str) -> Option<Cursor> {
        path.iter().try_fold(Cursor::default(), |at, &node| {
            self.prefix(node, number, line, at)
        })
    }

    /// Where `line` (1-based `number`) is after the prefix of `node`, from `at`, as comrak
    /// reads it: the marker on its first line, and what continues it on the others; `None`
    /// when the line does not continue it.
    fn prefix(
        &self,
        node: &'a AstNode<'a>,
        number: usize,
        line: &str,
        at: Cursor,
    ) -> Option<Cursor> {
        let data = node.data();
        let start = data.sourcepos.start;
        let first = number == start.line;
        // the byte where the node starts on its first line
        let marker = || {
            start
                .column
                .checked_sub(1 + if number == 1 { self.bom } else { 0 })
        };
        let bytes = line.as_bytes();
        match &data.value {
            NodeValue::Document | NodeValue::List(_) | NodeValue::DescriptionList => Some(at),
            NodeValue::BlockDirective(directive) => Some(strip(line, at, directive.fence_offset)),
            NodeValue::Item(_) | NodeValue::TaskItem(_) => {
                let (nonspace, indent) = nonspace(line, at);
                if first {
                    (Some(nonspace.byte) == marker())
                        .then(|| item_marker(line, at))
                        .flatten()
                        .map(|(content, _)| content)
                } else {
                    let rel = self.rels[self.id(node)?];
                    if indent >= rel {
                        Some(advance(line, at, rel))
                    } else {
                        // a blank line continues a list item that holds something
                        (nonspace.byte >= line.len() && node.first_child().is_some())
                            .then_some(nonspace)
                    }
                }
            }
            NodeValue::Alert(alert) if alert.multiline => None,
            NodeValue::BlockQuote | NodeValue::Alert(_) => {
                let (nonspace, indent) = nonspace(line, at);
                if indent > 3 || bytes.get(nonspace.byte) != Some(&b'>') {
                    return None;
                }
                let after = advance(line, at, indent + 1);
                Some(match bytes.get(after.byte) {
                    Some(b' ' | b'\t') => advance(line, after, 1),
                    _ => after,
                })
            }
            NodeValue::FootnoteDefinition(_) => {
                let (nonspace, indent) = nonspace(line, at);
                if first {
                    if Some(nonspace.byte) != marker() {
                        return None;
                    }
                    let label = footnote_label(line.get(nonspace.byte..)?)?;
                    Some(advance_bytes(line, nonspace, label))
                } else if indent >= 4 {
                    Some(advance(line, at, 4))
                } else {
                    // only an empty line continues it otherwise
                    line.is_empty().then_some(at)
                }
            }
            NodeValue::DescriptionItem(item) => {
                let details = node
                    .children()
                    .find(|child| matches!(child.data().value, NodeValue::DescriptionDetails))?;
                let details = details.data().sourcepos.start.line;
                match number.cmp(&details) {
                    std::cmp::Ordering::Less => None,
                    std::cmp::Ordering::Equal => Some(at),
                    std::cmp::Ordering::Greater => {
                        let (nonspace, indent) = nonspace(line, at);
                        let rel = item.marker_offset + item.padding;
                        if indent >= rel {
                            Some(advance(line, at, rel))
                        } else {
                            // a blank line continues a definition that holds something
                            (nonspace.byte >= line.len() && node.first_child().is_some())
                                .then_some(nonspace)
                        }
                    }
                }
            }
            NodeValue::DescriptionDetails => {
                if !first {
                    return Some(at);
                }
                let (nonspace, _) = nonspace(line, at);
                if Some(nonspace.byte) != marker() {
                    return None;
                }
                let rest = line.get(nonspace.byte..)?;
                let after = rest.strip_prefix([':', '~'])?;
                let space = after.len() - after.trim_start_matches([' ', '\t']).len();
                (space > 0).then(|| advance_bytes(line, nonspace, 1 + space))
            }
            NodeValue::MultilineBlockQuote(_) => None,
            _ => None,
        }
    }

    /// For each of `candidates` (1-based lines of `lines`, in order) that holds a fence after
    /// the prefixes of the innermost scope whose lines include it, that fence, in that scope;
    /// none for a line that continues a paragraph in it without those prefixes (lazily).
    fn place(&self, candidates: &[usize], lines: &[&str]) -> Vec<Fence> {
        // the containers in order of their start, and those whose lines include the line now
        let mut order: Vec<usize> = (1..self.nodes.len()).collect();
        order.sort_unstable_by_key(|&scope| self.keys[scope]);
        let mut order = order.into_iter().peekable();
        let mut around: Vec<usize> = vec![0];
        let mut placed = Vec::with_capacity(candidates.len());
        for &number in candidates {
            while let Some(scope) = order.next_if(|&scope| self.spans[scope].0 <= number) {
                while let Some(&last) = around.last()
                    && !self.contains(last, scope)
                {
                    around.pop();
                }
                around.push(scope);
            }
            while let Some(&last) = around.last()
                && self.spans[last].1 < number
            {
                around.pop();
            }
            let Some(line) = lines.get(number - 1) else {
                continue;
            };
            // the innermost of them whose prefixes the line continues (comrak may count the
            // line after a container among its lines); a line in a container that does not
            // continue its prefixes (one before its last line, or one that continues a
            // paragraph) continues a paragraph in it lazily, and is text of it (or of the
            // heading, table or link reference definition comrak makes of it), and no fence
            let mut scope = around.last().copied().unwrap_or(0);
            if self.content(scope, number, line).is_none()
                && (number < self.spans[scope].1 || self.continues_paragraph(number))
            {
                continue;
            }
            let at = loop {
                if let Some(at) = self.content(scope, number, line) {
                    break Some(at);
                }
                match self.parents[scope] {
                    Some(parent) => scope = parent,
                    None => break None,
                }
            };
            if let Some(fence) = at.and_then(|at| fence_from(line, at)) {
                placed.push(Fence {
                    line: number,
                    scope,
                    ..fence
                });
            }
        }
        placed
    }

    /// The container that `line` (1-based `number`) continues below `directive`, which comrak
    /// closed there, and where its content starts: the innermost list item or definition (a
    /// line that continues a block quote or footnote is no closing fence) still open at the
    /// end of the directive, in it or in the directives, lists and containers at its end,
    /// whose prefix the line continues once the directive strips its indentation. `closed`
    /// holds the opening lines of the directives that comrak closed with a fence of their own
    /// before. `None` when the line continues none: then it is in the directive's scope. The
    /// flag says whether the line is content there, of a code block or HTML block that comrak
    /// left unfinished at the end of the container ([`unfinished_end`]).
    fn culprit(
        &self,
        directive: &'a AstNode<'a>,
        number: usize,
        lines: &[&str],
        closed: &HashSet<usize>,
    ) -> Option<(usize, Cursor, bool)> {
        let line = lines.get(number - 1)?;
        // the path down to the directive: its scope's, then the directives of that scope
        // around it, and the directive itself
        let mut between = Vec::new();
        let mut up = Some(directive);
        let scope = loop {
            let node = up?;
            if let Some(scope) = self.id(node) {
                break scope;
            }
            if matches!(node.data().value, NodeValue::Document) {
                break 0;
            }
            between.push(node);
            up = node.parent();
        };
        let mut at = self.content(scope, number, line)?;
        for &node in between.iter().rev() {
            at = self.prefix(node, number, line, at)?;
        }
        // where the content of `node`, below the nodes on `path`, starts on the line
        // `number`, when the line continues them all
        let continues = |path: &[&'a AstNode<'a>], node: &'a AstNode<'a>, number: usize| {
            let line = lines.get(number.checked_sub(1)?)?;
            let at = self.content(scope, number, line)?;
            let at = between
                .iter()
                .rev()
                .chain(path)
                .try_fold(at, |at, &node| self.prefix(node, number, line, at))?;
            self.prefix(node, number, line, at)
        };
        let mut found = None;
        let mut path = Vec::new();
        let mut node = directive;
        while let Some(last) = node.last_child() {
            let data = last.data();
            let (start, end) = (data.sourcepos.start.line, self.end(last));
            let open = match &data.value {
                NodeValue::BlockDirective(_) => !closed.contains(&start),
                NodeValue::List(_) | NodeValue::DescriptionList | NodeValue::DescriptionDetails => {
                    true
                }
                // comrak ends a container that it closes along with the directive on its last
                // line, or, when the container holds an open directive, on the line that ended
                // it (which then does not continue it): the container is still open on the
                // fence line when its last line is its own (its first, one that continues it,
                // or one that lazily continues a paragraph or setext heading in it) and every
                // line after it continues it
                NodeValue::Item(_) | NodeValue::TaskItem(_) | NodeValue::DescriptionItem(_) => {
                    end >= number
                        || (end == start
                            || continues(&path, last, end).is_some()
                            || self.ends_in_paragraph(last, end))
                            && (end + 1..number).all(|k| continues(&path, last, k).is_some())
                }
                _ => false,
            };
            if !open {
                break;
            }
            drop(data);
            let Some(next) = self.prefix(last, number, line, at) else {
                break;
            };
            at = next;
            if let Some(scope) = self.id(last) {
                found = Some((scope, at));
            }
            path.push(last);
            node = last;
        }
        // the line goes on a code block or HTML block that comrak left unfinished at the end
        // of the innermost node open there (an indented code block when it is indented far
        // enough); `at` is where that node's content starts
        let content = node.last_child().is_some_and(|leaf| {
            unfinished_end(leaf, &mut |_| None) == Some(number - 1)
                && match &leaf.data().value {
                    NodeValue::CodeBlock(code) => code.fenced || nonspace(line, at).1 >= 4,
                    _ => true,
                }
        });
        found.map(|(scope, at)| (scope, at, content))
    }
}

/// The last line of every node of the tree of `root`: the last of its own (comrak's
/// sourcepos), of its children's (also inline: comrak does not count the lines that lazily
/// continue a paragraph among the lines of a paragraph that it leaves unfinished), for a
/// block that comrak left unfinished, the last line it holds ([`unfinished_end`]), and for a
/// fenced code block, the last line of its text ([`held_end`]).
fn last_lines<'a>(root: &'a AstNode<'a>) -> HashMap<*const AstNode<'a>, usize> {
    let mut ends: HashMap<*const AstNode<'a>, usize> = HashMap::new();
    // the directives (first line, last line, node) in order of their first lines, built when
    // a block that comrak left unfinished is in a footnote definition that it moved
    let mut directives: Option<Vec<(usize, usize, &'a AstNode<'a>)>> = None;
    let mut around = |line: usize| {
        let directives = directives.get_or_insert_with(|| {
            let mut all: Vec<(usize, usize, &'a AstNode<'a>)> = root
                .descendants()
                .filter(|node| matches!(node.data().value, NodeValue::BlockDirective(_)))
                .map(|node| {
                    let span = node.data().sourcepos;
                    (span.start.line, span.end.line, node)
                })
                .collect();
            all.sort_by_key(|&(start, _, _)| start);
            all
        });
        let before = directives.partition_point(|&(start, _, _)| start < line);
        directives[..before]
            .iter()
            .rev()
            .find(|&&(_, end, _)| end >= line)
            .map(|&(_, _, node)| node)
    };
    for edge in root.traverse() {
        let NodeEdge::End(node) = edge else {
            continue;
        };
        let own = node.data().sourcepos.end.line;
        let end = node
            .children()
            .filter_map(|child| ends.get(&(child as *const _)).copied())
            .chain(unfinished_end(node, &mut around))
            .chain(held_end(node))
            .fold(own, usize::max);
        ends.insert(node as *const _, end);
    }
    ends
}

/// The last line of the text of `node` when it is a fenced code block: comrak 0.56 ends one
/// without a closing fence on its first line when blank lines follow its text before its
/// container ends, though it holds the lines up to there (the blank ones are not counted).
fn held_end<'a>(node: &'a AstNode<'a>) -> Option<usize> {
    let data = node.data();
    let NodeValue::CodeBlock(code) = &data.value else {
        return None;
    };
    if !code.fenced {
        return None;
    }
    // its lines as comrak has them (a lone `\r` ends one too), without the blank ones at the end
    let text = code.literal.trim_end_matches([' ', '\t', '\n', '\r']);
    let lines = match text {
        "" => 0,
        text => split_lines(text).1.len(),
    };
    Some(data.sourcepos.start.line + lines)
}

/// The last line of `node` when it is a code block or HTML block that comrak 0.56 left
/// unfinished: the last block of the last list item of a list at the end of a directive that
/// a closing fence ended (comrak then finalizes the list, but not its last item and what is
/// in it, so the block keeps no text and ends on its first line; it held the lines up to the
/// fence). A footnote definition that comrak moved to the end of the document is in the
/// directive that `around` gives for its first line (the innermost one around that line, if
/// any), and was its last block when all else in it starts before it.
fn unfinished_end<'a>(
    node: &'a AstNode<'a>,
    around: &mut dyn FnMut(usize) -> Option<&'a AstNode<'a>>,
) -> Option<usize> {
    let data = node.data();
    let start = data.sourcepos.start.line;
    let empty = match &data.value {
        NodeValue::HtmlBlock(html) => html.literal.is_empty(),
        NodeValue::CodeBlock(code) => code.literal.is_empty() && data.sourcepos.end.line == start,
        _ => false,
    };
    if !empty {
        return None;
    }
    let mut list = false;
    let mut child = node;
    while let Some(mut parent) = child.parent() {
        if matches!(parent.data().value, NodeValue::Document)
            && matches!(child.data().value, NodeValue::FootnoteDefinition(_))
        {
            // the directive comrak moved it out of, when it was its last block (all else in
            // the directive starts before it); an empty block held lines only when comrak
            // left it unfinished, as in a list (the lists between are no longer known)
            parent = around(child.data().sourcepos.start.line)?;
            let mut last = parent;
            while let Some(inner) = last.last_child() {
                last = inner;
            }
            let (inner, own) = (last.data().sourcepos.start, child.data().sourcepos.start);
            if (inner.line, inner.column) > (own.line, own.column) {
                return None;
            }
            list = true;
        } else if child.next_sibling().is_some() {
            return None;
        }
        match &parent.data().value {
            NodeValue::List(_) => list = true,
            NodeValue::BlockDirective(_) => {
                let end = parent.data().sourcepos.end.line;
                if end > start {
                    return list.then_some(end - 1);
                }
            }
            NodeValue::Document => return None,
            _ => {}
        }
        child = parent;
    }
    None
}

/// Where the content of a list item starts on its first line, whose marker is the first byte
/// from `at` (where its parent's content starts) that is not a space or tab, and the columns
/// its content lines are indented by from `at` (comrak's `marker_offset + padding`): one
/// column past the marker when what follows it is blank, or 5 columns or more of white
/// space, and past that white space otherwise.
fn item_marker(line: &str, at: Cursor) -> Option<(Cursor, usize)> {
    let (marker, indent) = nonspace(line, at);
    let rest = line.get(marker.byte..)?;
    let width = match rest.as_bytes().first()? {
        b'-' | b'+' | b'*' => 1,
        b'0'..=b'9' => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            match rest.as_bytes().get(digits) {
                Some(b'.' | b')') if digits <= 9 => digits + 1,
                _ => return None,
            }
        }
        _ => return None,
    };
    let after = advance_bytes(line, marker, width);
    let mut space = after;
    while space.column - after.column <= 5
        && matches!(line.as_bytes().get(space.byte), Some(b' ' | b'\t'))
    {
        space = advance(line, space, 1);
    }
    let columns = space.column - after.column;
    let blank = space.byte >= line.len();
    if !(1..5).contains(&columns) || blank {
        let content = if columns > 0 {
            advance(line, after, 1)
        } else {
            after
        };
        Some((content, indent + width + 1))
    } else {
        Some((space, indent + width + columns))
    }
}

/// The length of a footnote definition's label at the start of `rest` (`[^label]:`, then any
/// spaces and tabs).
fn footnote_label(rest: &str) -> Option<usize> {
    let label = rest.strip_prefix("[^")?;
    let length = label.find([']', ' ', '\t', '\r', '\n'])?;
    let after = label[length..].strip_prefix("]:")?;
    let space = after.len() - after.trim_start_matches([' ', '\t']).len();
    (length > 0).then_some(2 + length + 2 + space)
}

/// The fences of a tree's directives in line order ([`collect`]), the role comrak gave each,
/// how deeply its directives nest, and how many lines they have (a line in `n` directives
/// counts `n` times).
struct Collected {
    fences: Vec<Fence>,
    roles: Vec<Role>,
    depth: usize,
    area: usize,
}

/// The fences of the directives of the tree whose scopes are `scopes` (its text has `lines`,
/// without the byte order mark), with the role comrak gave each, or `None` when a fence line
/// is not what comrak's tree says it is. A directive's closing fence is the line it ends on
/// when that line is a bare fence at least as long as its opening fence, in its scope,
/// indented little enough to close it, and not where another directive opens (a directive
/// closed with its parent, or by the end of the text, ends on its last content line instead,
/// or on the parent's fence when it is empty or ends with blank lines). A closing fence that
/// continues a list item or definition at the end of its directive is in that container
/// ([`Scopes::culprit`]).
fn collect<'a>(scopes: &Scopes<'a>, lines: &[&str]) -> Option<Collected> {
    // (fence, opening line of the parent or target, whether it closes)
    let mut found: Vec<(Fence, Option<usize>, bool)> = Vec::new();
    // the closing fences found, and their directives
    let mut closers: Vec<(usize, &'a AstNode<'a>)> = Vec::new();
    let mut depth = 0;
    let mut area = 0;
    // (node, its scope, the opening line of the directive around it in that scope and the
    // columns stripped inside that directive, how many directives are around it)
    type Pending<'a> = (&'a AstNode<'a>, usize, Option<(usize, usize)>, usize);
    let root = scopes.nodes[0];
    let mut pending: Vec<Pending<'a>> = root
        .reverse_children()
        .map(|node| (node, 0, None, 0))
        .collect();
    while let Some((node, scope, parent, around)) = pending.pop() {
        let data = node.data();
        if !holds_blocks(&data.value) {
            continue;
        }
        let NodeValue::BlockDirective(directive) = &data.value else {
            let (scope, parent) = match scopes.id(node) {
                Some(inner) => (inner, None),
                None => (scope, parent),
            };
            pending.extend(
                node.reverse_children()
                    .map(|child| (child, scope, parent, around)),
            );
            continue;
        };
        let (start, end) = (data.sourcepos.start.line, data.sourcepos.end.line);
        area += end.saturating_sub(start) + 1;
        let line = lines.get(start.checked_sub(1)?)?;
        let opening = fence_from(line, scopes.content(scope, start, line)?)?;
        let named = !directive.info.trim().is_empty();
        let column = data.sourcepos.start.column;
        let byte = column.checked_sub(1 + if start == 1 { scopes.bom } else { 0 });
        if opening.colons != directive.fence_length
            || named && opening.bare
            || Some(opening.at) != byte
        {
            return None;
        }
        found.push((
            Fence {
                line: start,
                named,
                scope,
                ..opening
            },
            parent.map(|(line, _)| line),
            false,
        ));
        depth = depth.max(around + 1);
        let base = parent.map_or(0, |(_, base)| base);
        if end > start
            && let Some(closing) = lines
                .get(end - 1)
                .and_then(|line| fence_from(line, scopes.content(scope, end, line)?))
            && closing.bare
            && closing.colons >= directive.fence_length
            && closing.indent.saturating_sub(base) <= 3
        {
            found.push((
                Fence {
                    line: end,
                    scope,
                    ..closing
                },
                Some(start),
                true,
            ));
            closers.push((end, node));
        }
        let inner = base + opening.indent.saturating_sub(base);
        pending.extend(
            node.reverse_children()
                .map(|child| (child, scope, Some((start, inner)), around + 1)),
        );
    }
    // A "closing fence" where a directive opens is the last line of a directive closed by
    // the end of the text, whose last child opens there. A directive closed with its parent
    // can end on the parent's closing fence: of the directives ending on one fence, the
    // outermost (opened first) is the one it closes.
    let opened: HashSet<usize> = found
        .iter()
        .filter(|(_, _, closes)| !closes)
        .map(|(fence, _, _)| fence.line)
        .collect();
    found.retain(|(fence, _, closes)| !closes || !opened.contains(&fence.line));
    found.sort_by_key(|&(fence, other, _)| (fence.line, other));
    found.dedup_by_key(|(fence, _, _)| fence.line);
    // the closing fences that continue a list item or definition at the end of their
    // directive are in that container
    let closed: HashSet<usize> = found
        .iter()
        .filter(|(_, _, closes)| *closes)
        .filter_map(|(_, other, _)| *other)
        .collect();
    for (line, directive) in closers {
        let Ok(index) = found.binary_search_by_key(&line, |(fence, _, _)| fence.line) else {
            continue;
        };
        let (fence, _, closes) = &mut found[index];
        if !*closes {
            continue;
        }
        if let Some((scope, at, content)) = scopes.culprit(directive, line, lines, &closed)
            && let Some(inner) = lines.get(line - 1).and_then(|text| fence_from(text, at))
        {
            *fence = Fence {
                line,
                scope,
                content,
                ..inner
            };
        }
    }
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
    Some(Collected {
        fences,
        roles,
        depth,
        area,
    })
}

/// The fences to read in this tree, in line order, and the role comrak gave each: those of
/// `found` (with their colons as written in `lines`, the text as written), the fences
/// `earlier` of the last reading that this tree has as no fences (ignored lines, and fences
/// it hides), and the other bare fence lines of `placed` ([`Scopes::place`]) as content; a
/// line that comrak has as no fence is [`Role::Text`] in its reading. `None` when a fence of
/// `found` is not as written.
fn fences_to_read(
    found: &Collected,
    earlier: &[Fence],
    placed: &[Fence],
    lines: &[&str],
) -> Option<(Vec<Fence>, Vec<Role>)> {
    let place = |line: usize| {
        placed
            .binary_search_by_key(&line, |fence| fence.line)
            .ok()
            .map(|index| placed[index])
    };
    // (fence, its index in `found`)
    let mut fences: Vec<(Fence, Option<usize>)> = Vec::with_capacity(found.fences.len());
    for (index, fence) in found.fences.iter().enumerate() {
        let written = lines.get(fence.line - 1)?.get(fence.at..)?;
        let colons = written.len() - written.trim_start_matches(':').len();
        let rest = &written[colons..];
        if colons < 3
            || rest.contains(':')
            || rest.trim_matches([' ', '\t']).is_empty() != fence.bare
        {
            return None;
        }
        fences.push((Fence { colons, ..*fence }, Some(index)));
    }
    let mut taken: HashSet<usize> = fences.iter().map(|(fence, _)| fence.line).collect();
    for fence in earlier {
        if taken.insert(fence.line)
            && let Some(now) = place(fence.line)
        {
            let now = Fence {
                named: fence.named,
                content: fence.content,
                ..now
            };
            fences.push((now, None));
        }
    }
    for fence in placed {
        if fence.bare && taken.insert(fence.line) {
            let content = Fence {
                content: true,
                ..*fence
            };
            fences.push((content, None));
        }
    }
    fences.sort_by_key(|(fence, _)| fence.line);
    let mut index = vec![0; found.fences.len()];
    for (now, (_, from)) in fences.iter().enumerate() {
        if let Some(from) = from {
            index[*from] = now;
        }
    }
    let roles = fences
        .iter()
        .map(|(_, from)| match from.map(|from| found.roles[from]) {
            Some(Role::Open { parent }) => Role::Open {
                parent: parent.map(|parent| index[parent]),
            },
            Some(Role::Close { target }) => Role::Close {
                target: index[target],
            },
            _ => Role::Text,
        })
        .collect();
    Some((fences.into_iter().map(|(fence, _)| fence).collect(), roles))
}

/// A line read as a directive fence: indentation, three or more colons, then anything
/// without a colon (`bare` when that is only spaces and tabs). `None` for anything else.
fn fence_line(line: &str) -> Option<Fence> {
    fence_from(line, Cursor::default())
}

/// The fence that `line` holds from `at` on ([`fence_line`]), its indentation counted from
/// there.
fn fence_from(line: &str, at: Cursor) -> Option<Fence> {
    let (colon, indent) = nonspace(line, at);
    let rest = line.get(colon.byte..)?;
    let colons = rest.len() - rest.trim_start_matches(':').len();
    let info = &rest[colons..];
    if colons < 3 || info.contains(':') {
        return None;
    }
    Some(Fence {
        line: 0,
        colons,
        indent,
        at: colon.byte,
        bare: info.trim_matches([' ', '\t']).is_empty(),
        named: false,
        content: false,
        scope: 0,
    })
}

/// Reads `fences` as the dialect does (module docs, step 3), in the scopes `scopes`. A fence
/// can close an open directive of its scope when it is bare, at least as long as the
/// directive's fence, and indented at most three columns more than comrak strips there: each
/// enclosing directive of the scope strips as many columns as its own fence was indented
/// relative to it. A fence outside the scope of an open directive (after the container that
/// directive was opened in) ends it first.
fn read(fences: &[Fence], scopes: &Scopes) -> Reading {
    let mut open: Vec<Open> = Vec::new();
    let mut reading = Reading {
        roles: Vec::with_capacity(fences.len()),
        groups: Vec::with_capacity(fences.len()),
        outer: Vec::with_capacity(fences.len()),
        unclosed: Vec::new(),
        deep: false,
    };
    for (i, fence) in fences.iter().enumerate() {
        // the directives of the containers this line is past end with them
        while let Some(&last) = open.last()
            && !scopes.contains(last.scope, fence.scope)
        {
            reading.unclosed.push(Unclosed {
                line: fences[last.fence].line,
                colons: last.colons,
                closed_by: ClosedBy::Container(scopes.kinds[last.scope]),
            });
            open.pop();
        }
        // the directives open in the line's scope
        let own = open
            .iter()
            .rposition(|directive| directive.scope != fence.scope)
            .map_or(0, |outer| outer + 1);
        reading
            .groups
            .push(open.first().map(|directive| directive.fence));
        reading
            .outer
            .push(own.checked_sub(1).map(|outer| open[outer].fence));
        // of those it can close, the innermost one with the longest fence
        let mut target = None::<usize>;
        for (depth, directive) in open.iter().enumerate().skip(own) {
            let closable = fence.bare
                && fence.colons >= directive.colons
                && fence.indent.saturating_sub(directive.base) <= 3;
            if closable && target.is_none_or(|target| directive.colons >= open[target].colons) {
                target = Some(depth);
            }
        }
        if let Some(depth) = target {
            let directive = open[depth];
            reading.roles.push(Role::Close {
                target: directive.fence,
            });
            reading
                .unclosed
                .extend(open[depth + 1..].iter().map(|inner| Unclosed {
                    line: fences[inner.fence].line,
                    colons: inner.colons,
                    closed_by: ClosedBy::Outer {
                        colons: fence.colons,
                        line: fences[directive.fence].line,
                    },
                }));
            open.truncate(depth);
            continue;
        }
        if fence.content {
            reading.roles.push(Role::Text);
            continue;
        }
        // the columns stripped inside the innermost directive open in the scope
        let parent = open[own..].last();
        let base = parent.map_or(0, |directive| {
            directive.base
                + fences[directive.fence]
                    .indent
                    .saturating_sub(directive.base)
        });
        if !fence.named {
            reading.roles.push(Role::Ignore {
                offset: fence.indent.saturating_sub(base),
            });
            continue;
        }
        reading.roles.push(Role::Open {
            parent: parent.map(|directive| directive.fence),
        });
        if open.is_empty() {
            reading.groups[i] = Some(i);
        }
        open.push(Open {
            fence: i,
            colons: fence.colons,
            base,
            scope: fence.scope,
        });
        // the reading is not used: reading on would take time quadratic in the depth
        if open.len() > MAX_DEPTH {
            reading.deep = true;
            return reading;
        }
    }
    reading
        .unclosed
        .extend(open.iter().rev().map(|directive| Unclosed {
            line: fences[directive.fence].line,
            colons: directive.colons,
            closed_by: match directive.scope {
                0 => ClosedBy::End,
                scope => ClosedBy::Container(scopes.kinds[scope]),
            },
        }));
    reading
        .unclosed
        .sort_unstable_by_key(|directive| directive.line);
    reading
}

/// For each fence, whether the group it belongs to in `reading` ([`Reading::groups`]) is
/// rewritten: when a fence of the group has another role in `comrak` (comrak's reading of
/// the tree), or comrak gives one a role in the group, or a fence of the group is on one of
/// the lines `rewritten` before. Only those groups get longer fences.
fn changed(
    fences: &[Fence],
    reading: &Reading,
    comrak: &[Role],
    rewritten: &HashSet<usize>,
) -> Vec<bool> {
    let mut groups = HashSet::new();
    for (i, (role, theirs)) in reading.roles.iter().zip(comrak).enumerate() {
        if role != theirs {
            groups.extend(reading.groups[i]);
            if let Role::Close { target }
            | Role::Open {
                parent: Some(target),
            } = *theirs
            {
                groups.extend(reading.groups[target]);
            }
        }
        if rewritten.contains(&fences[i].line) {
            groups.extend(reading.groups[i]);
        }
    }
    reading
        .groups
        .iter()
        .map(|group| group.is_some_and(|group| groups.contains(&group)))
        .collect()
}

/// The number of colons each fence needs for comrak to read `reading` (module docs, step 4):
/// for the fences marked in `changed`, a directive's fence is at least as long as before and
/// as its closing fence, and longer than the fence of every directive inside it, and than
/// every bare line left as content in a container inside it (comrak looks for a directive's
/// closing fence before it looks at the containers inside it); its closing fence is as long
/// as that. Other fences keep their length.
fn fence_lengths(fences: &[Fence], reading: &Reading, changed: &[bool]) -> Vec<usize> {
    let mut colons: Vec<usize> = fences.iter().map(|fence| fence.colons).collect();
    // in reverse order, every directive inside a directive and its closing fence come first
    for i in (0..fences.len()).rev().filter(|&i| changed[i]) {
        match reading.roles[i] {
            Role::Close { target } => colons[target] = colons[target].max(fences[i].colons),
            Role::Open { parent } => {
                if let Some(around) = parent.or(reading.outer[i]) {
                    colons[around] = colons[around].max(colons[i] + 1);
                }
            }
            Role::Text => {
                if let Some(around) = reading.outer[i] {
                    colons[around] = colons[around].max(fences[i].colons + 1);
                }
            }
            Role::Ignore { .. } => {}
        }
    }
    for i in (0..fences.len()).filter(|&i| changed[i]) {
        if let Role::Close { target } = reading.roles[i] {
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

/// What comrak should find in the rewritten text ([`without_ignored`]).
struct Expected {
    /// The lines of its fences.
    lines: Vec<usize>,
    /// The scope of each ([`Scopes::keys`]).
    keys: Vec<(usize, usize)>,
    /// Their roles, with the fences numbered among them.
    roles: Vec<Role>,
}

/// The fences that `roles` neither ignores nor leaves as content, in the scopes `scopes`:
/// what comrak should find in the rewritten text.
fn without_ignored(fences: &[Fence], roles: &[Role], scopes: &Scopes) -> Expected {
    let mut index = vec![0; fences.len()];
    let mut expected = Expected {
        lines: Vec::with_capacity(fences.len()),
        keys: Vec::with_capacity(fences.len()),
        roles: Vec::with_capacity(fences.len()),
    };
    for (i, (fence, role)) in fences.iter().zip(roles).enumerate() {
        index[i] = expected.roles.len();
        expected.roles.push(match *role {
            Role::Ignore { .. } | Role::Text => continue,
            Role::Open { parent } => Role::Open {
                parent: parent.map(|parent| index[parent]),
            },
            Role::Close { target } => Role::Close {
                target: index[target],
            },
        });
        expected.lines.push(fence.line);
        expected.keys.push(scopes.keys[fence.scope]);
    }
    expected
}

/// Step 5 of the module docs: `root` and its `text` with the line of every directive
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
            ClosedBy::Outer { colons, line } => format!(
                "has no closing fence of its own: the `{}` line that closes the {} around it \
                 ends it too",
                ":".repeat(colons),
                label(line)
            ),
            ClosedBy::Container(kind) => format!(
                "has no closing fence in the {kind} it is in: it ends with the {kind} (a `:::` \
                 outside the {kind} cannot close it)",
                kind = kind.name()
            ),
            ClosedBy::End => "has no closing fence: it runs to the end of the document (or \
                              notebook cell), and `---` lines inside it start no slides"
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

/// The line [`report_fallback`] reports `fallback` on: that of the first directive of `root`
/// (a tree of `text`; for [`Fallback::Strays`], the first without a name), or else of the
/// first line of `text` that holds a fence.
fn fallback_line<'a>(root: &'a AstNode<'a>, text: &str, fallback: Fallback) -> Option<usize> {
    root.descendants()
        .find_map(|node| {
            let data = node.data();
            match &data.value {
                NodeValue::BlockDirective(directive)
                    if fallback != Fallback::Strays || directive.info.trim().is_empty() =>
                {
                    Some(data.sourcepos.start.line)
                }
                _ => None,
            }
        })
        .or_else(|| {
            split_lines(text)
                .1
                .iter()
                .position(|line| fence_line(line).is_some())
                .map(|index| index + 1)
        })
}

/// The `invalid-directive` warning for a text left as comrak reads it, on `line`
/// ([`fallback_line`]).
fn report_fallback(line: Option<usize>, fallback: Fallback, diagnostics: &mut Diagnostics) {
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
                NodeValue::Table(_) => "table".to_string(),
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
    fn deeply_nested_rewrites_count_against_the_budget() {
        // each parse settles one more of these code blocks, and every rewrite that makes
        // markers of the fences from a line on leaves each `a` past it open, so that comrak's
        // tree nests about as deep as there are code blocks left: counting only the bytes
        // parsed, 221 parses took 11 s in a release build; with the lines of the directives,
        // the text is left as comrak reads it after a few
        let started = std::time::Instant::now();
        let text = ":::a\n- ```\n  :::\n```\n".repeat(3600);
        let (edited, _, warnings) = renest_warned(&text);
        assert_eq!(edited, text);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].1.starts_with("presenter could not rewrite"),
            "{warnings:?}"
        );
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
        // `unit` repeated between `head` and `tail` so many times that one parse each would
        // take more parses than the text may take, and how many times
        let chain = |head: &str, unit: &str, tail: &str| {
            let mut count = 100;
            loop {
                let text = format!("{head}{}{tail}", unit.repeat(count));
                if attempts(&text) < count {
                    return (text, count);
                }
                count *= 2;
            }
        };
        // each slide's columns, read as comrak does, end early and leave a `:::` that opens
        // a directive around the next code block, whose `:::` ends it early, so that the
        // code block's closing fence opens one that hides the next slide: one parse per
        // slide, unless the next reading takes the fences and ignored lines the rewrite hides
        // too (all of them settle in a few parses then)
        let columns = ":::columns\n:::column\nA\n:::\n:::column\nB\n:::\n:::\n";
        let sample = "```markdown\n:::note\nText\n:::\n```\n";
        let (text, count) = chain("", &format!("{columns}\n---\n\n{sample}\n---\n\n"), "End\n");
        let (_, tree, warnings) = renest_all(&text);
        let expected = "columns[column[A] column[B]] --- code --- ".repeat(count) + "End";
        assert_eq!(tree, expected);
        assert_eq!(warnings, []);
        // a `:::` too many before each code block
        let (text, count) = chain("", &format!(":::\n\n{sample}\n---\n\n"), columns);
        let (_, tree, warnings) = renest_warned(&text);
        let expected = "~ code --- ".repeat(count) + "columns[column[A] column[B]]";
        assert_eq!(tree, expected);
        assert_eq!(warnings, []);
        let lines: Vec<usize> = (0..count).map(|slide| slide * 10 + 1).collect();
        assert_eq!(ignored_lines(&text), lines);
        // in list items: comrak's reading of each item's `:::` ends the item, so that the
        // code block's closing fence opens one that hides the next item, until a rewrite
        // makes every fence from there on a marker
        let (text, count) = chain(":::a\n", "- ```\n  :::\n  ```\n", ":::\n");
        let (_, tree, warnings) = renest_all(&text);
        assert_eq!(tree, format!("a[{}]", vec!["code"; count].join(" ")));
        assert_eq!(warnings, []);
        // comrak's reading leaves each item's sample a directive of the document's, which the
        // rewrite's tree has in the item's code block again
        let unit = "- item\n  ```\n  :::note\n  text\n  :::\n  ```\n";
        let (text, count) = chain(":::outer\n", unit, ":::\n");
        let (_, tree, warnings) = renest_all(&text);
        assert_eq!(
            tree,
            format!("outer[{}]", vec!["item code"; count].join(" "))
        );
        assert_eq!(warnings, []);
        let slide = ":::columns\n:::column\n- Write a note:\n\n  ```\n  :::note\n  Text\n  :::\n  \
                     ```\n:::\n:::column\nResult\n:::\n:::\n";
        let (text, count) = chain("", &format!("{slide}\n---\n\n"), "End\n");
        let (_, tree, warnings) = renest_all(&text);
        let expected =
            "columns[column[Write a note: code] column[Result]] --- ".repeat(count) + "End";
        assert_eq!(tree, expected);
        assert_eq!(warnings, []);
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
        // in a block quote, neither `:::` closes anything; in comrak's reading the second one
        // closes the unnamed directive the first one opens, and once that one is a marker,
        // the second opens one, and is a marker in the next round
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
    fn directives_in_containers_nest_too() {
        // in a list item, task item, block quote, footnote or definition, fences of one
        // length nest as at the top
        for (text, edited, tree) in [
            (
                "- item\n\n  :::columns\n  :::column\n  A\n  :::\n  :::column\n  B\n  :::\n  :::\n",
                "- item\n\n  ::::columns\n  :::column\n  A\n  :::\n  :::column\n  B\n  :::\n  ::::\n",
                "item columns[column[A] column[B]]",
            ),
            (
                "> :::columns\n> :::column\n> A\n> :::\n> :::column\n> B\n> :::\n> :::\n",
                "> ::::columns\n> :::column\n> A\n> :::\n> :::column\n> B\n> :::\n> ::::\n",
                ">[columns[column[A] column[B]]]",
            ),
            (
                "- [ ] task\n\n  :::columns\n  :::column\n  A\n  :::\n  :::\n",
                "- [ ] task\n\n  ::::columns\n  :::column\n  A\n  :::\n  ::::\n",
                "task columns[column[A]]",
            ),
            (
                "Text[^1]\n\n[^1]: Note\n    :::columns\n    :::column\n    A\n    :::\n    :::\n",
                "Text[^1]\n\n[^1]: Note\n    ::::columns\n    :::column\n    A\n    :::\n    ::::\n",
                "Text Note columns[column[A]]",
            ),
            (
                "Term\n\n: :::columns\n  :::column\n  A\n  :::\n  :::\n",
                "Term\n\n: ::::columns\n  :::column\n  A\n  :::\n  ::::\n",
                "Term columns[column[A]]",
            ),
            (
                ":::note\n- a\n  :::small\n  A\n  :::\n- b\n:::\n",
                "::::note\n- a\n  :::small\n  A\n  :::\n- b\n::::\n",
                "note[a small[A] b]",
            ),
        ] {
            let (now, now_tree, warnings) = renest_all(text);
            assert_eq!(now, edited, "{text:?}");
            assert_eq!(now_tree, tree, "{text:?}");
            assert_eq!(warnings, [], "{text:?}");
        }
        // as comrak reads them already
        let (edited, tree, warnings) = renest_all("- item\n\n  :::small\n  A\n  :::\n");
        assert_eq!(edited, "- item\n\n  :::small\n  A\n  :::\n");
        assert_eq!(tree, "item small[A]");
        assert_eq!(warnings, []);
    }

    #[test]
    fn a_fence_in_a_container_closes_nothing_around_it() {
        // comrak takes a `:::` that continues a list item, or one in a block quote, as the
        // closing fence of the directive around the list or quote
        for (text, edited, tree, line) in [
            (
                ":::note\n- item\n\n  :::\n:::\n",
                format!(":::note\n- item\n\n  {}\n:::\n", marker(0)),
                "note[item ~]",
                4,
            ),
            (
                ":::note\n- item\n  :::\n:::\n",
                format!(":::note\n- item\n  {}\n:::\n", marker(0)),
                "note[item ~]",
                3,
            ),
            (
                ":::note\n> A\n> :::\n:::\n",
                format!(":::note\n> A\n> {}\n:::\n", marker(0)),
                "note[>[A ~]]",
                3,
            ),
        ] {
            let (now, now_tree, warnings) = renest_warned(text);
            assert_eq!(now, edited, "{text:?}");
            assert_eq!(now_tree, tree, "{text:?}");
            assert_eq!(warnings, [], "{text:?}");
            assert_eq!(ignored_lines(text), [line], "{text:?}");
        }
        // in a code block in a list item it stays code, and the directive around the list
        // gets a longer fence
        let text = ":::note\n- ```\n  :::\n  ```\n:::\n";
        let (edited, tree, warnings) = renest_all(text);
        assert_eq!(edited, "::::note\n- ```\n  :::\n  ```\n::::\n");
        assert_eq!(tree, "note[code]");
        assert_eq!(warnings, []);
        // where it does not continue the list item, it is outside it
        let text = ":::note\n- a\n\n  :::small\n  B\n:::\n";
        let (edited, tree, warnings) = renest_all(text);
        assert_eq!(edited, text);
        assert_eq!(tree, "note[a small[B]]");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].0, 4);
    }

    #[test]
    fn a_directive_ends_with_its_container() {
        for (text, edited, tree, kind, ignored) in [
            (
                "- item\n\n  :::small\n  A\n\nB\n:::\n",
                format!("- item\n\n  :::small\n  A\n\nB\n{}\n", marker(0)),
                "item small[A] B ~",
                "list item",
                7,
            ),
            (
                "> :::note\n> A\n\n:::\n",
                format!("> :::note\n> A\n\n{}\n", marker(0)),
                ">[note[A]] ~",
                "block quote",
                4,
            ),
        ] {
            let (now, now_tree, warnings) = renest_warned(text);
            assert_eq!(now, edited, "{text:?}");
            assert_eq!(now_tree, tree, "{text:?}");
            let opened = text.lines().position(|line| line.contains(":::")).unwrap() + 1;
            assert_eq!(warnings.len(), 1, "{text:?}: {warnings:?}");
            assert_eq!(warnings[0].0, opened, "{text:?}");
            let expected = format!(
                "has no closing fence in the {kind} it is in: it ends with the {kind} (a `:::` \
                 outside the {kind} cannot close it). Add one (`:::`) where it ends"
            );
            assert!(warnings[0].1.contains(&expected), "{}", warnings[0].1);
            assert_eq!(ignored_lines(text), [ignored], "{text:?}");
        }
    }

    #[test]
    fn list_item_indentation_is_derived_as_comrak_has_it() {
        for text in [
            "- a\n",
            "-   a\n",
            "-     a\n",
            "-\ta\n",
            "-\t\ta\n",
            " 1. a\n",
            "10) a\n",
            "-\n  a\n",
            "- \n  a\n",
            "> - a\n",
            ">\t- a\n",
            "- - a\n",
            "-\t-\ta\n",
            "  -  \ta\n",
            "1.     code\n",
            " -    \tb\n",
            "- a\n\n  1)  b\n",
            "Text[^1]\n\n[^1]: - a\n    - b\n",
            "Term\n\n: - a\n  - b\n",
        ] {
            let arena = Arena::new();
            let root = parse_document(&arena, text, &dialect_options());
            let (_, lines) = text_lines(text);
            let scopes = Scopes::new(root, &lines, 0);
            let mut items = 0;
            for (scope, node) in scopes.nodes.iter().enumerate() {
                if let NodeValue::Item(list) = &node.data().value {
                    assert_eq!(
                        scopes.rels[scope],
                        list.marker_offset + list.padding,
                        "{text:?}: item at line {}",
                        scopes.keys[scope].0
                    );
                    items += 1;
                }
            }
            assert!(items > 0, "{text:?}");
        }
    }

    #[test]
    fn a_fence_is_in_the_containers_still_open_on_its_line() {
        // comrak ends the item that `a`'s fence closes on its last line, before the blank
        // one: the fence is still in the item, so it closes `note`, not `a`
        let (edited, tree, warnings) =
            renest_all(":::a\n1. :::::note\n   x\n\n   :::::\n2. y\n:::\n");
        assert_eq!(
            edited,
            "::::::a\n1. :::::note\n   x\n\n   :::::\n2. y\n::::::\n"
        );
        assert_eq!(tree, "a[note[x] y]");
        assert_eq!(warnings, []);
        // comrak ends the item on the footnote that ended it: the fence after it is no
        // longer in the item (nor in the footnote, which it does not continue), so it
        // closes `a`, around the list
        let (edited, tree, warnings) =
            renest_all("Text[^1]\n\n:::a\n- :::b\n\n[^1]: x\n\n  :::\n:::\n");
        assert_eq!(
            edited,
            "Text[^1]\n\n:::a\n- :::b\n\n[^1]: x\n\n  :::\n<!--presenter:ignored-fence colons=3 offset=0-->\n"
        );
        assert_eq!(tree, "Text a[b[]] ~ x");
        let lines: Vec<usize> = warnings.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [4, 9]);
    }

    #[test]
    fn a_line_that_continues_a_paragraph_lazily_keeps_the_container_open() {
        // comrak does not count the lazy line among the item's lines when `b`'s fence ends
        // the item; the fence after it is in the item all the same
        let (edited, tree, warnings) = renest_all(":::b\n- :::a\n  x\ny\n  :::\n  z\n:::\n");
        assert_eq!(edited, "::::b\n- :::a\n  x\ny\n  :::\n  z\n::::\n");
        assert_eq!(tree, "b[a[xy] z]");
        assert_eq!(warnings, []);
        for (text, tree) in [
            (":::b\n- x\ny\n  :::\n:::\n", "b[xy ~]"),
            (
                "> :::b\n> - item\n> lazy\n>   :::\n> :::\n",
                ">[b[itemlazy ~]]",
            ),
            (
                ":::b\n- item\n  - sub\nlazy\n  :::\n:::\n",
                "b[item sublazy ~]",
            ),
        ] {
            // the line before the last is in the item, and closes nothing there
            let (_, now, _) = renest_warned(text);
            assert_eq!(now, tree, "{text:?}");
            assert_eq!(ignored_lines(text), [text.lines().count() - 1], "{text:?}");
        }
        // a footnote's lazy line, though comrak moves the footnote to the end
        let text =
            "T[^1].\n\n:::outer\nTerm\n\n: head\n\n  [^1]: text\nmore\n  :::\n\nafter\n:::\n";
        let (_, tree, _) = renest_warned(text);
        assert_eq!(tree, "T. outer[Term head ~ after] textmore");
        assert_eq!(ignored_lines(text), [10]);
        // a fence on a lazy line (indented too far to interrupt the paragraph) is text of the
        // paragraph: it neither ends the container nor closes anything, whatever it is
        // indented by from where the directive's content starts
        for (text, tree) in [
            ("> :::a\n> x\n    :::\n> :::\n> after\n", ">[a[x:::] after]"),
            (
                ">   :::outer\n>   :::a\n>   x\n    :::\n>   :::\n>   :::\n",
                ">[outer[a[x:::]]]",
            ),
            (
                "> 10.  :::a\n>      x\n>     :::\n>      :::\n",
                ">[a[x:::]]",
            ),
            ("- :::a\n  x\n      :::\n  :::\n", "a[x:::]"),
            // also when comrak makes a setext heading, a table or a link reference
            // definition of the paragraph
            ("> :::a\n> p\n    :::\n> =\n> :::\n> b\n", ">[a[#p:::] b]"),
            (
                "> :::a\n> p\n    :::\n> |-|\n> :::\n> b\n",
                ">[a[p table] b]",
            ),
            ("> :::a\n> [r]:\n    :::\n> :::\n> q\n", ">[a[] q]"),
        ] {
            let (_, now, warnings) = renest_all(text);
            assert_eq!(now, tree, "{text:?}");
            assert_eq!(warnings, [], "{text:?}");
        }
        // the directives in the block quote end with it, though the lazy line is no fence
        let (_, tree, warnings) = renest_warned("> :::a\n> [r]:\n    :::\n> :::b\n");
        assert_eq!(tree, ">[a[b[]]]");
        assert_eq!(
            warnings.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            [1, 4]
        );
        // a setext heading's lazy line keeps its list item open as a paragraph's does
        let text = ":::b\n- x\ny\n  =\n  :::\n  a\n";
        let (_, tree, warnings) = renest_warned(text);
        assert_eq!(tree, "b[#xy ~ a]");
        assert_eq!(
            warnings.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(ignored_lines(text), [5]);
    }

    #[test]
    fn a_fence_in_code_left_open_at_the_end_of_a_list_item_is_code() {
        // comrak does not finish the item's last block when `a`'s fence ends the list: the
        // block keeps no text in its tree, though it held the `:::` lines
        for (text, tree) in [
            (":::a\n- item\n  ```\n  :::\n:::\n", "a[item code]"),
            (":::a\n- <div>\n  :::\n:::\n", "a[html]"),
            (":::a\n- ```\n  :::\n  :::\n  ```\n:::\n", "a[code]"),
            (":::a\n1. ~~~\n   ::::\n   :::\n   ~~~\n:::\n", "a[code]"),
            (
                "> :::a\n> - ```\n>   :::\n>   :::\n>   ```\n> :::\n",
                ">[a[code]]",
            ),
        ] {
            let (_, now, warnings) = renest_all(text);
            assert_eq!(now, tree, "{text:?}");
            assert_eq!(warnings, [], "{text:?}");
        }
        // when blank lines follow the block's text before the item ends, comrak ends the
        // block on its first line, and the item there too
        for (text, tree) in [
            (":::a\n- x\n  ```\n  :::\n\ntext\n:::\n", "a[x code text]"),
            (":::a\n1. x\n   ```\n   :::\n\n2. y\n:::\n", "a[x code y]"),
            (
                ":::a\nTerm\n: def\n  ```\n  :::\n\ntext\n:::\n",
                "a[Term def code text]",
            ),
            (
                "Ref[^1]\n\n[^1]: x\n    ```\n    :::\n\n:::a\ntext\n:::\n",
                "Ref a[text] x code",
            ),
        ] {
            let (_, now, warnings) = renest_all(text);
            assert_eq!(now, tree, "{text:?}");
            assert_eq!(warnings, [], "{text:?}");
        }
        // also when a lone `\r` ends a line of it, and in a footnote that comrak moves out
        // of the directive whose fence ends it
        for (text, tree, line) in [
            (":::a\n- x\n  ~~~~\n  :::\n\n", "a[x code]", 1),
            (":::a\n- ```\n  l\r  :::\n\n", "a[code]", 1),
            (
                "[^1]\n::::w\n[^1]: :::n\n    - ```\n      :::\n::::\n",
                " w[] n[code]",
                3,
            ),
        ] {
            let (_, now, warnings) = renest_all(text);
            assert_eq!(now, tree, "{text:?}");
            assert_eq!(warnings.len(), 1, "{text:?}");
            assert_eq!(warnings[0].0, line, "{text:?}");
            assert!(warnings[0].1.contains("no closing fence"), "{warnings:?}");
        }
        // a code block that a line not in the footnote ends, before the directive's fence,
        // held no lines: the `:::` lines after it are in the definition, not in the block
        let text = "Ref[^1]\n:::d\nTerm\n: C\n  [^1]: x\n      ```\n     :::\n  :::\n:::\n";
        let (_, now, warnings) = renest_all(text);
        assert_eq!(now, "Ref d[Term C ~ ~] x code");
        let lines: Vec<usize> = warnings.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [7, 8]);
        assert!(
            warnings.iter().all(|(_, message)| message
                .contains("It cannot close the `d` directive around the definition it is in")),
            "{warnings:?}"
        );
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

    /// The scopes of a text without containers: the document only.
    fn document<'a>(arena: &'a Arena<'a>) -> Scopes<'a> {
        Scopes::new(parse_document(arena, "", &dialect_options()), &[], 0)
    }

    #[test]
    fn readings() {
        let arena = Arena::new();
        let scopes = document(&arena);
        let fence = |line, colons, bare| Fence {
            line,
            colons,
            indent: 0,
            at: 0,
            bare,
            named: !bare,
            content: false,
            scope: 0,
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
        // comrak: the first `:::` closes the column, and the small in it, and the second
        // opens a directive
        let comrak = [
            open(None),
            open(Some(0)),
            open(Some(1)),
            close(1),
            open(Some(0)),
            close(0),
        ];
        let inner = read(&fences, &scopes);
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
        assert_eq!(inner.unclosed, []);
        assert_eq!(inner.groups, [Some(0); 6]);
        assert_eq!(inner.outer, [None; 6]);
        let none = HashSet::new();
        let changed_now = changed(&fences, &inner, &comrak, &none);
        assert_eq!(changed_now, [true; 6]);
        assert_eq!(
            fence_lengths(&fences, &inner, &changed_now),
            [5, 4, 3, 3, 4, 5]
        );
        // a group that comrak reads as the dialect does keeps its fences, unless they were
        // rewritten before
        assert_eq!(changed(&fences, &inner, &inner.roles, &none), [false; 6]);
        assert_eq!(
            changed(&fences, &inner, &inner.roles, &HashSet::from([4])),
            [true; 6]
        );
        // without the second `:::`: the `::::` closes the columns and the column with them
        let fences = [fences[0], fences[1], fences[2], fences[3], fences[5]];
        let inner = read(&fences, &scopes);
        assert_eq!(
            inner.roles,
            [open(None), open(Some(0)), open(Some(1)), close(2), close(0)]
        );
        assert_eq!(
            inner.unclosed,
            [Unclosed {
                line: 2,
                colons: 3,
                closed_by: ClosedBy::Outer { colons: 4, line: 1 },
            }]
        );
        // a `:::` with nothing to close is ignored, with its indentation inside the
        // directive around it; nothing closes that one
        let indented = Fence {
            indent: 2,
            ..fence(3, 3, true)
        };
        let fences = [fence(1, 4, false), fence(2, 3, true), indented];
        let inner = read(&fences, &scopes);
        assert_eq!(
            inner.roles,
            [
                open(None),
                Role::Ignore { offset: 0 },
                Role::Ignore { offset: 2 }
            ]
        );
        assert_eq!(
            inner.unclosed,
            [Unclosed {
                line: 1,
                colons: 4,
                closed_by: ClosedBy::End,
            }]
        );
    }
}
