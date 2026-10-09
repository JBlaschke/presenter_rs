//! `:::include`: composing a deck from several files (markdown-dialect §4.1, T48).
//!
//! The Markdown frontend ([`super::parse`]) splits a deck into slides first; then:
//!
//! 1. `find_includes` looks at every `:::include` directive of the split deck. One that is
//!    the only visible content (HTML comments and `:::notes` blocks do not count,
//!    [`compat::is_invisible`]) of a horizontal slide without vertical slides, closed by a
//!    `:::` line right after it, is an *include slide*; any other `:::include` is an
//!    [`Error::Parse`] at its line, with the including file's path: in a vertical slide
//!    (after `--`), inside a list, quote or another directive, next to other content, with a
//!    body, not closed, without a path, or a `:::include` line comrak did not open (a `:`
//!    after the fence). Slide attributes (`--- {…}`, `<!-- .slide: -->`) and `:::notes` of
//!    an include slide are ignored with an [`codes::INCLUDE_ATTRS_IGNORED`] warning.
//! 2. `expand` replaces an include slide by every top-level slide of the included file,
//!    with their vertical children. The path (quoted when it has spaces) is resolved against
//!    the including file's directory (absolute paths work; URLs do not); the extension picks
//!    the frontend (`.md`/`.markdown` or `.ipynb`, as [`crate::pipeline::detect_kind`] does for an
//!    input); the file is read as UTF-8 and parsed as if it were built on its own, with
//!    a [`SourceInfo`] of its own (its images resolve against its directory, its diagnostics
//!    and errors carry its path) and with the including deck's config (front matter merged).
//!    Its own front matter or notebook metadata config tables still apply to its parsing; its
//!    deck metadata is discarded, with an [`codes::INCLUDE_META_IGNORED`] warning naming the
//!    keys it sets that differ from the top-level deck's. A missing, unreadable or non-UTF-8
//!    file, an unknown extension, a cycle (by canonical path) and nesting deeper than
//!    [`MAX_DEPTH`] are [`Error::Parse`] at the directive's line. Errors inside the included
//!    file keep its own path and line (a front matter error becomes an [`Error::Parse`] with
//!    its path, so that it is not located in the top-level deck).
//! 3. Every slide (and child) of an included file records the file in `SourceSpan.file`, as
//!    do the spans of its diagnostics; its assets, features and diagnostics are merged into the
//!    deck, and `Deck.includes` lists every included file, transitively, in first-use order.
//!
//! [`scan_includes`] finds the files a Markdown deck includes without parsing it as a deck,
//! for the watcher of a deck that does not parse. In a notebook (markdown cells, `text/markdown`
//! outputs), the renderer removes a `:::include` with an [`codes::INCLUDE_IN_NOTEBOOK`] warning.

use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};
use serde_json::{Map, Value};

use crate::assets::registry::{AssetRegistry, normalize_lexically};
use crate::config::Config;
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{DeckMeta, Features, Parsed, Slide, SourceInfo, SourceKind, SourceSpan};
use crate::markdown::compat;
use crate::markdown::render::{is_include_info, is_notes_info};
use crate::markdown::split::RawSlide;

/// How deep includes may nest: a file included by the top-level deck is at depth 1, a file
/// it includes at depth 2, and so on (markdown-dialect §4.1).
pub const MAX_DEPTH: usize = 8;

/// The diagnostic codes of includes (all `Level::Warning`).
pub mod codes {
    /// Slide attributes or `:::notes` on an include slide; ignored.
    pub const INCLUDE_ATTRS_IGNORED: &str = "include-attrs-ignored";
    /// Deck metadata of an included file that differs from the top-level deck's; ignored.
    pub const INCLUDE_META_IGNORED: &str = "include-meta-ignored";
    /// `:::include` in a notebook (a markdown cell or output); nothing is included and the
    /// directive renders nothing.
    pub const INCLUDE_IN_NOTEBOOK: &str = "include-in-notebook";
}

/// The config tables of front matter (config.md §3), which an included file applies to its
/// own parsing; they are not deck metadata for [`codes::INCLUDE_META_IGNORED`].
const CONFIG_TABLES: [&str; 4] = ["slides", "markdown", "math", "notebook"];

/// An include slide found by [`find_includes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Include {
    /// The directive's line in the including file.
    pub(crate) line: usize,
    /// The path as written (quotes removed).
    pub(crate) path: String,
}

/// A file pulled in by `:::include`, with the config it was parsed with: the including
/// deck's, with the file's own front matter (or notebook metadata) config tables merged.
/// `check` lints its source with it (the split mode, the Markdown and notebook options).
#[derive(Debug, Clone)]
pub(crate) struct IncludedSource {
    /// The file, absolute (as in `Deck.includes`).
    pub(crate) path: PathBuf,
    /// Which frontend parsed it.
    pub(crate) kind: SourceKind,
    /// The config it was parsed with.
    pub(crate) config: Config,
}

/// The files being parsed, from the top-level deck down to the current one (for cycles and
/// the depth limit), and the top-level deck's metadata (what an included file's metadata is
/// compared with).
#[derive(Debug, Clone)]
pub(crate) struct Context {
    /// `(canonical path, path as shown in messages)` of each file, the top-level deck first.
    files: Vec<(PathBuf, String)>,
    /// The top-level deck's directory; paths below it are shown relative to it.
    root_dir: PathBuf,
    /// The top-level deck's metadata.
    root_meta: DeckMeta,
}

impl Context {
    /// The context of the top-level deck `source` with front matter `meta`.
    pub(crate) fn root(source: &SourceInfo, meta: &DeckMeta) -> Context {
        Context {
            files: vec![(
                canonical(&source.path),
                shown(&source.path, &source.base_dir),
            )],
            root_dir: source.base_dir.clone(),
            root_meta: meta.clone(),
        }
    }

    /// The context of a file included from the current one.
    fn enter(&self, canonical: PathBuf, path: &Path) -> Context {
        let mut next = self.clone();
        next.files.push((canonical, shown(path, &self.root_dir)));
        next
    }

    /// The chain of files, each as shown, ending with `last`: `a.md → b.md → last`.
    fn chain_to(&self, last: &str) -> String {
        let mut names: Vec<&str> = self.files.iter().map(|(_, name)| name.as_str()).collect();
        names.push(last);
        names.join(" → ")
    }
}

/// What [`expand`] makes of one include slide.
#[derive(Debug)]
pub(crate) struct Expanded {
    /// The included file's top-level slides (with their children), spans tagged.
    pub(crate) slides: Vec<Slide>,
    /// Its assets.
    pub(crate) assets: AssetRegistry,
    /// Its features.
    pub(crate) features: Features,
    /// Its diagnostics (and those of the files it includes), paths and span files set.
    pub(crate) diagnostics: Diagnostics,
    /// The file, then the files it includes.
    pub(crate) includes: Vec<PathBuf>,
    /// The file and the files it includes, with the config each was parsed with.
    pub(crate) sources: Vec<IncludedSource>,
}

/// The `:::include` directives of a split deck (module docs, step 1): for each horizontal
/// slide of `slides`, the include it consists of, if it is an include slide.
///
/// `lines` is the source the deck was split from ([`super::split::source_lines`]), `path`
/// the including file (for errors), `headings` whether it is split in headings mode (for the
/// hint of an include next to other content). Ignored slide attributes and notes are
/// reported in `diagnostics` (no path). Errors: [`Error::Parse`] at the line of the first
/// `:::include` that is not an include slide, or whose path cannot be read from its line.
pub(crate) fn find_includes<'a>(
    slides: &[(RawSlide<'a>, Vec<RawSlide<'a>>)],
    lines: &[&str],
    path: &Path,
    headings: bool,
    diagnostics: &mut Diagnostics,
) -> Result<Vec<Option<Include>>> {
    let error = |line: usize, message: String| Error::Parse {
        path: path.to_path_buf(),
        line: Some(line),
        message,
    };
    let mut found = Vec::with_capacity(slides.len());
    for (slide, children) in slides {
        for raw in std::iter::once(slide).chain(children) {
            check_nested(&raw.nodes).map_err(|(line, message)| error(line, message))?;
        }
        for child in children {
            if let Some(node) = child.nodes.iter().find(|node| is_include(node)) {
                let message = format!(
                    "`:::include` cannot be in a vertical slide (the `--` on line {} opened \
                     it): an included file's slides are horizontal slides; put `---` before \
                     the directive instead",
                    child.start_line
                );
                return Err(error(start_line(node), message));
            }
        }
        let include = include_slide(slide, children, lines, headings)
            .map_err(|(line, message)| error(line, message))?;
        if let Some(include) = &include {
            report_ignored(slide, include.line, diagnostics);
        }
        found.push(include);
    }
    Ok(found)
}

/// A problem with an `:::include`: its line and the message.
type Misuse = (usize, String);

/// Errors for an `:::include` nested inside one of `nodes` (a list, a quote, another
/// directive, …) and for a `:::include` line comrak did not open as a directive.
fn check_nested<'a>(nodes: &[&'a AstNode<'a>]) -> std::result::Result<(), Misuse> {
    for node in nodes.iter().flat_map(|node| node.descendants()) {
        if let Some(message) = unopened_include(node) {
            return Err((start_line(node), message));
        }
        let Some(parent) = node.parent() else {
            continue;
        };
        if !is_include(&node) || matches!(parent.data().value, NodeValue::Document) {
            continue;
        }
        let message = format!(
            "`:::include` must be a top-level block of a slide of its own, not inside {}: an \
             included file's slides replace that slide",
            container_name(parent)
        );
        return Err((start_line(node), message));
    }
    Ok(())
}

/// The include `slide` (a horizontal slide with `children`) consists of, if any; an error
/// when it has an `:::include` but is not an include slide.
fn include_slide<'a>(
    slide: &RawSlide<'a>,
    children: &[RawSlide<'a>],
    lines: &[&str],
    headings: bool,
) -> std::result::Result<Option<Include>, Misuse> {
    let Some(&directive) = slide.nodes.iter().find(|node| is_include(node)) else {
        return Ok(None);
    };
    let line = start_line(directive);
    let other = slide
        .nodes
        .iter()
        .find(|node| !std::ptr::eq(**node, directive) && !compat::is_invisible(node));
    if let Some(other) = other {
        let hint = if headings {
            "put `---` before the directive, and after it unless the next slide starts with a \
             heading"
        } else {
            "put `---` before and after the directive"
        };
        let message = format!(
            "`:::include` must be the only content of its slide, but line {} has other \
             content: {hint}",
            start_line(other)
        );
        return Err((line, message));
    }
    if let Some(child) = children.first() {
        let message = format!(
            "the slide of `:::include` cannot have vertical slides (the `--` on line {}): the \
             included file's slides replace it; use `---` instead",
            child.start_line
        );
        return Err((line, message));
    }
    check_closed(directive, lines)?;
    let info = match &directive.data().value {
        NodeValue::BlockDirective(directive) => directive.info.clone(),
        _ => String::new(),
    };
    let path = include_path(&info).map_err(|message| (line, message))?;
    Ok(Some(Include { line, path }))
}

/// An error unless `directive` is closed by a `:::` line and has nothing in between.
fn check_closed<'a>(directive: &'a AstNode<'a>, lines: &[&str]) -> std::result::Result<(), Misuse> {
    let data = directive.data();
    let (start, end) = (data.sourcepos.start.line, data.sourcepos.end.line);
    let fence = match &data.value {
        NodeValue::BlockDirective(directive) => directive.fence_length,
        _ => 3,
    };
    let closing = end > start
        && lines.get(end - 1).is_some_and(|line| {
            let line = line.trim();
            line.len() >= fence && line.bytes().all(|b| b == b':')
        });
    if !closing {
        let message = format!(
            "`:::include` on line {start} is not closed: put a `:::` line right after it \
             (without one the directive takes everything after it, to the end of the \
             document)"
        );
        return Err((start, message));
    }
    if let Some(first) = directive.first_child() {
        let message = format!(
            "`:::include` takes no content, but line {} is inside it: put the closing `:::` \
             right after the `:::include` line",
            start_line(first)
        );
        return Err((start, message));
    }
    Ok(())
}

/// [`codes::INCLUDE_ATTRS_IGNORED`] warnings for slide attributes and `:::notes` on an
/// include slide (the directive is on `line`).
fn report_ignored<'a>(slide: &RawSlide<'a>, line: usize, diagnostics: &mut Diagnostics) {
    let span = SourceSpan {
        start_line: line,
        end_line: line,
        cell: None,
        file: None,
    };
    if !slide.attrs.is_empty() {
        let message = "slide attributes on the slide of `:::include` (from its `--- {…}` \
                       separator or a `<!-- .slide: -->` comment) are ignored: the included \
                       file's slides keep their own";
        diagnostics.warn(codes::INCLUDE_ATTRS_IGNORED, message).span = Some(span.clone());
    }
    let notes = slide.nodes.iter().any(
        |node| matches!(&node.data().value, NodeValue::BlockDirective(d) if is_notes_info(&d.info)),
    );
    if notes {
        let message = "speaker notes on the slide of `:::include` are ignored: write them on a \
                       slide of the included file";
        diagnostics.warn(codes::INCLUDE_ATTRS_IGNORED, message).span = Some(span);
    }
}

/// The path of an `:::include` directive's info string (`include PATH`, `include "a b.md"`
/// or `include 'a b.md'`), or the problem with it.
fn include_path(info: &str) -> std::result::Result<String, String> {
    let info = info.trim();
    if info.starts_with('{') {
        return Err(format!(
            "write `:::include PATH`: the attribute form `::: {info}` is not read for includes"
        ));
    }
    let rest = info.strip_prefix("include").unwrap_or(info).trim();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''));
    let (path, after) = match quote {
        Some(quote) => {
            let inner = &rest[1..];
            let Some(end) = inner.find(quote) else {
                return Err(format!("unbalanced {quote} quote in `:::include {rest}`"));
            };
            (&inner[..end], inner[end + 1..].trim())
        }
        None => match rest.split_once(char::is_whitespace) {
            Some((path, after)) => (path, after.trim()),
            None => (rest, ""),
        },
    };
    if path.is_empty() {
        return Err(
            "`:::include` needs the path of a Markdown (.md) or notebook (.ipynb) \
                    file: `:::include part2.md`"
                .into(),
        );
    }
    if !after.is_empty() {
        return Err(format!(
            "unexpected `{after}` after the path of `:::include`: quote a path that has \
             spaces (`:::include \"my talk/part 2.md\"`)"
        ));
    }
    if path.contains("://") {
        return Err(format!(
            "cannot include `{path}`: only local files can be included, not URLs"
        ));
    }
    Ok(path.to_string())
}

/// For a paragraph that starts like `:::include …` (comrak did not open a directive, because
/// of a `:` after the fence), the error message.
fn unopened_include<'a>(node: &'a AstNode<'a>) -> Option<String> {
    if !matches!(node.data().value, NodeValue::Paragraph) {
        return None;
    }
    let first = node.first_child()?;
    let data = first.data();
    let NodeValue::Text(text) = &data.value else {
        return None;
    };
    let line = text.lines().next().unwrap_or_default();
    let rest = line.trim_start_matches(':');
    let after = rest.trim_start().strip_prefix("include")?;
    if line.len() - rest.len() < 3 || !(after.is_empty() || after.starts_with(char::is_whitespace))
    {
        return None;
    }
    Some(if after.contains("://") {
        format!(
            "cannot include `{}`: only local files can be included, not URLs",
            after.trim()
        )
    } else {
        "this `:::include` line is not read as a directive: comrak does not open one when the \
         line has a `:` after the fence; write the colon as `&#58;`"
            .to_string()
    })
}

/// Whether `node` is an `:::include` directive.
fn is_include<'a>(node: &&'a AstNode<'a>) -> bool {
    matches!(&node.data().value, NodeValue::BlockDirective(d) if is_include_info(&d.info))
}

/// What a node that holds blocks is, for messages.
fn container_name<'a>(node: &'a AstNode<'a>) -> String {
    match &node.data().value {
        NodeValue::List(_) | NodeValue::Item(_) | NodeValue::TaskItem(_) => "a list".into(),
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => "a block quote".into(),
        NodeValue::Alert(_) => "an alert".into(),
        NodeValue::FootnoteDefinition(_) => "a footnote".into(),
        NodeValue::BlockDirective(directive) => {
            let name = directive.info.split_whitespace().next().unwrap_or_default();
            format!("the `:::{name}` directive")
        }
        _ => "another block".into(),
    }
}

/// The first source line of `node`.
fn start_line(node: &AstNode<'_>) -> usize {
    node.data().sourcepos.start.line
}

/// Parses the file `include` names and returns its slides and the rest (module docs, steps 2
/// and 3). `source` is the including file, `config` the config it was parsed with, `context`
/// its place in the chain of includes. An [`codes::INCLUDE_META_IGNORED`] warning goes to
/// `warnings` (at the directive's line, no path).
pub(crate) fn expand(
    include: &Include,
    source: &SourceInfo,
    config: &Config,
    context: &Context,
    warnings: &mut Diagnostics,
) -> Result<Expanded> {
    let at = |message: String| Error::Parse {
        path: source.path.clone(),
        line: Some(include.line),
        message,
    };
    let written = &include.path;
    let path = normalize_lexically(&source.base_dir.join(written));
    let kind = included_kind(&path).ok_or_else(|| {
        at(format!(
            "cannot include `{written}`: only Markdown (.md, .markdown) and notebook (.ipynb) \
             files can be included"
        ))
    })?;
    let text =
        read_included(&path).map_err(|why| at(format!("cannot include `{written}`: {why}")))?;
    let canonical = canonical(&path);
    let name = shown(&path, &context.root_dir);
    if context.files.iter().any(|(file, _)| *file == canonical) {
        return Err(at(format!("include cycle: {}", context.chain_to(&name))));
    }
    if context.files.len() > MAX_DEPTH {
        return Err(at(format!(
            "includes nest more than {MAX_DEPTH} levels deep: {}",
            context.chain_to(&name)
        )));
    }
    let included = SourceInfo {
        base_dir: path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        path: path.clone(),
        kind,
    };
    tracing::debug!("including {} ({kind:?})", path.display());
    let inner = context.enter(canonical, &path);
    let (parsed, effective, nested) = parse_included(&included, &text, config, &inner)
        .map_err(|err| in_included_file(err, &path))?;
    let keys = differing_meta(&parsed.deck.meta, &context.root_meta);
    if !keys.is_empty() {
        let message = format!(
            "`{written}` sets {}; an included file's deck metadata is ignored (the top-level \
             deck's front matter applies)",
            and_list(&keys)
        );
        let span = SourceSpan {
            start_line: include.line,
            end_line: include.line,
            cell: None,
            file: None,
        };
        warnings.warn(codes::INCLUDE_META_IGNORED, message).span = Some(span);
    }
    Ok(assemble(parsed, path, kind, effective, nested))
}

/// The included file's parse turned into an [`Expanded`] (spans tagged with `path`).
fn assemble(
    parsed: Parsed,
    path: PathBuf,
    kind: SourceKind,
    config: Config,
    nested: Vec<IncludedSource>,
) -> Expanded {
    let Parsed {
        deck,
        assets,
        diagnostics,
    } = parsed;
    let mut slides = deck.slides;
    for slide in &mut slides {
        tag_span(&mut slide.span, &path);
        for child in &mut slide.children {
            tag_span(&mut child.span, &path);
        }
    }
    let diagnostics = diagnostics
        .into_iter()
        .map(|mut diagnostic| {
            if diagnostic.path.as_ref().is_none_or(|own| *own == path) {
                diagnostic.path = Some(path.clone());
                tag_span(&mut diagnostic.span, &path);
            }
            diagnostic
        })
        .collect();
    let mut includes = vec![path.clone()];
    includes.extend(deck.includes);
    let mut sources = vec![IncludedSource { path, kind, config }];
    sources.extend(nested);
    Expanded {
        slides,
        assets,
        features: deck.features,
        diagnostics,
        includes,
        sources,
    }
}

/// Sets `span.file` to `path` unless it names a file already (a nested include's).
fn tag_span(span: &mut Option<SourceSpan>, path: &Path) {
    if let Some(span) = span {
        span.file.get_or_insert_with(|| path.to_path_buf());
    }
}

/// Parses the included file `source` as a deck of its own: its deck, the config it was
/// parsed with and the files it includes in turn.
fn parse_included(
    source: &SourceInfo,
    text: &str,
    config: &Config,
    context: &Context,
) -> Result<(Parsed, Config, Vec<IncludedSource>)> {
    match source.kind {
        SourceKind::Markdown => {
            let outcome = super::parse_included(source, text, config, context)?;
            Ok((outcome.parsed, outcome.config, outcome.sources))
        }
        SourceKind::Notebook => {
            let (parsed, deferred) = crate::notebook::parse_deferring_config(source, text, config)?;
            if let Some(err) = deferred {
                tracing::debug!("ignored with the included notebook's metadata: {err}");
            }
            // What `check` needs to lint it like the frontend read it.
            let mut effective = config.clone();
            let tables = DeckMeta {
                vars: config_tables(&parsed.deck.meta.vars, &["markdown", "notebook"]),
                ..DeckMeta::default()
            };
            if let Err(err) = effective.merge_front_matter(&tables) {
                tracing::debug!("included notebook's config tables: {err}");
            }
            Ok((parsed, effective, Vec::new()))
        }
    }
}

/// The entries of `vars` named in `tables`.
fn config_tables(vars: &Map<String, Value>, tables: &[&str]) -> Map<String, Value> {
    vars.iter()
        .filter(|(key, _)| tables.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The frontend for an included file, by extension (as [`pipeline::detect_kind`]).
///
/// [`pipeline::detect_kind`]: crate::pipeline::detect_kind
fn included_kind(path: &Path) -> Option<SourceKind> {
    crate::pipeline::detect_kind(path).ok()
}

/// The text of the included file (UTF-8), or why it cannot be read.
fn read_included(path: &Path) -> std::result::Result<String, String> {
    let bytes = std::fs::read(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => {
            format!("file not found (looked for {})", path.display())
        }
        _ => format!("cannot read {}: {err}", path.display()),
    })?;
    String::from_utf8(bytes).map_err(|err| format!("not valid UTF-8 ({err})"))
}

/// An error of the included file `path`, located there: a front matter error (which names no
/// file, and would be located in the top-level deck) becomes an [`Error::Parse`] with `path`
/// and the line (and notebook cell) it gives; a parse error without a path gets `path`.
fn in_included_file(err: Error, path: &Path) -> Error {
    let message = match &err {
        Error::FrontMatter(_) => err.message().into_owned(),
        Error::Parse {
            path: empty,
            line,
            message,
        } if empty.as_os_str().is_empty() => {
            return Error::Parse {
                path: path.to_path_buf(),
                line: *line,
                message: message.clone(),
            };
        }
        _ => return err,
    };
    let (line, message) = match err.span() {
        Some(SourceSpan {
            cell: Some(cell),
            start_line,
            ..
        }) => (None, format!("cell {cell}, line {start_line}: {message}")),
        Some(span) => (Some(span.start_line), message),
        None => (None, message),
    };
    Error::Parse {
        path: path.to_path_buf(),
        line,
        message,
    }
}

/// The deck metadata keys `included` sets that differ from `root`'s (`title`, …,
/// `reveal.<key>`, other front matter keys), each in backticks; the config tables are not
/// deck metadata.
fn differing_meta(included: &DeckMeta, root: &DeckMeta) -> Vec<String> {
    let fields = [
        ("title", &included.title, &root.title),
        ("subtitle", &included.subtitle, &root.subtitle),
        ("author", &included.author, &root.author),
        ("date", &included.date, &root.date),
        ("description", &included.description, &root.description),
        ("lang", &included.lang, &root.lang),
        ("theme", &included.theme, &root.theme),
    ];
    let mut keys: Vec<String> = fields
        .into_iter()
        .filter(|(_, mine, theirs)| mine.is_some() && mine != theirs)
        .map(|(key, ..)| format!("`{key}`"))
        .collect();
    keys.extend(
        included
            .reveal
            .iter()
            .filter(|(key, value)| root.reveal.get(*key) != Some(value))
            .map(|(key, _)| format!("`reveal.{key}`")),
    );
    keys.extend(
        included
            .vars
            .iter()
            .filter(|(key, value)| {
                !CONFIG_TABLES.contains(&key.as_str()) && root.vars.get(*key) != Some(value)
            })
            .map(|(key, _)| format!("`{key}`")),
    );
    keys
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `path` relative to `dir` when it is below it, else as is.
fn shown(path: &Path, dir: &Path) -> String {
    path.strip_prefix(dir).unwrap_or(path).display().to_string()
}

/// `path` canonicalized (symbolic links resolved), or as is when that fails.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The files the Markdown deck `path` includes, transitively and in first-use order, as far
/// as they can be found: every `:::include` directive anywhere in the file (also one the
/// frontend rejects) whose path can be read, resolved against the file's directory, and in
/// turn the includes of each included Markdown file that can be read (to [`MAX_DEPTH`],
/// each file once). Files that do not exist are listed too. Best effort and without errors:
/// for the watcher of a deck that does not parse (`serve::watch`), so that fixing an included
/// file rebuilds. Nothing for a path that is not a Markdown file.
pub fn scan_includes(path: &Path) -> Vec<PathBuf> {
    let Ok(path) = std::path::absolute(path).map(|path| normalize_lexically(&path)) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    scan_file(&path, 1, &mut vec![path.clone()], &mut found);
    found
}

/// [`scan_includes`] of one file at `depth`; `seen` holds every file listed or scanned.
fn scan_file(path: &Path, depth: usize, seen: &mut Vec<PathBuf>, found: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || included_kind(path) != Some(SourceKind::Markdown) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let dir = path.parent().unwrap_or(Path::new("/"));
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.block_directive = true;
    options.extension.front_matter_delimiter = Some("---".into());
    let root = parse_document(&arena, &text, &options);
    let targets: Vec<PathBuf> = root
        .descendants()
        .filter_map(|node| match &node.data().value {
            NodeValue::BlockDirective(d) if is_include_info(&d.info) => include_path(&d.info).ok(),
            _ => None,
        })
        .map(|written| normalize_lexically(&dir.join(written)))
        .collect();
    for target in targets {
        if seen.contains(&target) {
            continue;
        }
        seen.push(target.clone());
        found.push(target.clone());
        scan_file(&target, depth + 1, seen, found);
    }
}

#[cfg(test)]
mod tests {
    use comrak::{Arena, parse_document};

    use super::*;
    use crate::markdown::comrak_options;
    use crate::markdown::split::{SplitMode, source_lines, split_with_source};

    /// `find_includes` on `text` (rules mode, or headings mode with `headings`): the include
    /// of each horizontal slide, or the error's line and message; and the warnings' codes.
    fn find(text: &str, headings: bool) -> (Result<Vec<Option<Include>>>, Vec<&'static str>) {
        let arena = Arena::new();
        let options = comrak_options(&Default::default());
        let root = parse_document(&arena, text, &options);
        let mode = match headings {
            true => SplitMode::Headings { slide_level: 2 },
            false => SplitMode::Rules,
        };
        let raw = split_with_source(root, text, mode).unwrap();
        let lines = source_lines(text);
        let mut diagnostics = Diagnostics::new();
        let found = find_includes(
            &raw.slides,
            &lines,
            Path::new("/d/deck.md"),
            headings,
            &mut diagnostics,
        );
        (found, diagnostics.iter().map(|d| d.code).collect())
    }

    fn include(line: usize, path: &str) -> Option<Include> {
        Some(Include {
            line,
            path: path.into(),
        })
    }

    /// The line and message of the error `find_includes` gives for `text`.
    fn misuse(text: &str, headings: bool) -> (usize, String) {
        match find(text, headings).0 {
            Err(Error::Parse {
                path,
                line: Some(line),
                message,
            }) => {
                assert_eq!(path, Path::new("/d/deck.md"));
                (line, message)
            }
            other => panic!("expected a parse error for {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn include_slides_are_found_in_both_modes() {
        let text = "# A\n\n---\n\n:::include b.md\n:::\n\n---\n\n<!-- c -->\n\n\
                    :::include \"my talk/part 2.md\"\n:::\n";
        let (found, codes) = find(text, false);
        assert_eq!(
            found.unwrap(),
            [None, include(5, "b.md"), include(12, "my talk/part 2.md")]
        );
        assert!(codes.is_empty());
        // headings mode: after `---`, and a heading after it starts the next slide
        let text = "# A\n\ntext\n\n---\n\n:::include 'b c.md'\n:::\n\n## B\n";
        let (found, _) = find(text, true);
        assert_eq!(found.unwrap(), [None, include(7, "b c.md"), None]);
        // a deck that starts with an include, closed by a longer fence
        let (found, _) = find("::::include x.ipynb\n::::\n", false);
        assert_eq!(found.unwrap(), [include(1, "x.ipynb")]);
    }

    #[test]
    fn attributes_and_notes_on_an_include_slide_are_ignored() {
        let text = "# A\n\n--- {.dark background=#000}\n\n:::include b.md\n:::\n\n\
                    :::notes\nN\n:::\n";
        let (found, codes) = find(text, false);
        assert_eq!(found.unwrap(), [None, include(5, "b.md")]);
        assert_eq!(codes, [codes::INCLUDE_ATTRS_IGNORED; 2]);
        let text = "# A\n\n---\n\n<!-- .slide: data-state=\"x\" -->\n\n:::include b.md\n:::\n";
        let (found, codes) = find(text, false);
        assert_eq!(found.unwrap(), [None, include(7, "b.md")]);
        assert_eq!(codes, [codes::INCLUDE_ATTRS_IGNORED]);
    }

    #[test]
    fn every_misuse_is_an_error_at_the_directive() {
        let cases: [(&str, bool, usize, &str); 12] = [
            (
                "# A\n\n:::include b.md\n:::\n",
                false,
                3,
                "line 1 has other content",
            ),
            (
                "# A\n\n:::include b.md\n:::\n",
                true,
                3,
                "unless the next slide starts with a heading",
            ),
            (
                "# A\n\n--\n\n:::include b.md\n:::\n",
                false,
                5,
                "vertical slide (the `--` on line 3",
            ),
            (
                "# A\n\n---\n\n:::include b.md\n:::\n\n--\n\nC\n",
                false,
                5,
                "(the `--` on line 8)",
            ),
            (
                "- item\n\n  :::include b.md\n  :::\n",
                false,
                3,
                "not inside a list",
            ),
            (
                "> :::include b.md\n> :::\n",
                false,
                1,
                "not inside a block quote",
            ),
            (
                "::::center\n:::include b.md\n:::\n::::\n",
                false,
                2,
                "the `:::center` directive",
            ),
            (
                ":::include b.md\nText\n:::\n",
                false,
                1,
                "line 2 is inside it",
            ),
            (
                ":::include b.md\n\n---\n\nmore\n",
                false,
                1,
                "is not closed",
            ),
            (":::include b.md\n", false, 1, "is not closed"),
            (":::include\n:::\n", false, 1, "needs the path"),
            (":::include a b.md\n:::\n", false, 1, "unexpected `b.md`"),
        ];
        for (text, headings, line, part) in cases {
            let (at, message) = misuse(text, headings);
            assert_eq!(at, line, "{text:?}: {message}");
            assert!(message.contains(part), "{text:?}: {message}");
        }
        let (at, message) = misuse(":::include x:y.md\n:::\n", false);
        assert_eq!(at, 1);
        assert!(message.contains("`&#58;`"), "{message}");
        let (_, message) = misuse(":::include https&#58;//e.org/a.md\n:::\n", false);
        assert!(message.contains("not URLs"), "{message}");
        let (_, message) = misuse(":::include \"a.md\n:::\n", false);
        assert!(message.contains("unbalanced"), "{message}");
        let (_, message) = misuse("::: {.include}\n:::\n", false);
        assert!(message.contains("write `:::include PATH`"), "{message}");
    }

    #[test]
    fn include_paths_are_read_like_the_dialect_says() {
        for (info, path) in [
            ("include b.md", "b.md"),
            ("include   ../x/b.ipynb  ", "../x/b.ipynb"),
            ("include \"a b/c d.md\"", "a b/c d.md"),
            ("include \"it's.md\"", "it's.md"),
            ("include 'say \"hi\".md'", "say \"hi\".md"),
            ("include /abs/deck.md", "/abs/deck.md"),
        ] {
            assert_eq!(include_path(info).as_deref(), Ok(path), "{info}");
        }
        for info in [
            "include",
            "include \"\"",
            "include 'a.md' b",
            "include \"a.md",
        ] {
            assert!(include_path(info).is_err(), "{info}");
        }
    }

    #[test]
    fn differing_metadata_is_named() {
        let mut root = DeckMeta {
            title: Some("T".into()),
            ..DeckMeta::default()
        };
        root.reveal.insert("transition".into(), "fade".into());
        let mut included = DeckMeta {
            title: Some("T".into()),
            author: Some("A".into()),
            theme: Some("dark".into()),
            ..DeckMeta::default()
        };
        included.reveal.insert("transition".into(), "fade".into());
        included.reveal.insert("controls".into(), false.into());
        included.vars.insert("venue".into(), "X".into());
        included
            .vars
            .insert("slides".into(), serde_json::json!({"split": "headings"}));
        assert_eq!(
            differing_meta(&included, &root),
            ["`author`", "`theme`", "`reveal.controls`", "`venue`"]
        );
        assert!(differing_meta(&root, &root).is_empty());
        assert!(differing_meta(&DeckMeta::default(), &root).is_empty());
    }

    #[test]
    fn front_matter_errors_are_located_in_the_included_file() {
        let path = Path::new("/d/part.md");
        let err = in_included_file(
            Error::FrontMatter("mapping values are not allowed at line 3 column 7".into()),
            path,
        );
        assert_eq!(err.path(), Some(path));
        assert_eq!(err.span().map(|span| span.start_line), Some(3));
        assert_eq!(err.column(), Some(7));
        let err = in_included_file(
            Error::FrontMatter("cell 2: expected a mapping at line 1 column 1".into()),
            Path::new("/d/nb.ipynb"),
        );
        let span = err.span().unwrap();
        assert_eq!((span.cell, span.start_line), (Some(2), 1));
        assert!(
            err.to_string().contains("front matter: expected a mapping"),
            "{err}"
        );
        let other = in_included_file(Error::Other("x".into()), path);
        assert!(matches!(other, Error::Other(_)));
    }
}
