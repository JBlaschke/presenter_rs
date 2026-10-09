//! Lint rules for Markdown decks and notebooks (`presenter check`, `docs/spec/cli.md`).
//!
//! [`run`] parses the deck without building it ([`pipeline::parse_only`]), resolves its
//! theme ([`pipeline::resolve_theme`]) and returns the frontend's diagnostics together with
//! the deck-level lints below, ordered by position (findings about the theme's files and
//! deck-level findings first, then by cell and line; a notebook output's file warning, which
//! has no line, after its cell's lines). Missing assets are never errors here.
//!
//! # What is reported
//!
//! Passed through from the frontends (all warnings):
//!
//! - Markdown (dialect §2–§7; T11, T12, T43): `empty-slide`, `orphan-vertical`,
//!   `separator-not-recognized` (lines that look like `--- {…}` separators but are not),
//!   `unknown-slide-attr`, `invalid-slide-attr`, `missing-asset`, `invalid-directive`,
//!   `invalid-directive-arg`, `duplicate-footnote`, `footnote-elsewhere`,
//!   `misplaced-slide-comment`, `element-comment-ignored`, `invalid-element-attr`.
//! - Notebooks (notebook-mapping §1–§5; T30–T32): `unknown-slide-type`, `unknown-cell-type`,
//!   `nbformat-newer`, `presenter-not-object`, `notebook-output-skipped`,
//!   `notebook-attachment-skipped`, `empty-slide`, `unknown-slide-attr`, `invalid-slide-attr`,
//!   `presenter-override-ignored`, `presenter-override-invalid`, `raw-cell-skipped`,
//!   `missing-attachment`, `missing-asset`, `front-matter-ignored`, `front-matter-empty`,
//!   `output-script`, `output-unsupported`, `output-invalid`, `output-widget` (§4
//!   "interactive widget rendered as text"), `output-file-outside` (an output names a file
//!   outside the notebook's directory, which is not embedded) and `notebook-single-slide`
//!   (no cell has a slide type, so the whole notebook is one slide).
//!
//! Added here ([`codes`]):
//!
//! - `separator-not-recognized` (dialect §2.2, §2.3 and §10): a raw source line that is `---`
//!   or `--` (ignoring surrounding spaces and tabs) but did not open a slide, with the reason
//!   and the fix (setext heading, indented code, part of a paragraph, text after a link
//!   reference definition, a lazy continuation of a list or quote, a table row, inside a
//!   directive, an unclosed fence or comment, …). Whether a line opened a slide is the
//!   splitter's own decision: the text is split with [`split_with_source`], as the frontend
//!   splits it, and a line opened a slide when it is the line of a separator the splitter
//!   took (a top-level node no slide holds). Skipped: the front matter, closed fenced code
//!   blocks, closed raw HTML (comments, `<pre>`/`<script>`/`<style>`/`<textarea>`, …),
//!   indented code blocks with other lines than `---`/`--` (code meant to be shown), and
//!   lines the splitter already reported. In headings mode (§2.3, where separators are still
//!   honored) the underline of a setext heading (`Title` / `---`, level 2) that is a slide
//!   boundary (`slides.slide_level` 2 or more) is a real slide heading and is not reported;
//!   every other pitfall is. In notebooks the markdown cells are scanned when
//!   `[notebook] split_markdown_cells` is on (then `---`/`--` open slides there), except
//!   speaker-notes cells.
//! - `unknown-directive` (dialect §4): `:::include`, which is reserved and not implemented,
//!   in a Markdown deck or in a shown markdown cell.
//! - `duplicate-slide-id`: a slide `id` used by an earlier slide.
//! - `unknown-layout` (theme-format §2 `[layouts]`): a slide's `layout` is not a
//!   `templates/layouts/<name>.html` file of the theme chain ([`Theme::has_layout`]).
//! - `notebook-no-outputs` (notebook-mapping §4 notes): a notebook whose code cells were never
//!   executed (no outputs, no execution count), with a hint to execute it.
//! - `theme-error` (level error): the deck's theme cannot be resolved; `unknown-layout` and the
//!   theme problems below are then skipped.
//! - Theme problems a build fails on ([`Theme::problems`], level error, located in the
//!   theme's `theme.toml` or template; audit F10): `theme-style-entry`, `theme-script-entry`
//!   (a missing entry, or one outside its theme), `theme-highlight-style` (a `highlight_style`
//!   file that is missing or outside its theme), `theme-template` (a template of a disk theme
//!   that does not compile) and `theme-plugins` (an invalid merged `[reveal] plugins` list).
//!   They are the build failures `presenter theme check` reports; a deck whose build fails on
//!   its theme does not pass `check`. Stylesheets are not compiled here.
//!
//! Failures that leave no deck to lint (the input cannot be read or parsed, invalid front
//! matter or `presenter.toml`) are [`run`]'s `Err`; [`error_diagnostic`] turns them into an
//! error-level [`Diagnostic`] so that `check` can report them like any other finding.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::assets::registry::normalize_lexically;
use crate::config::{Config, SlideSplit};
use crate::error::{Diagnostic, Diagnostics, Error, Result};
use crate::ir::{Block, Deck, Slide, SourceKind, SourceSpan};
use crate::markdown::comrak_options;
use crate::markdown::frontmatter::split_front_matter;
use crate::markdown::split::{SplitMode, source_lines, split_with_source};
use crate::notebook::convert::codes::FRONT_MATTER_IGNORED;
use crate::notebook::nbformat::{self, Cell, CellType, SlideType};
use crate::notebook::outputs::output_position;
use crate::pipeline::{self, BuildOptions};
use crate::theme::Theme;

/// The diagnostic codes this module adds to the frontends' (module docs).
pub mod codes {
    /// A raw `---`/`--` line that did not become a slide separator (warning).
    pub const SEPARATOR_NOT_RECOGNIZED: &str = "separator-not-recognized";
    /// `:::include`, reserved and not implemented (warning).
    pub const UNKNOWN_DIRECTIVE: &str = "unknown-directive";
    /// A slide id that an earlier slide already uses (warning).
    pub const DUPLICATE_SLIDE_ID: &str = "duplicate-slide-id";
    /// A `layout` the theme chain has no template for (warning).
    pub const UNKNOWN_LAYOUT: &str = "unknown-layout";
    /// A notebook whose code cells were never executed (warning).
    pub const NOTEBOOK_NO_OUTPUTS: &str = "notebook-no-outputs";
    /// The deck's theme cannot be resolved or loaded (error).
    pub const THEME_ERROR: &str = "theme-error";
    /// The input cannot be read ([`crate::Error::Io`]; error).
    pub const IO_ERROR: &str = "io-error";
    /// The input cannot be parsed ([`crate::Error::Parse`], [`crate::Error::Json`]; error).
    pub const PARSE_ERROR: &str = "parse-error";
    /// Invalid front matter ([`crate::Error::FrontMatter`]; error).
    pub const FRONT_MATTER_ERROR: &str = "front-matter-error";
    /// Invalid configuration ([`crate::Error::Config`]; error).
    pub const CONFIG_ERROR: &str = "config-error";
    /// A template failed ([`crate::Error::Template`]; error).
    pub const TEMPLATE_ERROR: &str = "template-error";
    /// A stylesheet failed ([`crate::Error::Style`]; error).
    pub const STYLE_ERROR: &str = "style-error";
    /// A missing asset that was escalated to [`crate::Error::MissingAsset`] (error).
    pub const MISSING_ASSET: &str = "missing-asset";
    /// Any other failure ([`crate::Error::Other`], e.g. an unknown input extension; error).
    pub const ERROR: &str = "error";
}

/// Lints `opts.input` (see the module docs): the frontend's diagnostics plus this module's
/// lints, ordered by position. `opts.output`, `opts.build` and
/// `opts.missing_asset_is_error` are ignored (nothing is rendered).
///
/// Errors: everything [`pipeline::parse_only`] fails on (the input cannot be read, an
/// unknown extension, invalid `presenter.toml` or front matter, the frontend's parse
/// errors). A theme that cannot be resolved is not an error here but a `theme-error`
/// diagnostic.
pub fn run(opts: &BuildOptions) -> Result<Diagnostics> {
    let (parsed, config, config_path) = pipeline::parse_only(opts)?;
    let deck = parsed.deck;
    let mut found = source_lints(opts, &deck, &config, &parsed.diagnostics);
    found.extend(duplicate_slide_ids(&deck));
    found.extend(notebook_without_outputs(&deck));
    let mut theme_problems = Vec::new();
    match pipeline::resolve_theme(opts, &deck, &config, config_path.as_deref()) {
        Ok(theme) => {
            found.extend(unknown_layouts(&deck, &theme));
            theme_problems = theme.problems();
        }
        Err(err) => found.push(Diagnostic::error(codes::THEME_ERROR, err.to_string())),
    }
    let mut diagnostics = parsed.diagnostics.into_vec();
    diagnostics.extend(
        found
            .into_iter()
            .map(|diagnostic| diagnostic.with_path(&deck.source.path)),
    );
    // Theme problems keep the theme file they are about (a built-in theme's have none).
    diagnostics.extend(theme_problems.into_iter().map(|mut diagnostic| {
        diagnostic
            .path
            .get_or_insert_with(|| deck.source.path.clone());
        diagnostic
    }));
    sort_by_position(&mut diagnostics, &deck.source.path);
    tracing::debug!(
        diagnostics = diagnostics.len(),
        "checked {}",
        deck.source.path.display()
    );
    Ok(diagnostics.into())
}

/// Orders `diagnostics` (stable, so ties keep the frontend's order before this module's):
/// findings about other files (theme problems) and deck-level ones (no position) first,
/// then by cell and line; a notebook output's diagnostic, which has no line, after its
/// cell's lines ([`output_position`]).
fn sort_by_position(diagnostics: &mut [Diagnostic], deck: &Path) {
    diagnostics.sort_by_key(|d| {
        if d.path.as_deref() != Some(deck) {
            return None;
        }
        match &d.span {
            Some(span) => Some((span.cell, span.start_line, 0)),
            None => {
                output_position(&d.message).map(|(cell, output)| (Some(cell), usize::MAX, output))
            }
        }
    });
}

/// `err` (a failure of [`run`] for `input`) as an error-level diagnostic for `input`: the
/// code names the kind of failure ([`codes`]), the span is the error's line when it has one.
/// The path is `input` made absolute, like the frontends' diagnostic paths.
pub fn error_diagnostic(err: &Error, input: &Path) -> Diagnostic {
    let (code, message, span) = match err {
        Error::Parse { line, message, .. } => (
            codes::PARSE_ERROR,
            message.clone(),
            line.map(|line| SourceSpan {
                start_line: line,
                end_line: line,
                cell: None,
            }),
        ),
        Error::MissingAsset { span, .. } => (codes::MISSING_ASSET, err.to_string(), span.clone()),
        _ => (error_code(err), err.to_string(), None),
    };
    let mut diagnostic = Diagnostic::error(code, message).with_path(absolute(input));
    diagnostic.span = span;
    diagnostic
}

/// The [`codes`] entry for `err` (other than `Parse` and `MissingAsset`, which carry a span).
fn error_code(err: &Error) -> &'static str {
    match err {
        Error::Io(_) => codes::IO_ERROR,
        Error::Parse { .. } | Error::Json(_) => codes::PARSE_ERROR,
        Error::FrontMatter(_) => codes::FRONT_MATTER_ERROR,
        Error::Config(_) => codes::CONFIG_ERROR,
        Error::Theme { .. } => codes::THEME_ERROR,
        Error::Template { .. } => codes::TEMPLATE_ERROR,
        Error::Style { .. } => codes::STYLE_ERROR,
        Error::MissingAsset { .. } => codes::MISSING_ASSET,
        Error::Other(_) => codes::ERROR,
    }
}

/// `path` absolute and lexically normalized (unchanged when the current directory is
/// unknown).
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path)
        .map(|abs| normalize_lexically(&abs))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Every slide of `deck` with its label (`slide 3`, `slide 3.1`), in document order.
fn labelled_slides(deck: &Deck) -> Vec<(String, &Slide)> {
    let mut slides = Vec::new();
    for (i, slide) in deck.slides.iter().enumerate() {
        slides.push((format!("slide {}", i + 1), slide));
        for (j, child) in slide.children.iter().enumerate() {
            slides.push((format!("slide {}.{}", i + 1, j + 1), child));
        }
    }
    slides
}

/// A warning about `slide` (its span, no path yet).
fn slide_warning(code: &'static str, message: String, slide: &Slide) -> Diagnostic {
    let mut diagnostic = Diagnostic::warning(code, message);
    diagnostic.span = slide.span.clone();
    diagnostic
}

/// Where `span` starts, for messages: `line 12`, or `cell 3, line 2` in a notebook.
fn position(span: Option<&SourceSpan>) -> Option<String> {
    let span = span?;
    Some(match span.cell {
        Some(cell) => format!("cell {cell}, line {}", span.start_line),
        None => format!("line {}", span.start_line),
    })
}

/// `duplicate-slide-id`: every slide whose id an earlier slide already has.
fn duplicate_slide_ids(deck: &Deck) -> Vec<Diagnostic> {
    let mut first: BTreeMap<&str, (String, Option<&SourceSpan>)> = BTreeMap::new();
    let mut found = Vec::new();
    for (label, slide) in labelled_slides(deck) {
        let Some(id) = slide.id.as_deref() else {
            continue;
        };
        match first.get(id) {
            None => {
                first.insert(id, (label, slide.span.as_ref()));
            }
            Some((earlier, span)) => {
                let at = position(*span).map_or(String::new(), |at| format!(" ({at})"));
                let message = format!(
                    "{label}: id `{id}` is already the id of {earlier}{at}; links to `#/{id}` \
                     go to the first one"
                );
                found.push(slide_warning(codes::DUPLICATE_SLIDE_ID, message, slide));
            }
        }
    }
    found
}

/// `unknown-layout`: every slide whose layout `theme` does not provide. The message is the
/// renderer's ([`Theme::unknown_layout_message`], with a "did you mean" for a near miss), so
/// `check` and `build` agree (a test compares them). Each distinct layout name is looked up
/// once.
fn unknown_layouts(deck: &Deck, theme: &Theme) -> Vec<Diagnostic> {
    let mut known: BTreeMap<&str, bool> = BTreeMap::new();
    let mut found = Vec::new();
    for (label, slide) in labelled_slides(deck) {
        let layout = slide.layout.as_str();
        if *known
            .entry(layout)
            .or_insert_with(|| theme.has_layout(layout))
        {
            continue;
        }
        let message = theme.unknown_layout_message(&label, layout);
        found.push(slide_warning(codes::UNKNOWN_LAYOUT, message, slide));
    }
    found
}

/// `notebook-no-outputs`: a notebook deck with code cells that were never executed: none
/// has an output or an execution count (executed cells that print nothing are fine).
fn notebook_without_outputs(deck: &Deck) -> Option<Diagnostic> {
    if deck.source.kind != SourceKind::Notebook {
        return None;
    }
    let cells: Vec<_> = labelled_slides(deck)
        .into_iter()
        .flat_map(|(_, slide)| &slide.blocks)
        .filter_map(|block| match block {
            Block::Cell(cell) => Some(cell),
            Block::Html { .. } => None,
        })
        .collect();
    if cells.is_empty()
        || cells
            .iter()
            .any(|cell| !cell.outputs.is_empty() || cell.execution_count.is_some())
    {
        return None;
    }
    let file = deck
        .source
        .path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let finding = match cells.len() {
        1 => "the notebook's code cell has no outputs".to_string(),
        n => format!("none of the notebook's {n} code cells has outputs"),
    };
    Some(Diagnostic::warning(
        codes::NOTEBOOK_NO_OUTPUTS,
        format!(
            "{finding}; execute the notebook and save it before presenting (e.g. \
             `jupyter nbconvert --to notebook --execute --inplace {}`)",
            shell_quoted(&file)
        ),
    ))
}

/// `word` as one POSIX shell word: unchanged when it only has characters that need no
/// quoting, else in single quotes (a `'` inside becomes `'\''`).
fn shell_quoted(word: &str) -> Cow<'_, str> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./+,:@%=".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        Cow::Borrowed(word)
    } else {
        Cow::Owned(format!("'{}'", word.replace('\'', r"'\''")))
    }
}

/// The lints that need the source text: `separator-not-recognized` and `unknown-directive`.
///
/// The text is read again from `opts.input`, the path [`pipeline::parse_only`] read (not
/// `deck.source.path`: that is normalized lexically and names another file when the input
/// goes through a symlinked directory and `..`). If it can no longer be read or parsed (it
/// changed in between), that is an error diagnostic and these lints are skipped.
fn source_lints(
    opts: &BuildOptions,
    deck: &Deck,
    config: &Config,
    frontend: &Diagnostics,
) -> Vec<Diagnostic> {
    let text = match std::fs::read_to_string(&opts.input) {
        Ok(text) => text,
        Err(err) => {
            let message = format!(
                "cannot read {} again for the source lints: {err}",
                opts.input.display()
            );
            return vec![Diagnostic::error(codes::IO_ERROR, message)];
        }
    };
    match deck.source.kind {
        SourceKind::Markdown => markdown_lints(&text, config, frontend),
        SourceKind::Notebook => notebook_lints(&text, &deck.source.path, config, frontend)
            .unwrap_or_else(|err| {
                let message =
                    format!("cannot parse the notebook again for the source lints: {err}");
                vec![Diagnostic::error(codes::PARSE_ERROR, message)]
            }),
    }
}

/// The source lints of a Markdown deck. `config` is the deck's (front matter merged),
/// `frontend` the frontend's diagnostics.
fn markdown_lints(text: &str, config: &Config, frontend: &Diagnostics) -> Vec<Diagnostic> {
    // As the frontend splits it (`markdown::split_mode`).
    let split = match config.slides.split {
        SlideSplit::Rules => SplitMode::Rules,
        SlideSplit::Headings => SplitMode::Headings {
            slide_level: config.slides.slide_level,
        },
    };
    let options = comrak_options(&config.markdown);
    lint_text(text, &options, Origin::DECK, Some(split), frontend)
}

/// The source lints of the shown markdown cells of a notebook (`text` is its JSON):
/// `unknown-directive` in each, and with `[notebook] split_markdown_cells` (`---`/`--` open
/// slides there, as in a deck) `separator-not-recognized` in each that is not speaker notes.
/// A cell's front matter block is left out as the frontend leaves it out: always in the
/// first markdown cell (notebook-mapping §5), in another one when the frontend reported it
/// as `front-matter-ignored`.
fn notebook_lints(
    text: &str,
    path: &Path,
    config: &Config,
    frontend: &Diagnostics,
) -> Result<Vec<Diagnostic>> {
    let (notebook, _) = nbformat::parse_notebook(path, text)?;
    let mut options = comrak_options(&config.markdown);
    // As the frontend parses cells: front matter is removed beforehand (below).
    options.extension.front_matter_delimiter = None;
    let first_markdown = notebook
        .cells
        .iter()
        .position(|cell| cell.cell_type == CellType::Markdown);
    let mut found = Vec::new();
    for (index, cell) in notebook.cells.iter().enumerate() {
        if cell.cell_type != CellType::Markdown || is_removed(cell, config) {
            continue;
        }
        let ignored = frontend.iter().any(|d| {
            d.code == FRONT_MATTER_IGNORED && d.span.as_ref().and_then(|s| s.cell) == Some(index)
        });
        let (body, offset) = cell_markdown(cell, ignored || first_markdown == Some(index));
        let split =
            (config.notebook.split_markdown_cells && !is_notes(cell)).then_some(SplitMode::Rules);
        let origin = Origin {
            cell: Some(index),
            offset,
        };
        found.extend(lint_text(body, &options, origin, split, frontend));
    }
    Ok(found)
}

/// Whether the frontend leaves `cell` out entirely (notebook-mapping §2–§3: slide type
/// `skip` or one of the configured `remove_cell_tags`).
fn is_removed(cell: &Cell, config: &Config) -> bool {
    cell.slide_type() == SlideType::Skip
        || cell
            .tags()
            .iter()
            .any(|tag| config.notebook.remove_cell_tags.contains(tag))
}

/// Whether `cell` is speaker notes (slide type `notes` or the tag `notes`,
/// notebook-mapping §3); notes cells are never split into slides.
fn is_notes(cell: &Cell) -> bool {
    cell.slide_type() == SlideType::Notes || cell.tags().iter().any(|tag| tag == "notes")
}

/// The Markdown of a markdown cell as the frontend renders it, and the number of lines
/// removed before it: its leading `---` block when `front_matter` is set.
fn cell_markdown(cell: &Cell, front_matter: bool) -> (&str, usize) {
    let source = cell.source.as_str();
    match split_front_matter(source).filter(|_| front_matter) {
        Some((_, rest, consumed)) => (rest, consumed),
        None => (source, 0),
    }
}

/// Where a Markdown text sits in the input: the whole deck, or a notebook cell whose text
/// starts after `offset` removed lines (its front matter).
#[derive(Debug, Clone, Copy)]
struct Origin {
    /// The notebook cell, `None` for a Markdown deck.
    cell: Option<usize>,
    /// Lines of the input (cell) before line 1 of the text.
    offset: usize,
}

impl Origin {
    /// A whole Markdown deck.
    const DECK: Origin = Origin {
        cell: None,
        offset: 0,
    };

    /// A warning at `line` of the text (no path yet).
    fn warning(self, code: &'static str, line: usize, message: String) -> Diagnostic {
        let line = line + self.offset;
        Diagnostic::warning(code, message).with_span(SourceSpan {
            start_line: line,
            end_line: line,
            cell: self.cell,
        })
    }

    /// The lines of the text that the frontend already reported as
    /// `separator-not-recognized`.
    fn reported(self, frontend: &Diagnostics) -> BTreeSet<usize> {
        frontend
            .iter()
            .filter(|d| d.code == codes::SEPARATOR_NOT_RECOGNIZED)
            .filter_map(|d| d.span.as_ref())
            .filter(|span| span.cell == self.cell)
            .filter_map(|span| span.start_line.checked_sub(self.offset))
            .collect()
    }
}

/// `unknown-directive` and, when `split` is given (the text is split into slides that way:
/// the deck's mode, or rules mode for a split notebook cell), `separator-not-recognized` for
/// one Markdown text at `origin`.
fn lint_text(
    text: &str,
    options: &Options<'_>,
    origin: Origin,
    split: Option<SplitMode>,
    frontend: &Diagnostics,
) -> Vec<Diagnostic> {
    let arena = Arena::new();
    let root = parse_document(&arena, text, options);
    let mut found: Vec<Diagnostic> = include_directives(root)
        .into_iter()
        .map(|line| origin.warning(codes::UNKNOWN_DIRECTIVE, line, INCLUDE_MESSAGE.into()))
        .collect();
    let Some(split) = split else {
        return found;
    };
    let lines = source_lines(text);
    let Some(opened) = opened_lines(root, text, &lines, split) else {
        return found;
    };
    let reported = origin.reported(frontend);
    found.extend(
        unrecognized_separators(root, &lines, split, &opened, &reported)
            .into_iter()
            .map(|(line, message)| origin.warning(codes::SEPARATOR_NOT_RECOGNIZED, line, message)),
    );
    found
}

/// The lines on which a separator opened a slide when `text` (parsed as `root`, split into
/// `lines` by [`source_lines`]) is split in `mode`: the splitter's own decision. Every
/// top-level node that no slide holds was taken by the splitter (a separator, the front
/// matter or a `.slide:` comment); a thematic break opened a slide on its line, a separator
/// paragraph on its [`separator_line`].
///
/// `None` when the splitter fails (an unbalanced quote in a separator's `{…}` or a `.slide:`
/// comment), which the frontend already failed on, so only if the input changed in between.
fn opened_lines<'a>(
    root: &'a AstNode<'a>,
    text: &str,
    lines: &[&str],
    mode: SplitMode,
) -> Option<BTreeSet<usize>> {
    let deck = match split_with_source(root, text, mode) {
        Ok(deck) => deck,
        Err(err) => {
            tracing::debug!("separator lint skipped, the text does not split: {err}");
            return None;
        }
    };
    let held: BTreeSet<*const AstNode<'a>> = deck
        .slides
        .iter()
        .flat_map(|(slide, children)| std::iter::once(slide).chain(children))
        .flat_map(|slide| slide.nodes.iter().map(|node| std::ptr::from_ref(*node)))
        .collect();
    let opened = root
        .children()
        .filter(|node| !held.contains(&std::ptr::from_ref(*node)))
        .filter_map(|node| {
            let (start, end) = lines_of(node);
            match node.data().value {
                NodeValue::ThematicBreak => Some(start),
                NodeValue::Paragraph => Some(separator_line(lines, end)),
                _ => None,
            }
        })
        .collect();
    Some(opened)
}

/// The line of a separator paragraph that ends on `end` (dialect §2.2, `--` or
/// `^(---|--)\s*\{(.*)\}$` with the lines trimmed and joined by `\n`): the line before the
/// last when the last one starts with `{` (`--` and `{…}` on two lines, as `\s*` allows),
/// else the last one (`--`, or `-- {…}` on one line). Lines before the separator's own are
/// link reference definitions.
fn separator_line(lines: &[&str], end: usize) -> usize {
    let last = end
        .checked_sub(1)
        .and_then(|index| lines.get(index))
        .map_or("", |line| line.trim_matches([' ', '\t']));
    if last.starts_with('{') && end > 1 {
        end - 1
    } else {
        end
    }
}

/// The `unknown-directive` message for `:::include`.
const INCLUDE_MESSAGE: &str = "unknown directive 'include': including files is not \
                               implemented, so nothing is included (the directive renders as \
                               a plain <div>)";

/// The first and last source line of `node`.
fn lines_of<'a>(node: &'a AstNode<'a>) -> (usize, usize) {
    let pos = node.data().sourcepos;
    (pos.start.line, pos.end.line)
}

/// The line of every `:::include` (dialect §4: reserved, not implemented).
fn include_directives<'a>(root: &'a AstNode<'a>) -> Vec<usize> {
    root.descendants()
        .filter(|node| match &node.data().value {
            NodeValue::BlockDirective(directive) => {
                directive.info.split_whitespace().next() == Some("include")
            }
            _ => false,
        })
        .map(|node| lines_of(node).0)
        .collect()
}

/// Disjoint line ranges with a value each, sorted for lookups by line in O(log n).
struct Ranges<T>(Vec<((usize, usize), T)>);

impl<T: Copy> Ranges<T> {
    fn new(mut ranges: Vec<((usize, usize), T)>) -> Self {
        ranges.sort_by_key(|(range, _)| *range);
        Ranges(ranges)
    }

    /// The value of the range that contains `line`, if any.
    fn find(&self, line: usize) -> Option<T> {
        let after = self.0.partition_point(|((start, _), _)| *start <= line);
        let ((_, end), value) = *self.0.get(after.checked_sub(1)?)?;
        (line <= end).then_some(value)
    }
}

/// A raw `---`/`--` line that opened no slide.
struct DashLine<'s> {
    /// Its line number in the text.
    line: usize,
    /// `---` or `--`.
    mark: &'s str,
    /// The line as written.
    raw: &'s str,
    /// It is indented by 4 or more columns (a tab counts to the next multiple of 4): it can
    /// never be a separator, only a paragraph continuation line or code.
    indented: bool,
    /// The line before it is blank, or there is none.
    blank_before: bool,
    /// The line after it is blank, or there is none.
    blank_after: bool,
}

impl DashLine<'_> {
    /// The kind of slide the line would start.
    fn direction(&self) -> &'static str {
        if self.mark == "---" {
            "horizontal"
        } else {
            "vertical"
        }
    }

    /// How to make the line a separator of its own: no indentation of 4 or more columns, a
    /// blank line before it when text is above it, and for `--` (which must be a paragraph
    /// of its own) after it when text follows.
    fn advice(&self) -> String {
        let after = self.mark == "--" && !self.blank_after;
        let what = match (!self.blank_before, after) {
            (true, false) => "put a blank line before it",
            (false, true) => "put a blank line after it",
            _ => "put blank lines around it",
        };
        let unindent = if self.indented {
            "remove its indentation and "
        } else {
            ""
        };
        format!("{unindent}{what} to start a {} slide", self.direction())
    }
}

/// The columns of `raw`'s leading spaces and tabs, a tab advancing to the next multiple of
/// 4 (CommonMark's tab stops).
fn indent_columns(raw: &str) -> usize {
    let mut columns = 0;
    for c in raw.chars() {
        match c {
            ' ' => columns += 1,
            '\t' => columns += 4 - columns % 4,
            _ => break,
        }
    }
    columns
}

/// `separator-not-recognized` for every raw `---`/`--` line of a Markdown text split in
/// `split` that did not open a slide (module docs), as `(line, message)` with lines of the
/// text. `lines` are the text's lines ([`source_lines`]), `opened` the lines on which a
/// separator opened a slide ([`opened_lines`]), `reported` the lines the splitter already
/// reported.
fn unrecognized_separators<'a>(
    root: &'a AstNode<'a>,
    lines: &[&str],
    split: SplitMode,
    opened: &BTreeSet<usize>,
    reported: &BTreeSet<usize>,
) -> Vec<(usize, String)> {
    let skipped = Ranges::new(skipped_ranges(root));
    let tops = Ranges::new(root.children().map(|node| (lines_of(node), node)).collect());
    let blank = |index: usize| lines.get(index).is_none_or(|line| line.trim().is_empty());
    let mut found = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let line = index + 1;
        let mark = raw.trim_matches([' ', '\t']);
        if !matches!(mark, "---" | "--")
            || opened.contains(&line)
            || reported.contains(&line)
            || skipped.find(line).is_some()
        {
            continue;
        }
        let dash = DashLine {
            line,
            mark,
            raw,
            indented: indent_columns(raw) >= 4,
            blank_before: index == 0 || blank(index - 1),
            blank_after: blank(index + 1),
        };
        if let Some(reason) = why_not_a_separator(root, tops.find(line), &dash, split) {
            found.push((line, format!("`{mark}` {reason}")));
        }
    }
    found
}

/// Line ranges whose `---`/`--` lines are never meant as separators: the front matter,
/// closed fenced code blocks, indented code blocks that are code listings, and closed raw
/// HTML blocks (comments, `<pre>`, `<script>`, …), at any depth. They are leaf blocks, so the
/// ranges are disjoint.
fn skipped_ranges<'a>(root: &'a AstNode<'a>) -> Vec<((usize, usize), ())> {
    root.descendants()
        .filter(|node| match &node.data().value {
            NodeValue::FrontMatter(_) => true,
            NodeValue::CodeBlock(code) if code.fenced => code.closed,
            NodeValue::CodeBlock(code) => is_code_listing(&code.literal),
            NodeValue::HtmlBlock(html) => is_closed_raw_html(html.block_type, &html.literal),
            _ => false,
        })
        .map(|node| (lines_of(node), ()))
        .collect()
}

/// True for an indented code block with a line other than `---`/`--`: code meant to be
/// shown (say, a front matter example), not a separator indented by mistake.
fn is_code_listing(literal: &str) -> bool {
    literal
        .lines()
        .map(|line| line.trim_matches([' ', '\t']))
        .any(|line| !line.is_empty() && !matches!(line, "---" | "--"))
}

/// True for a raw HTML block of CommonMark type 1–5 (`<pre>`/`<script>`/`<style>`/
/// `<textarea>`, a comment, a processing instruction, a declaration, CDATA) whose end marker
/// is present.
fn is_closed_raw_html(block_type: u8, literal: &str) -> bool {
    match block_type {
        1 => {
            let lower = literal.to_ascii_lowercase();
            ["</pre>", "</script>", "</style>", "</textarea>"]
                .iter()
                .any(|end| lower.contains(end))
        }
        2 => literal.contains("-->"),
        3 => literal.contains("?>"),
        4 => literal.contains('>'),
        5 => literal.contains("]]>"),
        _ => false,
    }
}

/// What a raw HTML block of type 1–5 is called in messages, and its end marker.
fn raw_html_kind(block_type: u8, literal: &str) -> Option<(String, &'static str)> {
    Some(match block_type {
        1 => {
            let lower = literal.trim_start().to_ascii_lowercase();
            let (tag, end) = [
                ("pre", "</pre>"),
                ("script", "</script>"),
                ("style", "</style>"),
                ("textarea", "</textarea>"),
            ]
            .into_iter()
            .find(|(tag, _)| lower.starts_with(&format!("<{tag}")))?;
            (format!("`<{tag}>` block"), end)
        }
        2 => ("HTML comment".to_string(), "-->"),
        3 => ("processing instruction".to_string(), "?>"),
        4 => ("declaration".to_string(), ">"),
        5 => ("CDATA section".to_string(), "]]>"),
        _ => return None,
    })
}

/// The innermost node below `node` whose lines contain `line` (`node` itself if none).
fn deepest<'a>(node: &'a AstNode<'a>, line: usize) -> &'a AstNode<'a> {
    let contains = |child: &&'a AstNode<'a>| {
        let (start, end) = lines_of(child);
        start <= line && line <= end
    };
    let mut node = node;
    while let Some(child) = node.children().find(contains) {
        node = child;
    }
    node
}

/// Why the dash line inside the top-level block `top` opened no slide, or `None` when that
/// is right (the underline of a setext heading that starts a slide in headings mode).
fn why_not_a_separator<'a>(
    root: &'a AstNode<'a>,
    top: Option<&'a AstNode<'a>>,
    dash: &DashLine<'_>,
    split: SplitMode,
) -> Option<String> {
    if let Some(reason) = never_closed(deepest(top.unwrap_or(root), dash.line)) {
        return Some(reason);
    }
    let Some(top) = top else {
        return Some("did not start a slide".to_string());
    };
    let (opened, end) = lines_of(top);
    let advice = dash.advice();
    Some(match &top.data().value {
        NodeValue::Heading(heading) if heading.setext && dash.line == end => {
            if is_slide_boundary(heading.level, split) {
                return None;
            }
            format!(
                "under a line of text turns that text into a heading (setext), so no slide \
                 starts here; {advice}"
            )
        }
        NodeValue::Heading(heading) if heading.setext => format!(
            "is part of the text of the heading underlined on line {end} (setext), so no \
             slide starts here; {advice}"
        ),
        NodeValue::CodeBlock(code) if !code.fenced => format!(
            "is indented by 4 or more columns (a tab counts as 4), so it is code and no slide \
             starts here; remove the indentation to start a {} slide",
            dash.direction()
        ),
        NodeValue::Paragraph if dash.line == end && opened < end && has_one_inline(top) => {
            // The paragraph's text is this line alone: the lines above it are link
            // reference definitions, which comrak removes from the paragraph.
            format!(
                "follows link reference definitions (from line {opened}) without a blank line, \
                 so it is read as text and no slide starts here; {advice}"
            )
        }
        NodeValue::Paragraph => {
            format!("is part of a paragraph, so no slide starts here; {advice}")
        }
        NodeValue::Table(_) => format!(
            "is read as a row of the table that starts on line {opened}, so no slide starts \
             here; {advice}"
        ),
        NodeValue::HtmlBlock(_) => format!(
            "is part of the HTML block that starts on line {opened} (it ends at the first \
             blank line), so no slide starts here; {advice}"
        ),
        value => nested(value, opened, dash),
    })
}

/// Whether a top-level heading of `level` is a slide boundary when the text is split in
/// `split`: headings mode and `level <= slides.slide_level` (dialect §2.3; the splitter's
/// rule). In rules mode, and for deeper headings, it is content.
fn is_slide_boundary(level: u8, split: SplitMode) -> bool {
    matches!(split, SplitMode::Headings { slide_level } if level <= slide_level)
}

/// Whether the block `node` has exactly one inline child. For a paragraph that ends on a
/// raw `---`/`--` line this means its text is that line alone: inlines never span a line
/// break without a delimiter on the next line, and such a line has none.
fn has_one_inline<'a>(node: &'a AstNode<'a>) -> bool {
    node.first_child()
        .is_some_and(|child| child.next_sibling().is_none())
}

/// The reason for a dash line inside `leaf` when that is a fenced code block or a raw HTML
/// block whose end marker is missing (it then runs to the end of its container).
fn never_closed<'a>(leaf: &'a AstNode<'a>) -> Option<String> {
    let (what, fix) = match &leaf.data().value {
        NodeValue::CodeBlock(code) if code.fenced && !code.closed => (
            "code block".to_string(),
            "add the closing fence".to_string(),
        ),
        NodeValue::HtmlBlock(html) if !is_closed_raw_html(html.block_type, &html.literal) => {
            let (what, end) = raw_html_kind(html.block_type, &html.literal)?;
            (what, format!("add `{end}`"))
        }
        _ => return None,
    };
    let opened = lines_of(leaf).0;
    Some(format!(
        "is inside the {what} opened on line {opened}, which is never closed, so no slide \
         starts here; {fix}"
    ))
}

/// The reason for a dash line inside the top-level container `value` (a list, quote,
/// directive, …) that opens on line `opened`. A line without indentation in a list or
/// quote is a lazy continuation of the paragraph above it.
fn nested(value: &NodeValue, opened: usize, dash: &DashLine<'_>) -> String {
    let unindented = !dash.raw.starts_with([' ', '\t']);
    let (container, lazy) = match value {
        NodeValue::List(_) | NodeValue::Item(_) => ("a list".to_string(), unindented),
        NodeValue::BlockQuote => ("a block quote".to_string(), true),
        NodeValue::Alert(_) => ("an alert".to_string(), true),
        NodeValue::FootnoteDefinition(_) => ("a footnote".to_string(), unindented),
        NodeValue::DescriptionList => ("a description list".to_string(), unindented),
        NodeValue::MultilineBlockQuote(_) => ("a `>>>` block quote".to_string(), false),
        NodeValue::BlockDirective(directive) => {
            let name = directive.info.split_whitespace().next().unwrap_or_default();
            (format!("the `:::{name}` directive"), false)
        }
        _ => return "did not start a slide".to_string(),
    };
    if lazy {
        return format!(
            "continues the paragraph above it, in {container} (from line {opened}): a line \
             right after a paragraph belongs to it, so no slide starts here; {}",
            dash.advice()
        );
    }
    format!(
        "is inside {container} (from line {opened}), so no slide starts here; only top-level \
         `---`/`--` lines are separators"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(code, line)` of every diagnostic `run` reports for `text` written to `name` in a
    /// fresh directory (no `presenter.toml` above it is expected).
    fn check_text(name: &str, text: &str) -> Vec<(String, Option<usize>)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let diagnostics = run(&BuildOptions::new(&path)).unwrap();
        for diagnostic in &diagnostics {
            assert_eq!(
                diagnostic.path.as_deref(),
                Some(path.as_path()),
                "{diagnostic}"
            );
        }
        diagnostics
            .iter()
            .map(|d| (d.code.to_string(), d.span.as_ref().map(|s| s.start_line)))
            .collect()
    }

    /// The `(line, message)` of each `separator-not-recognized` for `text`.
    fn separator_warnings(text: &str) -> Vec<(usize, String)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.md");
        std::fs::write(&path, text).unwrap();
        run(&BuildOptions::new(&path))
            .unwrap()
            .iter()
            .filter(|d| d.code == codes::SEPARATOR_NOT_RECOGNIZED)
            .map(|d| (d.span.as_ref().unwrap().start_line, d.message.clone()))
            .collect()
    }

    fn lines(warnings: &[(usize, String)]) -> Vec<usize> {
        warnings.iter().map(|(line, _)| *line).collect()
    }

    /// Asserts that the warnings are on `expected` lines and that each message contains
    /// its fragment.
    fn assert_warnings(warnings: &[(usize, String)], expected: &[(usize, &str)]) {
        let want: Vec<usize> = expected.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines(warnings), want, "{warnings:#?}");
        for ((_, message), (line, fragment)) in warnings.iter().zip(expected) {
            assert!(message.contains(fragment), "line {line}: {message}");
        }
    }

    #[test]
    fn real_separators_and_front_matter_are_not_reported() {
        let text = "---\ntitle: T\n---\n\n# One\n\n---\n\n## Two\n\n--\n\n## Three\n\n\
                    --- {.a}\n\nfour\n\n  ---  \n\nfive\n";
        assert_eq!(check_text("deck.md", text), vec![]);
    }

    #[test]
    fn setext_pitfall_is_reported_on_the_dash_line() {
        let text = "# One\n\nSome text\n--\n\nMore\n\nA title\n---\n\nend\n";
        let warnings = separator_warnings(text);
        assert_eq!(lines(&warnings), [4, 9]);
        assert_eq!(
            warnings[0].1,
            "`--` under a line of text turns that text into a heading (setext), so no slide \
             starts here; put a blank line before it to start a vertical slide"
        );
        assert!(warnings[1].1.starts_with("`---` under"), "{warnings:?}");
        assert!(warnings[1].1.ends_with("horizontal slide"), "{warnings:?}");
        // `--` with text right after it also needs a blank line after it.
        let warnings = separator_warnings("# One\n\nSome text\n--\nmore\n");
        assert_warnings(&warnings, &[(4, "put blank lines around it")]);
    }

    #[test]
    fn a_dash_line_in_the_text_of_a_setext_heading_needs_a_blank_line_after_it() {
        let text = "# One\n\ntext\n\n--\nTitle\n=====\n\nbody\n";
        let warnings = separator_warnings(text);
        assert_eq!(
            warnings,
            [(
                5,
                "`--` is part of the text of the heading underlined on line 7 (setext), so no \
                 slide starts here; put a blank line after it to start a vertical slide"
                    .to_string()
            )]
        );
        let warnings = separator_warnings("--\n--\n");
        assert_warnings(
            &warnings,
            &[
                (
                    1,
                    "heading underlined on line 2 (setext), so no slide starts here; put a \
                     blank line after it",
                ),
                (
                    2,
                    "under a line of text turns that text into a heading (setext), so no \
                     slide starts here; put a blank line before it",
                ),
            ],
        );
    }

    #[test]
    fn nested_indented_and_paragraph_lines_are_reported_with_the_reason() {
        let text = "# One\n\n- item\n\n  --\n\n> quote\n>\n> ---\n\n:::note\n---\n:::\n\n\
                    \x20   ---\n\n--\nmore text\n";
        let warnings = separator_warnings(text);
        // `> ---` is not a bare `---` line; the rest are.
        assert_warnings(
            &warnings,
            &[
                (
                    5,
                    "is inside a list (from line 3), so no slide starts here; only top-level",
                ),
                (12, "is inside the `:::note` directive (from line 11)"),
                (
                    15,
                    "is indented by 4 or more columns (a tab counts as 4), so it is code",
                ),
                (
                    17,
                    "is part of a paragraph, so no slide starts here; put a blank line after it",
                ),
            ],
        );
    }

    #[test]
    fn lazy_continuations_table_rows_and_html_blocks_ask_for_a_blank_line() {
        let text = "# One\n\n- item\n--\n\n> quote\n--\n\n| a |\n|---|\n| b |\n--\n\n\
                    <div>\n---\n</div>\n\n\t---\n";
        let warnings = separator_warnings(text);
        assert_warnings(
            &warnings,
            &[
                (
                    4,
                    "`--` continues the paragraph above it, in a list (from line 3): a line \
                     right after a paragraph belongs to it, so no slide starts here; put a \
                     blank line before it to start a vertical slide",
                ),
                (
                    7,
                    "continues the paragraph above it, in a block quote (from line 6)",
                ),
                (
                    12,
                    "is read as a row of the table that starts on line 9, so no slide starts \
                      here; put a blank line before it",
                ),
                (
                    15,
                    "is part of the HTML block that starts on line 14 (it ends at the first \
                      blank line), so no slide starts here; put a blank line before it",
                ),
                (18, "indented by 4 or more columns (a tab counts as 4)"),
            ],
        );
    }

    #[test]
    fn a_leading_paragraph_that_starts_with_dashes_is_reported() {
        // Slide 1 starts on line 1 (its first content), which is not a separator.
        let warnings = separator_warnings("--\ntext\n\n---\n\nend\n");
        assert_warnings(&warnings, &[(1, "part of a paragraph")]);
    }

    #[test]
    fn code_comments_and_raw_html_are_skipped_but_unclosed_ones_are_not() {
        let text = "# One\n\n```text\n---\n--\n```\n\n~~~\n---\n~~~\n\n<!--\n---\n-->\n\n\
                    <pre>\n---\n</pre>\n\n<?php\n---\n?>\n\n---\n\n## Two\n";
        assert_eq!(separator_warnings(text), vec![]);
        let warnings = separator_warnings("# One\n\n```\ncode\n\n---\n\n## Lost\n");
        assert_warnings(
            &warnings,
            &[(
                6,
                "is inside the code block opened on line 3, which is never closed, so no \
                   slide starts here; add the closing fence",
            )],
        );
        let warnings = separator_warnings("# One\n\n<!-- todo\n\n---\n\n## Lost\n");
        assert_warnings(
            &warnings,
            &[(
                5,
                "is inside the HTML comment opened on line 3, which is never closed, so \
                   no slide starts here; add `-->`",
            )],
        );
        let warnings = separator_warnings("# One\n\n<pre>\n---\n\n## Lost\n");
        assert_warnings(
            &warnings,
            &[(
                4,
                "is inside the `<pre>` block opened on line 3, which is never closed, so \
                   no slide starts here; add `</pre>`",
            )],
        );
        // An unclosed fence inside a list item: the fence is the problem, not the list.
        let warnings = separator_warnings("# One\n\n- item\n\n  ```\n  code\n\n  ---\n");
        assert_warnings(
            &warnings,
            &[(
                8,
                "is inside the code block opened on line 5, which is never closed",
            )],
        );
    }

    #[test]
    fn indented_code_listings_are_not_reported() {
        let text = "# Front matter example\n\n    ---\n    title: My talk\n    ---\n\n    \
                    # First slide\n\n---\n\n## Two\n\n    ---\n\n    --\n\nend\n";
        // The listing (lines 3–7) is code; the lone indented `---` and `--` are pitfalls.
        let warnings = separator_warnings(text);
        assert_warnings(
            &warnings,
            &[(13, "is indented by 4"), (15, "is indented by 4")],
        );
    }

    #[test]
    fn indented_dash_lines_after_text_are_reported() {
        // 4+ columns (or a tab) after text: a paragraph continuation line, never a setext
        // underline or a separator, so all three slides are lost.
        let text = "# One\n\nSome text\n    --\n\nmore\n\nOther text\n    ---\n\nend\n\n\
                    tabbed\n\t--\n\nlast\n  \t--\nafter\n";
        let warnings = separator_warnings(text);
        let unindent = "is part of a paragraph, so no slide starts here; remove its \
                        indentation and put a blank line before it to start a";
        assert_warnings(
            &warnings,
            &[
                (4, unindent),
                (9, unindent),
                (14, unindent),
                (
                    17,
                    "remove its indentation and put blank lines around it to start a vertical",
                ),
            ],
        );
        assert!(warnings[1].1.ends_with("horizontal slide"), "{warnings:?}");
        // Up to 3 columns it is a setext underline.
        let warnings = separator_warnings("# One\n\nSome text\n   --\n\nmore\n");
        assert_warnings(&warnings, &[(4, "under a line of text turns that text")]);
        assert!(!warnings[0].1.contains("indentation"), "{warnings:?}");
    }

    #[test]
    fn a_rule_right_after_a_link_definition_is_reported() {
        let text = "# One\n\ntext\n\n[x]: /u\n---\n\nAfter [x]\n\n[y]: /v\n[z]: /w\n   ---\n";
        let warnings = separator_warnings(text);
        assert_eq!(
            warnings,
            [
                (
                    6,
                    "`---` follows link reference definitions (from line 5) without a blank \
                     line, so it is read as text and no slide starts here; put a blank line \
                     before it to start a horizontal slide"
                        .to_string()
                ),
                (
                    12,
                    "`---` follows link reference definitions (from line 10) without a blank \
                     line, so it is read as text and no slide starts here; put a blank line \
                     before it to start a horizontal slide"
                        .to_string()
                ),
            ]
        );
        // `--` (also indented) after definitions is a separator, as is `---` after a blank
        // line.
        let text = "# One\n\n[x]: /u\n--\n\ntwo\n\n[y]: /v\n    --\n\nthree\n\n[z]: /w\n\n\
                    ---\n\nfour\n";
        assert_eq!(separator_warnings(text), vec![]);
    }

    #[test]
    fn separators_with_attributes_on_the_next_line_are_not_reported() {
        // `\s*` in `^(---|--)\s*\{(.*)\}$` spans the line break: both open slides.
        let text = "# One\n\n--\n{.a}\n\ntwo\n\n[x]: /u\n---\n{.b}\n\nthree\n";
        assert_eq!(separator_warnings(text), vec![]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.md");
        std::fs::write(&path, text).unwrap();
        let deck = pipeline::parse_only(&BuildOptions::new(&path))
            .unwrap()
            .0
            .deck;
        assert_eq!(deck.slides.len(), 2);
        assert_eq!(deck.slides[0].children.len(), 1);
        assert_eq!(deck.slides[0].children[0].classes, ["a"]);
        assert_eq!(deck.slides[1].classes, ["b"]);
    }

    /// The lines on which the splitter opened a slide with a separator, read from its slides:
    /// the start line of every slide that starts inside a node it took as a separator.
    fn splitter_separator_lines(text: &str) -> BTreeSet<usize> {
        let arena = Arena::new();
        let root = parse_document(&arena, text, &crate::markdown::split::dialect_options());
        let deck = split_with_source(root, text, SplitMode::Rules).unwrap();
        let held: Vec<_> = deck
            .slides
            .iter()
            .flat_map(|(slide, children)| std::iter::once(slide).chain(children))
            .flat_map(|slide| slide.nodes.clone())
            .collect();
        let taken: Vec<(usize, usize)> = root
            .children()
            .filter(|node| !held.iter().any(|held| std::ptr::eq(*held, *node)))
            .filter(|node| {
                matches!(
                    node.data().value,
                    NodeValue::Paragraph | NodeValue::ThematicBreak
                )
            })
            .map(lines_of)
            .collect();
        deck.slides
            .iter()
            .flat_map(|(slide, children)| std::iter::once(slide).chain(children))
            .map(|slide| slide.start_line)
            .filter(|line| taken.iter().any(|(s, e)| s <= line && line <= e))
            .collect()
    }

    #[test]
    fn opened_lines_agree_with_the_splitters_slides() {
        // Random documents from lines that make separators, pitfalls and link definitions
        // (a fixed generator, so the test is deterministic).
        const PIECES: &[&str] = &[
            "--",
            "---",
            " --",
            "   ---",
            "    --",
            "\t---",
            "--  ",
            "-- {.c}",
            "--- {#i}",
            "{.a}",
            "{.b x=1}",
            "text",
            "Title",
            "[x]: /u",
            "[y]:",
            "/v",
            "\"title\"",
            "- item",
            "> quote",
            "| a |",
            "|---|",
            "# H",
            "```",
            ":::note",
            ":::",
            "",
            "",
            "",
            "[^1]: note",
            "<!-- c -->",
            "\\--",
            "`--`",
        ];
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |n: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state >> 33).unwrap() % n
        };
        for _ in 0..20_000 {
            let count = 2 + next(10);
            let text: String = (0..count)
                .map(|_| format!("{}\n", PIECES[next(PIECES.len())]))
                .collect();
            let arena = Arena::new();
            let root = parse_document(&arena, &text, &crate::markdown::split::dialect_options());
            let lines = source_lines(&text);
            let opened = opened_lines(root, &text, &lines, SplitMode::Rules).unwrap();
            let expected = splitter_separator_lines(&text);
            assert_eq!(opened, expected, "{text:?}");
            // Every other `---`/`--` line outside code, raw HTML and the front matter is
            // reported, and no line that opened a slide is.
            let reported: BTreeSet<usize> =
                unrecognized_separators(root, &lines, SplitMode::Rules, &opened, &BTreeSet::new())
                    .into_iter()
                    .map(|(line, _)| line)
                    .collect();
            let skipped = Ranges::new(skipped_ranges(root));
            let lost: BTreeSet<usize> = (1..=lines.len())
                .filter(|&line| matches!(lines[line - 1].trim_matches([' ', '\t']), "---" | "--"))
                .filter(|line| !expected.contains(line) && skipped.find(*line).is_none())
                .collect();
            assert_eq!(reported, lost, "{text:?}");
        }
    }

    #[test]
    fn crlf_sources_and_link_definitions_keep_line_numbers() {
        let text = "# One\r\n\r\nSome text\r\n--\r\n\r\n[x]: http://a\r\n--\r\n\r\nend\r\n";
        let warnings = separator_warnings(text);
        assert_eq!(lines(&warnings), [4], "{warnings:?}");
    }

    #[test]
    fn headings_mode_reports_every_pitfall_but_setext_underlines() {
        let text = "---\nslides:\n  split: headings\n---\n\n<!-- licence -->\n\n---\n\n\
                    # Section\n\nIntro text.\n\n---\n\nMore intro.\n\n    ---\n\n\
                    Third part\n\n--\nfourth\n\nA title\n---\n\n--\nTitle\n=====\n";
        let warnings = separator_warnings(text);
        // Line 8 opens slide 1, which starts at the comment on line 6 (dialect §2.3); line
        // 14 opens a slide; line 26 underlines a slide heading.
        assert_warnings(
            &warnings,
            &[
                (18, "is indented by 4"),
                (
                    22,
                    "is part of a paragraph, so no slide starts here; put a blank line after \
                      it",
                ),
                (28, "heading underlined on line 30"),
            ],
        );
        // The underline is still a pitfall in rules mode.
        let rules = text.replace("split: headings", "split: rules");
        assert_eq!(lines(&separator_warnings(&rules)), [18, 22, 26, 28]);
    }

    #[test]
    fn headings_mode_exempts_only_underlines_of_slide_headings() {
        // A `---` underline makes a level-2 heading: a slide boundary only from
        // `slide_level: 2` on; below that it is content and the slide is lost.
        let deck = |level: &str| {
            format!(
                "---\nslides: {{split: headings{level}}}\n---\n\n# A\n\ntext\n\nTitle\n---\n\n\
                 more\n"
            )
        };
        let underline = "`---` under a line of text turns that text into a heading (setext), so \
                         no slide starts here; put a blank line before it to start a horizontal \
                         slide";
        for level in [", slide_level: 1", ", slide_level: 0"] {
            assert_eq!(
                separator_warnings(&deck(level)),
                [(10, underline.to_string())],
                "{level}"
            );
        }
        for level in ["", ", slide_level: 2", ", slide_level: 3"] {
            assert_eq!(separator_warnings(&deck(level)), vec![], "{level}");
        }
    }

    #[test]
    fn duplicate_ids_unknown_layouts_and_include_are_reported() {
        let text = "# One {#same}\n\n---\n\n# Two {#same layout=nope}\n\n--\n\n\
                    ## Three {#same layout=title}\n\n:::include other.md\n:::\n";
        let found = check_text("deck.md", text);
        let some = |code: &str, line: usize| (code.to_string(), Some(line));
        assert_eq!(
            found,
            [
                some("duplicate-slide-id", 3),
                some("unknown-layout", 3),
                some("duplicate-slide-id", 7),
                some("unknown-directive", 11),
            ]
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.md");
        std::fs::write(&path, text).unwrap();
        let diagnostics = run(&BuildOptions::new(&path)).unwrap();
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages[0],
            "slide 2: id `same` is already the id of slide 1 (line 1); links to `#/same` go \
             to the first one"
        );
        assert!(
            messages[1].starts_with("slide 2: unknown layout 'nope' (theme 'default' has: "),
            "{messages:?}"
        );
        assert!(
            messages[2].starts_with("slide 2.1: id `same`"),
            "{messages:?}"
        );
        assert_eq!(messages[3], INCLUDE_MESSAGE);
    }

    #[test]
    fn unknown_layout_warnings_are_the_renderers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.md");
        let text = "# One {layout=nope}\n\n---\n\n# Two {layout=title}\n\n--\n\n\
                    ## Three {layout=nope}\n\n---\n\n# Four {layout=other}\n";
        std::fs::write(&path, text).unwrap();
        let opts = BuildOptions::new(&path);
        let unknown = |diagnostics: &Diagnostics| -> Vec<Diagnostic> {
            diagnostics
                .iter()
                .filter(|d| d.code == codes::UNKNOWN_LAYOUT)
                .cloned()
                .collect()
        };
        let checked = unknown(&run(&opts).unwrap());
        let built = unknown(&pipeline::build_deck(&opts).unwrap().diagnostics);
        assert_eq!(checked.len(), 3, "{checked:#?}");
        assert_eq!(checked, built);
    }

    #[test]
    fn a_missing_theme_is_an_error_diagnostic() {
        let found = check_text("deck.md", "---\ntheme: no-such-theme\n---\n\n# One\n");
        assert_eq!(found, [("theme-error".to_string(), None)]);
    }

    /// A notebook with `cells` (JSON objects).
    fn notebook(cells: &[String]) -> String {
        format!(
            r#"{{"nbformat": 4, "nbformat_minor": 5, "metadata": {{}}, "cells": [{}]}}"#,
            cells.join(",")
        )
    }

    /// A cell of `kind` with `source` and the JSON `metadata`; code cells get `outputs`
    /// and `count` as their execution count.
    fn cell(kind: &str, source: &str, metadata: &str, outputs: &str, count: &str) -> String {
        let extra = if kind == "code" {
            format!(r#", "outputs": {outputs}, "execution_count": {count}"#)
        } else {
            String::new()
        };
        let source = serde_json::to_string(source).unwrap();
        format!(r#"{{"cell_type": "{kind}", "metadata": {metadata}, "source": {source}{extra}}}"#)
    }

    #[test]
    fn notebooks_whose_code_never_ran_get_a_hint() {
        let code = |outputs: &str, count: &str| cell("code", "x", "{}", outputs, count);
        let output = r#"[{"output_type": "stream", "name": "stdout", "text": "1"}]"#;
        // A slide type, so that the three cells are not one slide without one (F28).
        let slide = r#"{"slideshow": {"slide_type": "slide"}}"#;
        let none = notebook(&[
            cell("markdown", "x", slide, "", ""),
            code("[]", "null"),
            code("[]", "null"),
        ]);
        let found = check_text("talk.ipynb", &none);
        assert_eq!(found, [("notebook-no-outputs".to_string(), None)]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my talk.ipynb");
        std::fs::write(&path, &none).unwrap();
        let message = run(&BuildOptions::new(&path)).unwrap().into_vec()[0]
            .message
            .clone();
        assert_eq!(
            message,
            "none of the notebook's 2 code cells has outputs; execute the notebook and save it \
             before presenting (e.g. `jupyter nbconvert --to notebook --execute --inplace \
             'my talk.ipynb'`)"
        );
        let one = notebook(&[code("[]", "null")]);
        std::fs::write(&path, &one).unwrap();
        let message = run(&BuildOptions::new(&path)).unwrap().into_vec()[0]
            .message
            .clone();
        assert!(
            message.starts_with("the notebook's code cell has no outputs;"),
            "{message}"
        );
        let some = notebook(&[code("[]", "null"), code(output, "2")]);
        assert_eq!(check_text("talk.ipynb", &some), vec![]);
        // Executed cells that print nothing are fine.
        let silent = notebook(&[code("[]", "1"), code("[]", "null")]);
        assert_eq!(check_text("talk.ipynb", &silent), vec![]);
        let markdown_only = notebook(&[cell("markdown", "x", "{}", "", "")]);
        assert_eq!(check_text("talk.ipynb", &markdown_only), vec![]);
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quoted("talk.ipynb"), "talk.ipynb");
        assert_eq!(shell_quoted("my talk.ipynb"), "'my talk.ipynb'");
        assert_eq!(shell_quoted("it's.ipynb"), r"'it'\''s.ipynb'");
        assert_eq!(shell_quoted(""), "''");
    }

    /// `(code, cell, line)` of this module's source lints for a notebook with `cells`,
    /// with `presenter_toml` next to it.
    fn notebook_source_lints(
        cells: &[String],
        presenter_toml: &str,
    ) -> Vec<(&'static str, Option<usize>, usize)> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("presenter.toml"), presenter_toml).unwrap();
        let path = dir.path().join("nb.ipynb");
        std::fs::write(&path, notebook(cells)).unwrap();
        run(&BuildOptions::new(&path))
            .unwrap()
            .iter()
            .filter(|d| {
                [codes::SEPARATOR_NOT_RECOGNIZED, codes::UNKNOWN_DIRECTIVE].contains(&d.code)
            })
            .map(|d| {
                let span = d.span.as_ref().unwrap();
                (d.code, span.cell, span.start_line)
            })
            .collect()
    }

    #[test]
    fn markdown_cells_are_linted_and_split_ones_for_separators() {
        let notes = r#"{"slideshow": {"slide_type": "notes"}}"#;
        let skip = r#"{"slideshow": {"slide_type": "skip"}}"#;
        let cells = [
            // Front matter (3 lines) is removed before the cell is rendered.
            cell(
                "markdown",
                "---\ntitle: T\n---\n# One\n\n---\n\n## Two\n\nSome text\n--\n\nmore\n\n\
                 :::include x.md\n:::\n",
                "{}",
                "",
                "",
            ),
            cell(
                "markdown",
                "Notes\n--\n\n:::include y.md\n:::\n",
                notes,
                "",
                "",
            ),
            cell("markdown", "x\n--\n\n:::include z.md\n:::\n", skip, "", ""),
            cell("markdown", "text\n---\n", "{}", "", ""),
            cell("code", "1", "{}", "[]", "1"),
            // A later front matter block that sets metadata is ignored and removed too.
            cell(
                "markdown",
                "---\ntitle: Other\n---\nafter\n--\n",
                "{}",
                "",
                "",
            ),
        ];
        let include = codes::UNKNOWN_DIRECTIVE;
        let separator = codes::SEPARATOR_NOT_RECOGNIZED;
        assert_eq!(
            notebook_source_lints(&cells, ""),
            [(include, Some(0), 15), (include, Some(1), 4)]
        );
        assert_eq!(
            notebook_source_lints(&cells, "[notebook]\nsplit_markdown_cells = true\n"),
            [
                (separator, Some(0), 11),
                (include, Some(0), 15),
                (include, Some(1), 4),
                (separator, Some(3), 2),
                (separator, Some(5), 5),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_source_lints_read_the_file_the_frontend_read() {
        // `link/../deck.md` is `real/deck.md` for the OS but `deck.md` lexically.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("real/sub")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real/sub"), dir.path().join("link")).unwrap();
        std::fs::write(dir.path().join("real/deck.md"), "# One\n\nSome text\n--\n").unwrap();
        std::fs::write(dir.path().join("deck.md"), "# Other\n\n---\n\n## two\n").unwrap();
        let opts = BuildOptions::new(dir.path().join("link/../deck.md"));
        let found = |opts: &BuildOptions| -> Vec<(&'static str, Option<usize>)> {
            run(opts)
                .unwrap()
                .iter()
                .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
                .collect()
        };
        let expected = [(codes::SEPARATOR_NOT_RECOGNIZED, Some(4))];
        assert_eq!(found(&opts), expected);
        std::fs::remove_file(dir.path().join("deck.md")).unwrap();
        assert_eq!(found(&opts), expected);
    }

    #[test]
    fn failures_become_error_diagnostics() {
        let missing = Path::new("no/such/deck.md");
        let err = run(&BuildOptions::new(missing)).unwrap_err();
        let diagnostic = error_diagnostic(&err, missing);
        assert_eq!(diagnostic.code, codes::IO_ERROR);
        assert_eq!(diagnostic.level, crate::error::Level::Error);
        assert!(diagnostic.path.as_ref().unwrap().is_absolute());
        assert!(
            diagnostic
                .path
                .as_ref()
                .unwrap()
                .ends_with("no/such/deck.md")
        );
        assert!(
            diagnostic.message.contains("no/such/deck.md"),
            "{diagnostic}"
        );

        let parse = Error::Parse {
            path: PathBuf::from("/x/deck.md"),
            line: Some(7),
            message: "unbalanced quote".into(),
        };
        let diagnostic = error_diagnostic(&parse, Path::new("/x/deck.md"));
        assert_eq!(
            diagnostic.to_string(),
            "/x/deck.md:7: error: unbalanced quote"
        );
        assert_eq!(diagnostic.code, codes::PARSE_ERROR);
        let front = error_diagnostic(&Error::FrontMatter("bad".into()), Path::new("/x/a.md"));
        assert_eq!(front.to_string(), "/x/a.md: error: front matter: bad");
        assert_eq!(front.code, codes::FRONT_MATTER_ERROR);
        let config = error_diagnostic(&Error::Config("bad".into()), Path::new("/x/a.md"));
        assert_eq!(config.code, codes::CONFIG_ERROR);
        let other = error_diagnostic(&Error::Other("x".into()), Path::new("/x/a.txt"));
        assert_eq!(other.code, codes::ERROR);
    }

    #[test]
    fn clean_fixtures_and_examples_report_nothing() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for deck in [
            "examples/basic/slides.md",
            "examples/headings-mode/slides.md",
            "tests/fixtures/md/code.md",
            "tests/fixtures/md/headings-mode.md",
            "tests/fixtures/md/math.md",
            "tests/fixtures/md/minimal.md",
            "tests/fixtures/md/notes.md",
        ] {
            let diagnostics = run(&BuildOptions::new(root.join(deck))).unwrap();
            assert!(diagnostics.is_empty(), "{deck}: {diagnostics:#?}");
        }
        // directives.md documents `:::include` ("`check` warns") and is otherwise clean.
        let path = root.join("tests/fixtures/md/directives.md");
        let found: Vec<(&str, Option<usize>)> = run(&BuildOptions::new(path))
            .unwrap()
            .iter()
            .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
            .collect();
        assert_eq!(found, [("unknown-directive", Some(84))]);
    }

    #[test]
    fn separators_fixture_reports_every_non_separator_line() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md/separators.md");
        let found: Vec<(String, Option<usize>)> = run(&BuildOptions::new(path))
            .unwrap()
            .iter()
            .map(|d| (d.code.to_string(), d.span.as_ref().map(|s| s.start_line)))
            .collect();
        let at = |code: &str, line: usize| (code.to_string(), Some(line));
        assert_eq!(
            found,
            [
                at("separator-not-recognized", 57),
                at("separator-not-recognized", 70),
                at("separator-not-recognized", 76),
                at("empty-slide", 80),
                at("empty-slide", 93),
            ]
        );
    }
}
