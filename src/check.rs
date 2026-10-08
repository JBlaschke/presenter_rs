//! Lint rules for Markdown decks and notebooks (`presenter check`, `docs/spec/cli.md`).
//!
//! [`run`] parses the deck without building it ([`pipeline::parse_only`]), resolves its
//! theme ([`pipeline::resolve_theme`]) and returns the frontend's diagnostics together with
//! the deck-level lints below, ordered by position (deck-level findings first, then by cell
//! and line). Missing assets are never errors here.
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
//!   `output-script`, `output-unsupported`, `output-invalid` and `output-widget` (§4
//!   "interactive widget rendered as text").
//!
//! Added here ([`codes`]):
//!
//! - `separator-not-recognized` (Markdown, rules mode, dialect §2.2 and §10): a raw source
//!   line that is `---` or `--` (ignoring surrounding spaces and tabs) but did not open a
//!   slide, with the reason (setext heading, indented code, part of a paragraph, inside a list,
//!   block quote, directive, …). Lines in the front matter, in closed fenced code blocks and in
//!   closed HTML comments or `<pre>`/`<script>`/`<style>`/`<textarea>` blocks are skipped, and
//!   so are lines the splitter already reported. Headings mode (dialect §2.3) is not linted:
//!   there separators are optional and `Title`/`---` is a legitimate setext heading.
//! - `unknown-directive` (Markdown, dialect §4): `:::include`, which is reserved and not
//!   implemented.
//! - `duplicate-slide-id`: a slide `id` used by an earlier slide.
//! - `unknown-layout` (theme-format §2 `[layouts]`): a slide's `layout` is not a
//!   `templates/layouts/<name>.html` file of the theme chain ([`Theme::has_layout`]).
//! - `notebook-no-outputs` (notebook-mapping §4 notes): a notebook with code cells none of
//!   which has outputs, with a hint to execute it.
//! - `theme-error` (level error): the deck's theme cannot be resolved; `unknown-layout` is then
//!   skipped.
//!
//! Failures that leave no deck to lint (the input cannot be read or parsed, invalid front
//! matter or `presenter.toml`) are [`run`]'s `Err`; [`error_diagnostic`] turns them into an
//! error-level [`Diagnostic`] so that `check` can report them like any other finding.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, parse_document};

use crate::assets::registry::normalize_lexically;
use crate::config::{Config, SlideSplit};
use crate::error::{Diagnostic, Diagnostics, Error, Result};
use crate::ir::{Block, Deck, Slide, SourceKind, SourceSpan};
use crate::markdown::{comrak_options, split::source_lines};
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
    /// A notebook whose code cells have no outputs at all (warning).
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
    let mut found: Vec<Diagnostic> = Vec::new();
    if deck.source.kind == SourceKind::Markdown {
        let text = std::fs::read_to_string(&deck.source.path).map_err(|err| {
            Error::Io(std::io::Error::new(
                err.kind(),
                format!("cannot read {}: {err}", deck.source.path.display()),
            ))
        })?;
        found.extend(markdown_lints(&text, &config, &deck, &parsed.diagnostics));
    }
    found.extend(duplicate_slide_ids(&deck));
    found.extend(notebook_without_outputs(&deck));
    match pipeline::resolve_theme(opts, &deck, &config, config_path.as_deref()) {
        Ok(theme) => found.extend(unknown_layouts(&deck, &theme)),
        Err(err) => found.push(Diagnostic::error(codes::THEME_ERROR, err.to_string())),
    }
    let mut diagnostics = parsed.diagnostics.into_vec();
    diagnostics.extend(
        found
            .into_iter()
            .map(|diagnostic| diagnostic.with_path(&deck.source.path)),
    );
    // Stable: deck-level findings (no span) first, then by cell and line; ties keep the
    // frontend's order before this module's.
    diagnostics.sort_by_key(|d| d.span.as_ref().map(|span| (span.cell, span.start_line)));
    tracing::debug!(
        diagnostics = diagnostics.len(),
        "checked {}",
        deck.source.path.display()
    );
    Ok(diagnostics.into())
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
/// renderer's (`render::context`), so `check` and `build` agree.
fn unknown_layouts(deck: &Deck, theme: &Theme) -> Vec<Diagnostic> {
    labelled_slides(deck)
        .into_iter()
        .filter(|(_, slide)| !theme.has_layout(&slide.layout))
        .map(|(label, slide)| {
            let message = format!(
                "{label}: unknown layout '{}' (theme '{}' has: {}); using the default layout",
                slide.layout,
                theme.name(),
                theme.layouts().join(", ")
            );
            slide_warning(codes::UNKNOWN_LAYOUT, message, slide)
        })
        .collect()
}

/// `notebook-no-outputs`: a notebook deck with code cells, none of which has an output.
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
    if cells.is_empty() || cells.iter().any(|cell| !cell.outputs.is_empty()) {
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
             `jupyter nbconvert --to notebook --execute --inplace {file}`)"
        ),
    ))
}

/// The lints that need the Markdown source: `separator-not-recognized` (rules mode only)
/// and `unknown-directive`. `config` is the deck's (front matter merged), `frontend` the
/// frontend's diagnostics (lines it already reported are not reported again).
fn markdown_lints(
    text: &str,
    config: &Config,
    deck: &Deck,
    frontend: &Diagnostics,
) -> Vec<Diagnostic> {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &comrak_options(&config.markdown));
    let mut found = include_directives(root);
    if config.slides.split == SlideSplit::Rules {
        let starts: BTreeSet<usize> = labelled_slides(deck)
            .into_iter()
            .filter_map(|(_, slide)| slide.span.as_ref().map(|span| span.start_line))
            .collect();
        let reported: BTreeSet<usize> = frontend
            .iter()
            .filter(|d| d.code == codes::SEPARATOR_NOT_RECOGNIZED)
            .filter_map(|d| d.span.as_ref().map(|span| span.start_line))
            .collect();
        found.extend(unrecognized_separators(root, text, &starts, &reported));
    }
    found
}

/// A warning at `line` of a Markdown source (no path yet).
fn line_warning(code: &'static str, line: usize, message: String) -> Diagnostic {
    Diagnostic::warning(code, message).with_span(SourceSpan {
        start_line: line,
        end_line: line,
        cell: None,
    })
}

/// The first and last source line of `node`.
fn lines_of<'a>(node: &'a AstNode<'a>) -> (usize, usize) {
    let pos = node.data().sourcepos;
    (pos.start.line, pos.end.line)
}

/// `unknown-directive` for every `:::include` (dialect §4: reserved, not implemented).
fn include_directives<'a>(root: &'a AstNode<'a>) -> Vec<Diagnostic> {
    root.descendants()
        .filter(|node| match &node.data().value {
            NodeValue::BlockDirective(directive) => {
                directive.info.split_whitespace().next() == Some("include")
            }
            _ => false,
        })
        .map(|node| {
            line_warning(
                codes::UNKNOWN_DIRECTIVE,
                lines_of(node).0,
                "unknown directive 'include': including files is not implemented, so nothing \
                 is included (the directive renders as a plain <div>)"
                    .to_string(),
            )
        })
        .collect()
}

/// `separator-not-recognized` for every raw `---`/`--` line of `text` that did not open a
/// slide (module docs). `starts` are the slides' first lines (a separator's line is the first
/// line of the slide it opens), `reported` the lines the splitter already reported.
fn unrecognized_separators<'a>(
    root: &'a AstNode<'a>,
    text: &str,
    starts: &BTreeSet<usize>,
    reported: &BTreeSet<usize>,
) -> Vec<Diagnostic> {
    let skipped = skipped_ranges(root);
    let mut found = Vec::new();
    for (index, raw) in source_lines(text).into_iter().enumerate() {
        let line = index + 1;
        let mark = raw.trim_matches([' ', '\t']);
        if !matches!(mark, "---" | "--")
            || reported.contains(&line)
            || skipped
                .iter()
                .any(|&(start, end)| start <= line && line <= end)
        {
            continue;
        }
        let top = root.children().find(|node| {
            let (start, end) = lines_of(node);
            start <= line && line <= end
        });
        if starts.contains(&line) && top.is_some_and(|node| is_separator_at(node, line)) {
            continue;
        }
        let message = format!("`{mark}` {}", why_not_a_separator(top, line, mark));
        found.push(line_warning(codes::SEPARATOR_NOT_RECOGNIZED, line, message));
    }
    found
}

/// Line ranges whose `---`/`--` lines are never meant as separators: the front matter,
/// closed fenced code blocks, and closed raw HTML blocks (comments, `<pre>`, `<script>`,
/// `<style>`, `<textarea>`), at any depth.
fn skipped_ranges<'a>(root: &'a AstNode<'a>) -> Vec<(usize, usize)> {
    root.descendants()
        .filter(|node| match &node.data().value {
            NodeValue::FrontMatter(_) => true,
            NodeValue::CodeBlock(code) => code.fenced && code.closed,
            NodeValue::HtmlBlock(html) => is_closed_raw_html(html.block_type, &html.literal),
            _ => false,
        })
        .map(lines_of)
        .collect()
}

/// True for an HTML block of CommonMark type 1 (`<pre>`, `<script>`, `<style>`,
/// `<textarea>`) or 2 (a comment) whose end marker is present.
fn is_closed_raw_html(block_type: u8, literal: &str) -> bool {
    match block_type {
        1 => {
            let lower = literal.to_ascii_lowercase();
            ["</pre>", "</script>", "</style>", "</textarea>"]
                .iter()
                .any(|end| lower.contains(end))
        }
        2 => literal.contains("-->"),
        _ => false,
    }
}

/// True if the top-level `node` is a separator on `line`: a thematic break there, or a
/// paragraph that ends there (`--` alone, possibly after link reference definitions,
/// dialect §2.2).
fn is_separator_at<'a>(node: &'a AstNode<'a>, line: usize) -> bool {
    let (start, end) = lines_of(node);
    match node.data().value {
        NodeValue::ThematicBreak => start == line,
        NodeValue::Paragraph => end == line,
        _ => false,
    }
}

/// Why the `mark` line `line` inside the top-level block `top` opened no slide.
fn why_not_a_separator<'a>(top: Option<&'a AstNode<'a>>, line: usize, mark: &str) -> String {
    let direction = if mark == "---" {
        "horizontal"
    } else {
        "vertical"
    };
    let Some(node) = top else {
        return "did not start a slide".to_string();
    };
    let opened = lines_of(node).0;
    let container = match &node.data().value {
        NodeValue::Heading(heading) if heading.setext => {
            return format!(
                "under a line of text turns that text into a heading (setext), so no slide \
                 starts here; put a blank line before it to start a {direction} slide"
            );
        }
        NodeValue::CodeBlock(code) if !code.fenced => {
            return format!(
                "is indented by 4 or more spaces, so it is code and no slide starts here; \
                 remove the indentation to start a {direction} slide"
            );
        }
        NodeValue::CodeBlock(_) => {
            return format!(
                "is inside the code block opened on line {opened}, which is never closed, so \
                 no slide starts here; add the closing fence"
            );
        }
        NodeValue::HtmlBlock(html) if html.block_type == 2 => {
            return format!(
                "is inside the HTML comment opened on line {opened}, which is never closed \
                 (`-->`), so no slide starts here"
            );
        }
        NodeValue::Paragraph if lines_of(node) != (line, line) => {
            return format!(
                "is part of a paragraph, so no slide starts here; put blank lines around it \
                 to start a {direction} slide"
            );
        }
        NodeValue::List(_) | NodeValue::Item(_) => "a list".to_string(),
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => "a block quote".to_string(),
        NodeValue::Alert(_) => "an alert".to_string(),
        NodeValue::BlockDirective(directive) => {
            let name = directive.info.split_whitespace().next().unwrap_or_default();
            format!("the `:::{name}` directive")
        }
        NodeValue::FootnoteDefinition(_) => "a footnote".to_string(),
        NodeValue::Table(_) => "a table".to_string(),
        NodeValue::DescriptionList => "a description list".to_string(),
        NodeValue::HtmlBlock(_) => "an HTML block".to_string(),
        _ => return "did not start a slide".to_string(),
    };
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
        assert!(
            warnings[0].1.starts_with("`--` under a line of text"),
            "{warnings:?}"
        );
        assert!(warnings[0].1.contains("vertical slide"), "{warnings:?}");
        assert!(warnings[1].1.starts_with("`---` under"), "{warnings:?}");
        assert!(warnings[1].1.contains("horizontal slide"), "{warnings:?}");
    }

    #[test]
    fn nested_indented_and_paragraph_lines_are_reported_with_the_reason() {
        let text = "# One\n\n- item\n\n  --\n\n> quote\n>\n> ---\n\n:::note\n---\n:::\n\n\
                    \x20   ---\n\n--\nmore text\n";
        let warnings = separator_warnings(text);
        // `> ---` is not a bare `---` line; the rest are.
        assert_eq!(lines(&warnings), [5, 12, 15, 17], "{warnings:?}");
        assert!(
            warnings[0].1.contains("inside a list (from line 3)"),
            "{warnings:?}"
        );
        assert!(
            warnings[1]
                .1
                .contains("inside the `:::note` directive (from line 11)"),
            "{warnings:?}"
        );
        assert!(
            warnings[2].1.contains("indented by 4 or more spaces"),
            "{warnings:?}"
        );
        assert!(
            warnings[3].1.contains("part of a paragraph"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_leading_paragraph_that_starts_with_dashes_is_reported() {
        // Slide 1 starts on line 1 (its first content), which is not a separator.
        let warnings = separator_warnings("--\ntext\n\n---\n\nend\n");
        assert_eq!(lines(&warnings), [1], "{warnings:?}");
    }

    #[test]
    fn code_comments_and_raw_html_are_skipped_but_unclosed_ones_are_not() {
        let text = "# One\n\n```text\n---\n--\n```\n\n~~~\n---\n~~~\n\n<!--\n---\n-->\n\n\
                    <pre>\n---\n</pre>\n\n---\n\n## Two\n";
        assert_eq!(separator_warnings(text), vec![]);
        let warnings = separator_warnings("# One\n\n```\ncode\n\n---\n\n## Lost\n");
        assert_eq!(lines(&warnings), [6]);
        assert!(
            warnings[0].1.contains("code block opened on line 3"),
            "{warnings:?}"
        );
        let warnings = separator_warnings("# One\n\n<!-- todo\n\n---\n\n## Lost\n");
        assert_eq!(lines(&warnings), [5]);
        assert!(
            warnings[0].1.contains("HTML comment opened on line 3"),
            "{warnings:?}"
        );
    }

    #[test]
    fn crlf_sources_and_link_definitions_keep_line_numbers() {
        let text = "# One\r\n\r\nSome text\r\n--\r\n\r\n[x]: http://a\r\n--\r\n\r\nend\r\n";
        let warnings = separator_warnings(text);
        assert_eq!(lines(&warnings), [4], "{warnings:?}");
    }

    #[test]
    fn headings_mode_is_not_linted_for_separators() {
        let text = "---\nslides:\n  split: headings\n---\n\n# Section\n\nA title\n---\n\ntext\n";
        assert_eq!(separator_warnings(text), vec![]);
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
    }

    #[test]
    fn a_missing_theme_is_an_error_diagnostic() {
        let found = check_text("deck.md", "---\ntheme: no-such-theme\n---\n\n# One\n");
        assert_eq!(found, [("theme-error".to_string(), None)]);
    }

    #[test]
    fn notebooks_without_any_output_get_a_hint() {
        let cell = |kind: &str, outputs: &str| {
            let extra = if kind == "code" {
                format!(r#", "outputs": {outputs}, "execution_count": null"#)
            } else {
                String::new()
            };
            format!(r#"{{"cell_type": "{kind}", "metadata": {{}}, "source": "x"{extra}}}"#)
        };
        let notebook = |cells: &[String]| {
            format!(
                r#"{{"nbformat": 4, "nbformat_minor": 5, "metadata": {{}}, "cells": [{}]}}"#,
                cells.join(",")
            )
        };
        let output = r#"[{"output_type": "stream", "name": "stdout", "text": "1"}]"#;
        let none = notebook(&[cell("markdown", ""), cell("code", "[]"), cell("code", "[]")]);
        let found = check_text("talk.ipynb", &none);
        assert_eq!(found, [("notebook-no-outputs".to_string(), None)]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.ipynb");
        std::fs::write(&path, &none).unwrap();
        let message = run(&BuildOptions::new(&path)).unwrap().into_vec()[0]
            .message
            .clone();
        assert_eq!(
            message,
            "none of the notebook's 2 code cells has outputs; execute the notebook and save it \
             before presenting (e.g. `jupyter nbconvert --to notebook --execute --inplace \
             talk.ipynb`)"
        );
        let one = notebook(&[cell("code", "[]")]);
        std::fs::write(&path, &one).unwrap();
        let message = run(&BuildOptions::new(&path)).unwrap().into_vec()[0]
            .message
            .clone();
        assert!(
            message.starts_with("the notebook's code cell has no outputs;"),
            "{message}"
        );
        let some = notebook(&[cell("code", "[]"), cell("code", output)]);
        assert_eq!(check_text("talk.ipynb", &some), vec![]);
        let markdown_only = notebook(&[cell("markdown", "")]);
        assert_eq!(check_text("talk.ipynb", &markdown_only), vec![]);
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
