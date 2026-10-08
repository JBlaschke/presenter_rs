//! Intermediate representation: `Deck`, `Slide`, `Block`, `NotebookCell`, … and `Parsed`.
//!
//! Contract: `docs/spec/ir.md` §1 (types, transcribed verbatim) and §4 (`Parsed`). The IR is
//! the only thing frontends (Markdown, notebook) produce and the only thing the renderer
//! consumes. It is plain data with `serde` derives; its JSON form is what `presenter dump`
//! prints and what the template context embeds under `deck`.
//!
//! Invariants (ir.md §2) the producers must uphold: children never have children; `layout`
//! is non-empty; `attrs` never contains `id`, `class` or `layout` and its keys are lowercase
//! and normalized; local URLs inside HTML use the `asset:<id>` scheme; every `notes`/`html`
//! string is HTML (never Markdown); `Features` is derived from content only; only
//! `BTreeMap`/`serde_json::Map` (never `HashMap`) so JSON is stable; spans are 1-based.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::assets::registry::AssetRegistry;
use crate::error::Diagnostics;

/// A whole presentation. One source file produces one `Deck`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deck {
    /// Deck metadata (front matter / notebook metadata).
    pub meta: DeckMeta,
    /// Top-level (horizontal) slides, in order.
    pub slides: Vec<Slide>,
    /// Which optional front-end assets the output needs (drives KaTeX inclusion etc.).
    pub features: Features,
    /// Where the deck came from.
    pub source: SourceInfo,
    /// Summary of registered assets without their bytes (for `dump` and `check`).
    pub assets: Vec<AssetInfo>,
}

impl Deck {
    /// Checks the mechanically checkable invariants of ir.md §2 and returns one message per
    /// violation (empty when the deck is well-formed). Frontend tests assert this is empty.
    ///
    /// Checked: children have no children (1); `attrs` keys are lowercase and never `id`,
    /// `class` or `layout` (2); `layout` is non-empty (3); every `asset:<12 hex>` placeholder
    /// in `Block::Html` / `CellOutput::Display` HTML is listed in `assets` (4); features
    /// implied by content are set — `notes` for notes, `notebook` and `code` for cells,
    /// `math` for `data-math-style` or `text/latex` outputs (6); spans are 1-based and
    /// ordered (8).
    pub fn invariant_violations(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (i, slide) in self.slides.iter().enumerate() {
            self.check_slide(slide, &format!("slides[{i}]"), &mut problems);
            for (j, child) in slide.children.iter().enumerate() {
                let path = format!("slides[{i}].children[{j}]");
                if !child.children.is_empty() {
                    problems.push(format!("{path}: a vertical child has children"));
                }
                self.check_slide(child, &path, &mut problems);
            }
        }
        problems
    }

    fn check_slide(&self, slide: &Slide, path: &str, problems: &mut Vec<String>) {
        if slide.layout.is_empty() {
            problems.push(format!("{path}: empty layout"));
        }
        for key in slide.attrs.keys() {
            if matches!(key.as_str(), "id" | "class" | "layout") || key.to_lowercase() != *key {
                problems.push(format!("{path}: attrs key {key:?} not allowed"));
            }
        }
        if let Some(span) = &slide.span
            && (span.start_line == 0 || span.end_line < span.start_line)
        {
            problems.push(format!("{path}: bad span {span:?}"));
        }
        if slide.notes.is_some() && !self.features.notes {
            problems.push(format!("{path}: has notes but features.notes is false"));
        }
        for block in &slide.blocks {
            match block {
                Block::Html { html } => self.check_html(html, path, problems),
                Block::Cell(cell) => {
                    if !(self.features.notebook && self.features.code) {
                        problems.push(format!(
                            "{path}: has a cell but features.notebook/code is not set"
                        ));
                    }
                    for output in &cell.outputs {
                        if let CellOutput::Display { mime, html, .. } = output {
                            if mime == "text/latex" && !self.features.math {
                                problems.push(format!("{path}: text/latex without features.math"));
                            }
                            self.check_html(html, path, problems);
                        }
                    }
                }
            }
        }
    }

    fn check_html(&self, html: &str, path: &str, problems: &mut Vec<String>) {
        if html.contains("data-math-style") && !self.features.math {
            problems.push(format!("{path}: math in HTML but features.math is false"));
        }
        for (at, _) in html.match_indices("asset:") {
            let id = &html[at + "asset:".len()..];
            let id = id.get(..12).unwrap_or(id);
            let is_id =
                id.len() == 12 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            if is_id && !self.assets.iter().any(|asset| asset.id == id) {
                problems.push(format!("{path}: placeholder asset:{id} not in deck.assets"));
            }
        }
    }
}

/// Deck-level metadata from front matter (Markdown) or notebook metadata.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeckMeta {
    /// Deck title (`<title>`, title layout).
    pub title: Option<String>,
    /// Subtitle shown by the title layout.
    pub subtitle: Option<String>,
    /// Author line.
    pub author: Option<String>,
    /// Date, as written (no parsing).
    pub date: Option<String>,
    /// Short description (`<meta name="description">`).
    pub description: Option<String>,
    /// BCP-47 tag for `<html lang>`; renderer falls back to config, then "en".
    pub lang: Option<String>,
    /// Theme name or path exactly as written in front matter; resolved by `theme::resolve`.
    pub theme: Option<String>,
    /// Raw `reveal:` table from front matter, merged into `Reveal.initialize` options.
    pub reveal: Map<String, Value>,
    /// Every other front-matter key, verbatim. Templates read `meta.vars.<key>`.
    pub vars: Map<String, Value>,
}

/// One slide: a top-level (horizontal) slide or a vertical child.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slide {
    /// `id` attribute for `<section>` (from `{#id}`), used for `#/id` links.
    pub id: Option<String>,
    /// Layout name; `"default"` unless set. The renderer maps it to `layouts/<name>.html`.
    pub layout: String,
    /// CSS classes for `<section>` (without the `layout-<name>` class the theme adds).
    pub classes: Vec<String>,
    /// Attributes emitted verbatim on `<section>`; keys already normalized
    /// (e.g. `data-background-color`). Never contains `id`, `class` or `layout`.
    pub attrs: BTreeMap<String, String>,
    /// Plain text of the first heading, if any (titles, TOC, aria labels).
    pub title: Option<String>,
    /// Ordered content of the slide.
    pub blocks: Vec<Block>,
    /// Speaker notes, already rendered to HTML. `None` when absent.
    pub notes: Option<String>,
    /// Vertical sub-slides. Only top-level slides may have children; children never do.
    pub children: Vec<Slide>,
    /// Source lines this slide came from.
    pub span: Option<SourceSpan>,
}

impl Default for Slide {
    fn default() -> Self {
        Slide {
            id: None,
            layout: "default".to_string(),
            classes: vec![],
            attrs: BTreeMap::new(),
            title: None,
            blocks: vec![],
            notes: None,
            children: vec![],
            span: None,
        }
    }
}

impl Slide {
    /// An empty slide with layout `"default"` (same as [`Slide::default`]).
    pub fn new() -> Self {
        Self::default()
    }
}

/// Ordered content of a slide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Rendered HTML (from Markdown). Image URLs inside use the `asset:<id>` scheme (§3).
    Html {
        /// The HTML fragment, inserted unescaped by the theme.
        html: String,
    },
    /// A notebook code cell with its outputs. Rendered by the theme's `cell.html`.
    Cell(NotebookCell),
}

impl Block {
    /// A [`Block::Html`] holding `html`.
    pub fn html(html: String) -> Self {
        Block::Html { html }
    }
}

/// A notebook code cell as the templates see it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotebookCell {
    /// 0-based index of the cell in the notebook (counting all cells, including skipped ones).
    pub index: usize,
    /// nbformat 4.5 cell id, if present.
    pub id: Option<String>,
    /// Language for highlight.js, e.g. `"python"`; from `metadata.language_info.name`,
    /// else `metadata.kernelspec.language`, else `"text"`.
    pub language: String,
    /// Raw source (not escaped; templates auto-escape).
    pub source: String,
    /// `In [n]` prompt number; `None` for never-executed cells.
    pub execution_count: Option<i64>,
    /// Rendered outputs, in notebook order.
    pub outputs: Vec<CellOutput>,
    /// From tags `hide-input`/`remove-input` or `metadata.jupyter.source_hidden`.
    pub hide_input: bool,
    /// From tags `hide-output`/`remove-output` or `metadata.jupyter.outputs_hidden`.
    pub hide_output: bool,
    /// `slideshow.slide_type == "fragment"`. The template wraps the cell in `.fragment`.
    pub fragment: bool,
    /// `metadata.tags`, verbatim.
    pub tags: Vec<String>,
    /// `cell.metadata` verbatim (a JSON object; `Value::Null` if absent).
    pub metadata: Value,
}

/// One rendered output of a notebook code cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellOutput {
    /// stdout/stderr. `html` is escaped text with ANSI colors converted to `<span>`s, no `<pre>`.
    Stream {
        /// Stream name: `"stdout"` or `"stderr"`.
        name: String,
        /// Escaped, ANSI-converted text.
        html: String,
    },
    /// An exception. `html` is the joined traceback, escaped, ANSI converted, no `<pre>`.
    Error {
        /// Exception class name, e.g. `"ZeroDivisionError"`.
        ename: String,
        /// Exception message.
        evalue: String,
        /// Escaped, ANSI-converted traceback.
        html: String,
    },
    /// `display_data` or `execute_result`, reduced to one MIME type (see notebook-mapping.md §4).
    /// `html` is a complete, insertable fragment (`<img>`, raw HTML, `<pre>`, math span, …).
    /// `execution_count` is `Some` only for `execute_result`.
    Display {
        /// The MIME type that was chosen from the bundle.
        mime: String,
        /// Insertable HTML fragment.
        html: String,
        /// `Out [n]` number for `execute_result`; `None` for `display_data`.
        execution_count: Option<i64>,
    },
    /// No renderable MIME type was present (e.g. only a widget view).
    Unsupported {
        /// The MIME types that were present.
        mimes: Vec<String>,
    },
}

/// Optional front-end assets the output needs. Derived from content, never from config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    /// Any `data-math-style` span or `text/latex` output → include KaTeX.
    pub math: bool,
    /// Any fenced code block or code cell → include highlight plugin.
    pub code: bool,
    /// Any slide has notes → include notes plugin.
    pub notes: bool,
    /// Source was a notebook, or any `Block::Cell` exists.
    pub notebook: bool,
}

impl Features {
    /// Field-wise OR: afterwards `self` needs everything either side needed.
    pub fn merge(&mut self, other: Features) {
        self.math |= other.math;
        self.code |= other.code;
        self.notes |= other.notes;
        self.notebook |= other.notebook;
    }
}

/// The source file a deck was parsed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Absolute path of the source file.
    pub path: PathBuf,
    /// Which frontend parsed it.
    pub kind: SourceKind,
    /// Directory relative paths (images, includes) resolve against. Normally `path.parent()`.
    pub base_dir: PathBuf,
}

/// Kind of source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A Markdown (`.md`) file.
    Markdown,
    /// A Jupyter notebook (`.ipynb`) file.
    Notebook,
}

/// 1-based inclusive line range. For notebooks, lines are within the cell's source and
/// `cell` is the cell index; for Markdown `cell` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    /// First line (1-based, inclusive).
    pub start_line: usize,
    /// Last line (1-based, inclusive).
    pub end_line: usize,
    /// Notebook cell index; `None` for Markdown.
    pub cell: Option<usize>,
}

/// Summary of one registered asset (no bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInfo {
    /// 12 lowercase hex chars; the placeholder is `asset:<id>`.
    pub id: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime: String,
    /// `"file:<path relative to base_dir>"` or `"inline:<byte length>"`.
    pub origin: String,
}

/// Output of any frontend (`markdown::parse`, `notebook::parse`; ir.md §4).
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The deck.
    pub deck: Deck,
    /// Assets referenced from the deck's HTML by `asset:<id>` placeholders.
    pub assets: AssetRegistry,
    /// Warnings (and non-fatal errors) found while parsing, e.g. `missing-asset`.
    pub diagnostics: Diagnostics,
}
