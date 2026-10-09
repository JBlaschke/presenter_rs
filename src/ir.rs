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
use crate::markdown::render::has_math_markup;

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
    /// Files pulled in with `:::include` (markdown-dialect §4.1), transitively, absolute, in
    /// first-use order. Serialized only when non-empty. (T48, contract change 2026-10-09)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<PathBuf>,
}

impl Deck {
    /// Checks the mechanically checkable invariants of ir.md §2 and returns one message per
    /// violation (empty when the deck is well-formed). Frontend tests assert this is empty.
    ///
    /// Checked: children have no children (1); `attrs` keys are lowercase and never `id`,
    /// `class` or `layout` (2); `layout` is non-empty (3); every `asset:<12 hex>` placeholder
    /// in `Block::Html` / `CellOutput::Display` HTML, in `attrs` values (e.g. a registered
    /// `data-background-image`) and in `notes` is listed in `assets` (4); features
    /// implied by content are set — `notes` for notes, `notebook` and `code` for cells,
    /// `math` for `text/latex` outputs and for math markup in `Block::Html`, in
    /// `CellOutput::Display` HTML or in `notes` (6); spans are 1-based and ordered (8), and a
    /// span's `file` (a slide from an included file, T48) is listed in `includes`.
    ///
    /// Math markup is what [`has_math_markup`] finds, the test the Markdown frontend derives
    /// `features.math` from (in the slide HTML and the notes): a start tag with a
    /// `data-math-style` attribute, its name matched case-insensitively as browsers do,
    /// outside comments and raw-text elements such as `<script>`/`<style>`. So the literal
    /// text `data-math-style`, e.g. in inline code, is not math. Every frontend must set
    /// `features.math` at least for the HTML and notes for which [`has_math_markup`] is
    /// true: a broader test only loads KaTeX needlessly, a narrower one (a case-sensitive
    /// substring test misses `<span DATA-MATH-STYLE=…>`) is a violation here.
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
        for (key, value) in &slide.attrs {
            if matches!(key.as_str(), "id" | "class" | "layout") || key.to_lowercase() != *key {
                problems.push(format!("{path}: attrs key {key:?} not allowed"));
            }
            self.check_placeholders(value, &format!("{path}.attrs[{key:?}]"), problems);
        }
        if let Some(span) = &slide.span
            && (span.start_line == 0 || span.end_line < span.start_line)
        {
            problems.push(format!("{path}: bad span {span:?}"));
        }
        if let Some(file) = slide.span.as_ref().and_then(|span| span.file.as_ref())
            && !self.includes.contains(file)
        {
            problems.push(format!(
                "{path}: span.file {} is not in includes",
                file.display()
            ));
        }
        if let Some(notes) = &slide.notes {
            if !self.features.notes {
                problems.push(format!("{path}: has notes but features.notes is false"));
            }
            if !self.features.math && has_math_markup(notes) {
                problems.push(format!(
                    "{path}.notes: math in notes but features.math is false"
                ));
            }
            self.check_placeholders(notes, &format!("{path}.notes"), problems);
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
        if !self.features.math && has_math_markup(html) {
            problems.push(format!("{path}: math in HTML but features.math is false"));
        }
        self.check_placeholders(html, path, problems);
    }

    /// Every `asset:<12 hex>` in `text` must name an entry of `self.assets`.
    fn check_placeholders(&self, text: &str, path: &str, problems: &mut Vec<String>) {
        for (at, _) in text.match_indices("asset:") {
            let id = &text[at + "asset:".len()..];
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
    /// The file the lines belong to when it is not the deck's own source (an included file,
    /// markdown-dialect §4.1); absolute. Serialized only when set. (T48, 2026-10-09)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A deck with one slide holding `html`, with `features.math` set to `math`.
    fn deck_with(html: &str, math: bool) -> Deck {
        let mut slide = Slide::new();
        slide.blocks.push(Block::html(html.to_string()));
        Deck {
            meta: DeckMeta::default(),
            slides: vec![slide],
            features: Features {
                math,
                ..Features::default()
            },
            source: SourceInfo {
                path: PathBuf::from("/x/deck.md"),
                kind: SourceKind::Markdown,
                base_dir: PathBuf::from("/x"),
            },
            assets: vec![],
            includes: Vec::new(),
        }
    }

    fn math_problems(deck: &Deck) -> Vec<String> {
        deck.invariant_violations()
            .into_iter()
            .filter(|p| p.contains("features.math"))
            .collect()
    }

    #[test]
    fn math_markup_without_features_math_is_a_violation() {
        for html in [
            r#"<p><span data-math-style="inline">x^2</span></p>"#,
            r#"<div data-math-style="display">\\int</div>"#,
            r#"<P><SPAN class="m" DATA-MATH-STYLE='inline'>x</SPAN></P>"#,
            "<!-- c --><span data-math-style=inline>x</span>",
            "<script>a</script><span data-math-style>x</span>",
        ] {
            assert_eq!(
                math_problems(&deck_with(html, false)),
                ["slides[0]: math in HTML but features.math is false"],
                "{html}"
            );
            assert_eq!(
                deck_with(html, true).invariant_violations(),
                Vec::<String>::new()
            );
        }
        // A notebook output's HTML is checked the same way.
        let mut deck = deck_with("<p>x</p>", false);
        deck.features.code = true;
        deck.features.notebook = true;
        deck.slides[0].blocks.push(Block::Cell(NotebookCell {
            outputs: vec![CellOutput::Display {
                mime: "text/html".into(),
                html: r#"<span data-math-style="inline">y</span>"#.into(),
                execution_count: None,
            }],
            ..NotebookCell::default()
        }));
        assert_eq!(math_problems(&deck).len(), 1);
    }

    #[test]
    fn literal_math_attribute_text_is_not_math() {
        for html in [
            "<p><code>data-math-style</code></p>",
            r#"<pre><code>&lt;span data-math-style="inline"&gt;</code></pre>"#,
            r#"<p title="data-math-style=inline">x</p>"#,
            r#"<!-- <span data-math-style="inline">x</span> -->"#,
            r#"<script>document.write('<span data-math-style>x</span>')</script>"#,
            r#"<style>[data-math-style] { color: red }</style>"#,
        ] {
            assert_eq!(
                deck_with(html, false).invariant_violations(),
                Vec::<String>::new(),
                "{html}"
            );
        }
    }

    #[test]
    fn markdown_frontend_and_invariant_agree_on_math() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md");
        let source = SourceInfo {
            path: dir.join("unit.md"),
            kind: SourceKind::Markdown,
            base_dir: dir,
        };
        let config = crate::config::Config::default();
        let parse = |text: &str| crate::markdown::parse(&source, text, &config).unwrap();
        // Only literal text: the frontend derives no math and the invariant agrees.
        let parsed = parse("# A\n\nThe attribute `data-math-style` marks math.\n");
        assert!(!parsed.deck.features.math);
        assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
        // Real math: features.math is set; clearing it is a violation.
        let mut parsed = parse("# A\n\nInline $x^2$ math.\n");
        assert!(parsed.deck.features.math);
        assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
        parsed.deck.features.math = false;
        assert_eq!(math_problems(&parsed.deck).len(), 1);
        // Math only in the notes: the frontend sets features.math (T12 scans the notes too).
        let mut parsed = parse("# A\n\nText\n\n::: notes\nSay $x^2$.\n:::\n");
        assert!(parsed.deck.features.math);
        assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
        parsed.deck.features.math = false;
        assert_eq!(
            math_problems(&parsed.deck),
            ["slides[0].notes: math in notes but features.math is false"]
        );
        // Literal text in the notes is not math.
        let parsed = parse("# A\n\n::: notes\nThe `data-math-style` attribute.\n:::\n");
        assert!(!parsed.deck.features.math);
        assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
    }

    #[test]
    fn math_in_notes_is_checked_like_html() {
        let with_notes = |notes: &str, math: bool| {
            let mut deck = deck_with("<p>x</p>", math);
            deck.features.notes = true;
            deck.slides[0].notes = Some(notes.to_string());
            let mut child = Slide::new();
            child.notes = Some(notes.to_string());
            deck.slides[0].children.push(child);
            deck
        };
        let math = r#"<p>Say <SPAN Data-Math-Style="inline">x^2</SPAN>.</p>"#;
        assert_eq!(
            with_notes(math, false).invariant_violations(),
            [
                "slides[0].notes: math in notes but features.math is false",
                "slides[0].children[0].notes: math in notes but features.math is false",
            ]
        );
        assert_eq!(
            with_notes(math, true).invariant_violations(),
            Vec::<String>::new()
        );
        for literal in [
            "<p><code>data-math-style</code></p>",
            "<!-- <span data-math-style>x</span> -->",
        ] {
            assert_eq!(
                with_notes(literal, false).invariant_violations(),
                Vec::<String>::new(),
                "{literal}"
            );
        }
    }

    /// T32's output renderer and the invariant agree on the outputs notebooks produce. (Its
    /// case-sensitive substring test misses an upper-case `DATA-MATH-STYLE` attribute, which
    /// the invariant counts; see the W3FIX contract notes.)
    #[test]
    fn notebook_outputs_and_invariant_agree_on_math() {
        use crate::assets::registry::AssetRegistry;
        use crate::notebook::{nbformat::Output, outputs::render_outputs};
        let render = |data: serde_json::Value| {
            let outputs: Vec<Output> = serde_json::from_value(serde_json::json!([
                { "output_type": "display_data", "data": data, "metadata": {} }
            ]))
            .unwrap();
            let (mut assets, mut diags) = (AssetRegistry::new(), Diagnostics::new());
            let mut md = |text: &str| Ok(text.to_string());
            let cfg = crate::config::NotebookConfig::default();
            let (outputs, features) =
                render_outputs(&outputs, &cfg, &mut assets, &mut diags, &mut md).unwrap();
            let mut deck = deck_with("<p>x</p>", features.math);
            deck.features.code = true;
            deck.features.notebook = true;
            deck.slides[0].blocks.push(Block::Cell(NotebookCell {
                outputs,
                ..NotebookCell::default()
            }));
            deck
        };
        let math = render(serde_json::json!({
            "text/html": r#"<p><span data-math-style="inline">x</span></p>"#
        }));
        assert!(math.features.math);
        assert_eq!(math.invariant_violations(), Vec::<String>::new());
        let text = render(serde_json::json!({ "text/plain": r#"<span data-math-style="x">"# }));
        assert!(!text.features.math);
        assert_eq!(text.invariant_violations(), Vec::<String>::new());
    }
}
