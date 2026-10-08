//! Cells → slides (`slideshow.slide_type`, tags, raw cells, notes). Implemented by T31.
//!
//! Contract: `docs/spec/notebook-mapping.md` §2 (cells → slides), §3 (tags and metadata) and
//! §5 (deck metadata). [`convert`] walks the cells of a parsed [`Notebook`] once:
//!
//! - **Deck metadata** (§5, later wins): `metadata.title`, then `metadata.presenter`
//!   ([`meta_from_json`]), then a `---` YAML block at the start of the **first markdown cell**
//!   ([`split_front_matter`] + [`parse_front_matter`]; removed from that cell). The result is
//!   merged into the config ([`Config::merge_front_matter`]) *before* the cells are converted,
//!   so `notebook:` tag lists, `markdown:` options, … from the notebook take effect. The
//!   config tables (`slides`, `markdown`, `math`, `notebook`) of the layers are merged key by
//!   key, `reveal` too; an invalid table names the layer it came from. First-cell front
//!   matter whose lines are only YAML comments (`---\n# Intro\n---`) sets nothing and is
//!   not shown: [`codes::FRONT_MATTER_EMPTY`] says so. A `---` block at the start of any
//!   *other* markdown cell that sets deck metadata (a [`DeckMeta`] field, `reveal` or a
//!   config table) is not deck metadata either: it is removed from that cell, with a
//!   [`codes::FRONT_MATTER_IGNORED`] warning (as the `frontmatter.ipynb` fixture expects).
//!   Any other `---` block there (prose such as `Note: this matters` between rules) is
//!   Markdown.
//! - **Removal** (§3): a cell tagged with one of `[notebook] remove_cell_tags`, a `skip`
//!   slide type and an unrecognized `cell_type` (already reported by `nbformat`) are ignored:
//!   they open no slide.
//! - **Structure** (§2): `slide` opens a top-level slide, `subslide` a vertical child of the
//!   current top-level slide (or a top-level slide when there is none yet). Anything before
//!   the first slide marker lands on slide 1, which the first such cell creates. The
//!   *current slide* (`cur`) is the last child of the last top-level slide, else that slide.
//! - **Contentless cells** add nothing to any slide: the first markdown cell when it holds
//!   only front matter (deck metadata, like front matter in a Markdown file), a code cell
//!   that is both `hide_input` and `hide_output` (§2: "omitted entirely") and a raw cell
//!   that is not shown. Their slide type counts once a slide exists (a `slide` cell opens an
//!   empty slide the next cells fill, as in nbconvert) and their `metadata.presenter`
//!   applies to the slide they open (ignored with a warning otherwise). Before the first
//!   slide they open nothing, like a leading separator in a Markdown file (a hidden setup
//!   cell at the top adds no empty slide 1): the next cell creates slide 1, and their
//!   `metadata.presenter` applies to that slide.
//! - **Placement**: a `notes` slide type or tag appends the rendered cell to `cur.notes`;
//!   everything else appends blocks to `cur`, as a fragment for a `fragment` slide type or
//!   tag. Tags and slide types combine: a `slide` cell tagged `fragment` opens a slide whose
//!   first content is a fragment, a `slide` cell tagged `notes` opens a slide and becomes its
//!   notes.
//! - **Markdown cells**: T12's renderer in notebook mode
//!   ([`render_fragment_with_lift`]); the attributes of a leading `# Heading {…}` are lifted
//!   onto `cur` only while `cur` has no blocks yet. One cell is one [`Block::Html`], wrapped
//!   in `<div class="fragment">` for fragments. `:::notes` go to `cur.notes`. Attachments
//!   (`attachment:NAME` in the source) are registered as inline assets and the URL becomes
//!   `asset:<id>`: the name as written and its `encodeURI` spelling in the source, any
//!   other spelling that names the attachment once HTML entities and percent escapes are
//!   decoded in the rendered HTML (body and notes). An `attachment:` URL left after that is
//!   a skipped attachment (`notebook-attachment-skipped`, reported once) or warns
//!   [`codes::MISSING_ATTACHMENT`]. With `[notebook] split_markdown_cells = true`, `---` /
//!   `--` separators inside a cell open new slides exactly as in a Markdown deck
//!   ([`split_with_source`]); the part before the first separator continues `cur`.
//! - **Code cells**: [`Block::Cell`] with the outputs of [`render_outputs`] (T32); relative
//!   `src`/`poster` URLs in `text/html` outputs go through T12's raw-HTML asset rewriting.
//!   `hide_input` / `hide_output` come from the configured tag lists and
//!   `metadata.jupyter.source_hidden` / `outputs_hidden` (a cell with both is contentless,
//!   see above). `:::notes` in a `text/markdown` output go to `cur.notes` unless the outputs
//!   are hidden.
//! - **Raw cells**: `raw_mimetype` (or `format`) `text/html` → the source as a
//!   [`Block::Html`], relative URLs rewritten like raw HTML in Markdown; any other raw cell
//!   is skipped with a [`codes::RAW_CELL_SKIPPED`] warning (and is contentless).
//! - **Per-cell `metadata.presenter`** (`{ "layout", "classes", "attrs" }`) goes through the
//!   slide-attribute normalization of Markdown separators (`SlideAttrs`, so shorthands such
//!   as `background` work) and applies to the slide the cell opens (a `slide`/`subslide`
//!   cell, or the cell that creates slide 1; in a split markdown cell that opens no slide
//!   itself, the first slide one of its separators opens); on any other cell it is ignored
//!   with a [`codes::OVERRIDE_IGNORED`] warning. A separator's or a lifted heading's
//!   attributes win over it, as a heading wins over a separator in Markdown.
//! - **Titles** (§2): a slide's title is the plain text of the first heading in the slide's
//!   first [`Block::Html`]: by the Markdown rule for a Markdown cell, the first `<h1>`–`<h6>`
//!   element for a raw HTML cell (found by T12's HTML tokenizer: headings in comments,
//!   raw-text elements such as `<script>` and `<template>` content do not count). A slide
//!   whose first HTML block has no heading has none.
//! - **Spans**: a slide's span is the whole source of the cell that opened it
//!   (`cell: Some(index)`); a slide opened by a separator inside a split cell spans the
//!   separator's lines.
//!
//! Every diagnostic carries the notebook path and a span with `cell: Some(index)`; lines are
//! lines of the cell source (front matter included).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use comrak::nodes::AstNode;
use comrak::{Arena, Options, parse_document};
use regex::Regex;
use serde_json::Value;

use crate::assets::registry::AssetRegistry;
use crate::config::Config;
use crate::error::{Diagnostic, Diagnostics, Error, Result};
use crate::ir::{
    Block, CellOutput, Deck, DeckMeta, Features, NotebookCell, Slide, SourceInfo, SourceSpan,
};
use crate::markdown::attrs::{ParsedAttrs, SlideAttrs};
use crate::markdown::comrak_options;
use crate::markdown::frontmatter::{meta_from_json, parse_front_matter, split_front_matter};
use crate::markdown::render::{
    RenderCtx, RenderMode, Rendered, has_math_markup, html_start_tags, percent_decode,
    render_fragment, render_fragment_with_lift, render_nodes, rewrite_html_assets,
    rewrite_slide_assets,
};
use crate::markdown::split::{RawSlide, SplitMode, split_with_source};
use crate::notebook::nbformat::{self, Cell, CellType, MultilineString, Notebook, SlideType};
use crate::notebook::outputs::render_outputs;

/// The diagnostic codes [`convert`] records (all `Level::Warning`). Notebook parsing
/// (`nbformat::codes`), outputs (`outputs::codes`) and Markdown rendering add their own.
pub mod codes {
    /// A slide without blocks or notes (same code as in Markdown decks).
    pub const EMPTY_SLIDE: &str = "empty-slide";
    /// A slide attribute key that is not a known shorthand; kept verbatim (same code as in
    /// Markdown decks).
    pub const UNKNOWN_SLIDE_ATTR: &str = "unknown-slide-attr";
    /// A slide attribute key that is not a valid attribute name; dropped (same code as in
    /// Markdown decks).
    pub const INVALID_SLIDE_ATTR: &str = "invalid-slide-attr";
    /// A cell's `metadata.presenter` override on a cell that does not open a slide.
    pub const OVERRIDE_IGNORED: &str = "presenter-override-ignored";
    /// An unknown key or a wrongly typed value in a cell's `metadata.presenter`; ignored.
    pub const OVERRIDE_INVALID: &str = "presenter-override-invalid";
    /// A raw cell whose `raw_mimetype` is not `text/html`; not rendered.
    pub const RAW_CELL_SKIPPED: &str = "raw-cell-skipped";
    /// An `attachment:NAME` URL whose name, as written or decoded, is not among the cell's
    /// attachments.
    pub const MISSING_ATTACHMENT: &str = "missing-attachment";
    /// A `---` front matter block that sets deck metadata at the start of a markdown cell
    /// that is not the first markdown cell; it is removed from the cell and does not change
    /// the deck metadata.
    pub const FRONT_MATTER_IGNORED: &str = "front-matter-ignored";
    /// Front matter of the first markdown cell that sets nothing although it has text (only
    /// YAML comments, e.g. `# Intro` between `---` lines); that text is not shown.
    pub const FRONT_MATTER_EMPTY: &str = "front-matter-empty";
}

/// The tag that makes a cell a fragment (spec §3; not configurable).
const FRAGMENT_TAG: &str = "fragment";
/// The tag that makes a cell speaker notes (spec §3; not configurable).
const NOTES_TAG: &str = "notes";
/// The config tables front matter may override (config.md §3); merged key by key.
const CONFIG_TABLES: [&str; 4] = ["slides", "markdown", "math", "notebook"];
/// Splitter diagnostics kept for split markdown cells (the others are recomputed per deck).
const SEPARATOR_NOT_RECOGNIZED: &str = "separator-not-recognized";
/// The splitter's report of a leading `--`, which in a cell opens a vertical slide.
const ORPHAN_VERTICAL: &str = "orphan-vertical";

/// The scheme of a notebook attachment URL (`attachment:NAME`).
const ATTACHMENT_SCHEME: &str = "attachment:";

/// `src`, `href` or `poster` values (double- or single-quoted) that still point at an
/// attachment after the source rewrite; the name is group 1 or 2, as written in the HTML,
/// and [`ATTACHMENT_SCHEME`] directly precedes it. Unquoted values are not matched: in
/// rendered Markdown they would also match HTML shown in code (`&lt;img src=attachment:…`).
static ATTACHMENT_URL: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r#"(?i:src|href|poster)\s*=\s*(?:"attachment:([^"]*)"|'attachment:([^']*)')"#)
        .expect("attachment URL regex")
});

/// An HTML comment, possibly unclosed (for the plain text of a heading).
static HTML_COMMENT: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"(?s)<!--.*?(?:-->|\z)").expect("HTML comment regex")
});

/// An HTML tag (for the plain text of a heading).
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"<[^>]*>").expect("HTML tag regex")
});

/// Converts a parsed notebook into a [`Deck`] (see the module docs).
///
/// `config` receives the deck metadata (`Config::merge_front_matter`), so afterwards it is
/// the config the notebook asked for. Assets (attachments, output images, files named by
/// raw HTML) are registered in `assets`; `Deck.assets` summarizes it. Warnings are appended
/// to `diags` with the notebook path and cell spans.
///
/// # Errors
/// [`Error::FrontMatter`] for invalid `metadata.presenter` or first-cell front matter,
/// including config tables or a `reveal.plugins` list the config rejects (the message
/// starts with its origin: `metadata.presenter: ` or `cell N: `); [`Error::Parse`] for a
/// separator with an unbalanced quote in a split markdown cell.
pub fn convert(
    nb: &Notebook,
    source: &SourceInfo,
    config: &mut Config,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
) -> Result<Deck> {
    let front = find_front_matter(nb);
    let (meta, layers) = deck_meta(nb, front.as_ref())?;
    merge_config(config, &meta, &layers)?;
    if let Some(warning) = front.as_ref().and_then(empty_front_matter) {
        diags.push(warning.with_path(&source.path));
    }
    let mut converter = Converter::new(nb, source, config, front, assets, diags);
    for (index, cell) in nb.cells.iter().enumerate() {
        converter.cell(index, cell)?;
    }
    let (slides, features) = converter.finish()?;
    Ok(Deck {
        meta,
        slides,
        features,
        source: source.clone(),
        assets: assets.infos(&source.base_dir),
    })
}

/// The front matter block of the first markdown cell.
#[derive(Debug, Clone, Copy)]
struct FrontMatter<'a> {
    /// Index of the first markdown cell.
    cell: usize,
    /// The YAML between the delimiters.
    yaml: &'a str,
    /// The cell source after the block.
    rest: &'a str,
    /// Lines taken by the block: `rest` starts on line `consumed + 1`.
    consumed: usize,
}

/// Front matter at the start of the first markdown cell, if any (spec §5).
fn find_front_matter(nb: &Notebook) -> Option<FrontMatter<'_>> {
    let (cell, first) = nb
        .cells
        .iter()
        .enumerate()
        .find(|(_, cell)| cell.cell_type == CellType::Markdown)?;
    let (yaml, rest, consumed) = split_front_matter(first.source.as_str())?;
    Some(FrontMatter {
        cell,
        yaml,
        rest,
        consumed,
    })
}

/// One layer of deck metadata and where it came from (`metadata.presenter`, `cell N`).
type Layer = (String, DeckMeta);

/// Deck metadata from the three notebook layers of spec §5 (later wins), and the two layers
/// that may carry config tables, with their origin.
fn deck_meta(nb: &Notebook, front: Option<&FrontMatter<'_>>) -> Result<(DeckMeta, Vec<Layer>)> {
    let mut layers = Vec::new();
    if let Some(presenter) = nb.presenter_meta() {
        let context = "metadata.presenter".to_owned();
        let layer = meta_from_json(presenter).map_err(|err| in_context(err, &context))?;
        layers.push((context, layer));
    }
    if let Some(front) = front {
        let context = format!("cell {}", front.cell);
        let layer = parse_front_matter(front.yaml).map_err(|err| {
            let hint = "(a `---` block at the start of the first markdown cell is read as \
                        YAML front matter)";
            match in_context(err, &context) {
                Error::FrontMatter(message) => Error::FrontMatter(format!("{message} {hint}")),
                other => other,
            }
        })?;
        layers.push((context, layer));
    }
    let mut meta = DeckMeta {
        title: nb.title().map(str::to_owned),
        ..DeckMeta::default()
    };
    for (_, layer) in &layers {
        overlay(&mut meta, layer.clone());
    }
    Ok((meta, layers))
}

/// Merges the deck metadata into `config`. Every layer is first merged on its own, in
/// order, into a scratch copy, so that an invalid config table or `reveal.plugins` list
/// names the layer it came from.
fn merge_config(config: &mut Config, meta: &DeckMeta, layers: &[Layer]) -> Result<()> {
    let mut scratch = config.clone();
    for (context, layer) in layers {
        scratch
            .merge_front_matter(layer)
            .map_err(|err| in_context(err, context))?;
    }
    config.merge_front_matter(meta)
}

/// Prefixes a front matter error message with where the metadata came from.
fn in_context(err: Error, context: &str) -> Error {
    match err {
        Error::FrontMatter(message) => Error::FrontMatter(format!("{context}: {message}")),
        other => other,
    }
}

/// A [`codes::FRONT_MATTER_EMPTY`] warning (without path) when the first markdown cell's
/// front matter sets nothing and has YAML comments: Jupyter shows them as Markdown (`# Intro`
/// between two rules), here they would otherwise vanish silently.
fn empty_front_matter(front: &FrontMatter<'_>) -> Option<Diagnostic> {
    let comments = front
        .yaml
        .lines()
        .any(|line| line.trim_start().starts_with('#'));
    if !comments || !parse_front_matter(front.yaml).is_ok_and(|meta| meta == DeckMeta::default()) {
        return None;
    }
    let message = "the `---` block at the start of the first markdown cell is read as YAML \
                   front matter and sets nothing; its text (YAML comments, such as `# …` \
                   lines) is not shown";
    let span = SourceSpan {
        start_line: 1,
        end_line: front.consumed,
        cell: Some(front.cell),
    };
    Some(Diagnostic::warning(codes::FRONT_MATTER_EMPTY, message).with_span(span))
}

/// Front matter that sets deck metadata: a [`DeckMeta`] field, `reveal` options or a config
/// table. Unknown keys alone do not count: `Note: this matters` between two `---` rules in
/// a later cell is prose.
fn sets_deck_metadata(meta: &DeckMeta) -> bool {
    let DeckMeta {
        title,
        subtitle,
        author,
        date,
        description,
        lang,
        theme,
        reveal,
        vars,
    } = meta;
    [title, subtitle, author, date, description, lang, theme]
        .iter()
        .any(|field| field.is_some())
        || !reveal.is_empty()
        || CONFIG_TABLES.iter().any(|table| vars.contains_key(*table))
}

/// Lays `later` over `meta`: set string fields replace, `reveal` keys replace, `vars` keys
/// replace except the config tables, which merge key by key (an empty table keeps the
/// earlier one).
fn overlay(meta: &mut DeckMeta, later: DeckMeta) {
    let DeckMeta {
        title,
        subtitle,
        author,
        date,
        description,
        lang,
        theme,
        reveal,
        vars,
    } = later;
    let fields = [
        (&mut meta.title, title),
        (&mut meta.subtitle, subtitle),
        (&mut meta.author, author),
        (&mut meta.date, date),
        (&mut meta.description, description),
        (&mut meta.lang, lang),
        (&mut meta.theme, theme),
    ];
    for (slot, value) in fields {
        if value.is_some() {
            *slot = value;
        }
    }
    meta.reveal.extend(reveal);
    for (key, value) in vars {
        let table = CONFIG_TABLES.contains(&key.as_str());
        match (meta.vars.get_mut(&key), value) {
            (Some(Value::Object(base)), Value::Object(over)) if table => base.extend(over),
            (Some(Value::Object(_)), Value::Null) if table => {}
            (_, value) => {
                meta.vars.insert(key, value);
            }
        }
    }
}

/// Where rendered content and its diagnostics belong: one part of a split markdown cell.
struct Part<'n, 'c> {
    arena: &'n Arena<'n>,
    cell: &'c Cell,
    index: usize,
    offset: usize,
    fragment: bool,
}

/// The cell walk of spec §2 (see the module docs).
struct Converter<'a> {
    nb: &'a Notebook,
    source: &'a SourceInfo,
    config: &'a Config,
    opts: Options<'static>,
    front: Option<FrontMatter<'a>>,
    assets: &'a mut AssetRegistry,
    diags: &'a mut Diagnostics,
    /// Finished top-level slides.
    done: Vec<Slide>,
    /// The open top-level slide (with its children).
    top: Option<Slide>,
    /// The cell being converted opened (or created) the current slide.
    opened_here: bool,
    /// `metadata.presenter` of contentless cells before slide 1, for slide 1.
    pending: Vec<(SourceSpan, &'a Value)>,
    /// `metadata.presenter` of the split markdown cell being converted, for the first slide
    /// one of its separators opens.
    deferred: Option<(SourceSpan, &'a Value)>,
    features: Features,
    /// Attachments registered so far, by (cell index, name): the asset id, or `None` when
    /// the attachment was skipped (and reported once).
    attachment_ids: BTreeMap<(usize, String), Option<String>>,
}

impl<'a> Converter<'a> {
    fn new(
        nb: &'a Notebook,
        source: &'a SourceInfo,
        config: &'a Config,
        front: Option<FrontMatter<'a>>,
        assets: &'a mut AssetRegistry,
        diags: &'a mut Diagnostics,
    ) -> Self {
        Converter {
            nb,
            source,
            config,
            opts: comrak_options(&config.markdown),
            front,
            assets,
            diags,
            done: Vec::new(),
            top: None,
            opened_here: false,
            pending: Vec::new(),
            deferred: None,
            features: Features::default(),
            attachment_ids: BTreeMap::new(),
        }
    }

    /// One iteration of the §2 walk.
    fn cell(&mut self, index: usize, cell: &'a Cell) -> Result<()> {
        let tags = cell.tags();
        let slide_type = cell.slide_type();
        if cell.cell_type == CellType::Unknown
            || slide_type == SlideType::Skip
            || has_any(&tags, &self.config.notebook.remove_cell_tags)
        {
            return Ok(());
        }
        let hidden = self.hidden(cell, &tags);
        let contentless = self.is_contentless(index, cell, hidden);
        self.structure(index, cell, slide_type, contentless);
        let notes = slide_type == SlideType::Notes || has_tag(&tags, NOTES_TAG);
        let split = cell.cell_type == CellType::Markdown
            && !notes
            && !contentless
            && self.config.notebook.split_markdown_cells;
        self.overrides(index, cell, contentless, split);
        if contentless {
            // Only a skipped raw cell's warning is left to give.
            if cell.cell_type == CellType::Raw {
                self.raw_html(index, cell)?;
            }
            return Ok(());
        }
        if notes {
            return self.notes_cell(index, cell);
        }
        let fragment = slide_type == SlideType::Fragment || has_tag(&tags, FRAGMENT_TAG);
        match cell.cell_type {
            CellType::Markdown => self.markdown_cell(index, cell, fragment),
            CellType::Code => self.code_cell(index, cell, tags, hidden, fragment),
            CellType::Raw => self.raw_cell(index, cell, fragment),
            CellType::Unknown => Ok(()),
        }
    }

    /// `(hide_input, hide_output)` of a code cell (spec §3: the configured tag lists and
    /// `metadata.jupyter`).
    fn hidden(&self, cell: &Cell, tags: &[String]) -> (bool, bool) {
        let config = &self.config.notebook;
        (
            cell.is_hidden_source() || has_any(tags, &config.hide_input_tags),
            cell.is_hidden_outputs() || has_any(tags, &config.hide_output_tags),
        )
    }

    /// The cell adds nothing to any slide: the first markdown cell holding only front
    /// matter, a code cell whose input and output are both hidden (`hidden`, §2: "omitted
    /// entirely") or a raw cell that is not shown (module docs).
    fn is_contentless(&self, index: usize, cell: &Cell, hidden: (bool, bool)) -> bool {
        match cell.cell_type {
            CellType::Markdown => self
                .front
                .is_some_and(|front| front.cell == index && front.rest.trim().is_empty()),
            CellType::Code => hidden == (true, true),
            CellType::Raw => !is_html_raw(cell),
            CellType::Unknown => false,
        }
    }

    /// The structure step of §2: `slide` opens a top-level slide, `subslide` a vertical
    /// child, and any cell creates slide 1 when there is none yet (ensure_first_slide). A
    /// contentless cell before the first slide opens nothing (module docs).
    fn structure(&mut self, index: usize, cell: &Cell, slide_type: SlideType, contentless: bool) {
        self.opened_here = false;
        if contentless && self.top.is_none() {
            return;
        }
        match slide_type {
            SlideType::Slide => self.open(false, cell.span(index)),
            SlideType::Subslide => self.open(true, cell.span(index)),
            _ if self.top.is_none() => self.open(false, cell.span(index)),
            _ => {}
        }
    }

    /// Opens a top-level slide, or a vertical child of the current top-level slide. Slide 1
    /// receives the overrides of the contentless cells before it.
    fn open(&mut self, vertical: bool, span: SourceSpan) {
        let slide = Slide {
            span: Some(span),
            ..Slide::new()
        };
        if vertical && let Some(top) = self.top.as_mut() {
            top.children.push(slide);
        } else if let Some(previous) = self.top.replace(slide) {
            self.done.push(previous);
        }
        self.opened_here = true;
        self.apply_pending();
    }

    /// Applies the overrides waiting for slide 1 (the current slide).
    fn apply_pending(&mut self) {
        for (span, value) in std::mem::take(&mut self.pending) {
            self.apply_presenter(value, &span);
        }
    }

    /// The slide receiving content: the last child of the open top-level slide, else that
    /// slide (created empty if there is none, which the walk never needs).
    fn cur(&mut self) -> &mut Slide {
        let top = self.top.get_or_insert_with(Slide::new);
        if top.children.is_empty() {
            return top;
        }
        let last = top.children.len() - 1;
        &mut top.children[last]
    }

    /// The current slide has no blocks yet (heading attributes may still be lifted).
    fn cur_is_empty(&self) -> bool {
        self.top
            .as_ref()
            .is_none_or(|top| top.children.last().unwrap_or(top).blocks.is_empty())
    }

    /// `slide N` / `slide N.M` (1-based) of the current slide, as in Markdown diagnostics.
    fn label(&self) -> String {
        let n = self.done.len() + 1;
        match self.top.as_ref().map_or(0, |top| top.children.len()) {
            0 => n.to_string(),
            m => format!("{n}.{m}"),
        }
    }

    fn warn(&mut self, code: &'static str, message: impl Into<String>, span: &SourceSpan) {
        self.diags.push(
            Diagnostic::warning(code, message)
                .with_path(&self.source.path)
                .with_span(span.clone()),
        );
    }

    /// Applies slide attributes to the current slide, reporting unknown and invalid keys.
    fn apply_attrs(&mut self, attrs: SlideAttrs, span: &SourceSpan) {
        let label = self.label();
        for key in &attrs.unknown_keys {
            let message =
                format!("unknown slide attribute `{key}` on slide {label} (kept verbatim)");
            self.warn(codes::UNKNOWN_SLIDE_ATTR, message, span);
        }
        for key in &attrs.invalid_keys {
            let message =
                format!("`{key}` on slide {label} is not a valid attribute name (dropped)");
            self.warn(codes::INVALID_SLIDE_ATTR, message, span);
        }
        attrs.apply_to(self.cur());
    }

    /// Per-cell `metadata.presenter` (spec §3): applied to the slide the cell opened, kept
    /// for slide 1 (a contentless cell before it) or for the first slide a separator of a
    /// `split` cell opens, else ignored with a warning.
    fn overrides(&mut self, index: usize, cell: &'a Cell, contentless: bool, split: bool) {
        self.deferred = None;
        let Some(value) = cell.presenter_overrides() else {
            return;
        };
        let span = cell.span(index);
        if self.opened_here {
            self.apply_presenter(value, &span);
        } else if contentless && self.top.is_none() {
            self.pending.push((span, value));
        } else if split {
            self.deferred = Some((span, value));
        } else {
            self.override_ignored(&span, false);
        }
    }

    fn override_ignored(&mut self, span: &SourceSpan, split: bool) {
        let opens = if split {
            "slide_type `slide` or `subslide`, or a separator in a split markdown cell"
        } else {
            "slide_type `slide` or `subslide`"
        };
        let message = format!(
            "metadata.presenter only applies to the slide a cell opens ({opens}); this cell \
             opens none, ignored"
        );
        self.warn(codes::OVERRIDE_IGNORED, message, span);
    }

    /// Applies one cell's `metadata.presenter` to the current slide.
    fn apply_presenter(&mut self, value: &Value, span: &SourceSpan) {
        let (parsed, problems) = presenter_attrs(value);
        for problem in problems {
            self.warn(
                codes::OVERRIDE_INVALID,
                format!("metadata.presenter: {problem}"),
                span,
            );
        }
        self.apply_attrs(SlideAttrs::from_parsed(parsed), span);
    }

    /// Runs `f` with a notebook-mode [`RenderCtx`]; its diagnostics get the notebook path
    /// and are moved into the cell (`at`): lines shift by `offset` (the front matter lines
    /// removed before rendering), a diagnostic without span gets `at`.
    fn render<T>(
        &mut self,
        at: Option<&SourceSpan>,
        offset: usize,
        f: impl FnOnce(&mut RenderCtx<'_>, &Options<'static>) -> Result<T>,
    ) -> Result<T> {
        let mut local = Diagnostics::new();
        let result = {
            let mut ctx = RenderCtx {
                assets: &mut *self.assets,
                base_dir: &self.source.base_dir,
                mode: RenderMode::Notebook,
                diagnostics: &mut local,
                missing_asset_is_error: false,
            };
            f(&mut ctx, &self.opts)
        };
        let path = &self.source.path;
        self.diags
            .extend(local.into_iter().map(|d| relocate(d, at, offset, path)));
        result
    }

    /// Adds a rendered Markdown fragment to the current slide.
    fn push_rendered(&mut self, rendered: Rendered, fragment: bool) {
        self.features.merge(rendered.features);
        if !rendered.html.is_empty() {
            self.push_html(rendered.html, rendered.title, fragment);
        }
        if let Some(notes) = rendered.notes {
            append_notes(self.cur(), &notes);
        }
    }

    /// Adds an HTML block to the current slide; `title` (the heading of this block) is the
    /// slide's title when this is the slide's first HTML block (§2).
    fn push_html(&mut self, html: String, title: Option<String>, fragment: bool) {
        let cur = self.cur();
        if !cur
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Html { .. }))
        {
            cur.title = title;
        }
        cur.blocks.push(Block::html(wrap_fragment(html, fragment)));
    }

    /// The Markdown source of a cell (front matter removed) with attachments rewritten, and
    /// the number of lines removed before it.
    fn markdown_text(&mut self, index: usize, cell: &'a Cell) -> (Cow<'a, str>, usize) {
        let front = self.front.filter(|front| front.cell == index);
        let (text, offset) = match front {
            Some(front) => (front.rest, front.consumed),
            None => self.without_ignored_front_matter(index, cell),
        };
        (self.with_attachments(index, cell, text), offset)
    }

    /// The source of a markdown cell other than the first one, without a leading `---` block
    /// that sets deck metadata ([`sets_deck_metadata`]): such a block is not deck metadata
    /// (§5) and is removed with a warning. Other `---` blocks are Markdown.
    fn without_ignored_front_matter(&mut self, index: usize, cell: &'a Cell) -> (&'a str, usize) {
        let source = cell.source.as_str();
        let Some((yaml, rest, consumed)) = split_front_matter(source) else {
            return (source, 0);
        };
        if !parse_front_matter(yaml).is_ok_and(|meta| sets_deck_metadata(&meta)) {
            return (source, 0);
        }
        let message = "front matter is only read from the first markdown cell; this `---` \
                       block is ignored and not shown";
        let span = SourceSpan {
            start_line: 1,
            end_line: consumed,
            cell: Some(index),
        };
        self.warn(codes::FRONT_MATTER_IGNORED, message, &span);
        (rest, consumed)
    }

    fn markdown_cell(&mut self, index: usize, cell: &'a Cell, fragment: bool) -> Result<()> {
        let (text, offset) = self.markdown_text(index, cell);
        if self.config.notebook.split_markdown_cells {
            self.split_cell(index, cell, &text, offset, fragment)?;
            if let Some((span, _)) = self.deferred.take() {
                self.override_ignored(&span, true);
            }
            return Ok(());
        }
        let lift = self.cur_is_empty();
        let span = cell.span(index);
        let (mut rendered, lifted) = self.render(Some(&span), offset, |ctx, opts| {
            render_fragment_with_lift(&text, opts, ctx, lift)
        })?;
        if let Some(attrs) = lifted {
            self.apply_attrs(SlideAttrs::from_parsed(attrs), &span);
        }
        self.resolve_rendered(index, cell, &mut rendered, &span);
        self.push_rendered(rendered, fragment);
        Ok(())
    }

    /// A markdown cell with `split_markdown_cells = true`: `---` / `--` open slides.
    fn split_cell(
        &mut self,
        index: usize,
        cell: &Cell,
        text: &str,
        offset: usize,
        fragment: bool,
    ) -> Result<()> {
        let arena = Arena::new();
        let mut opts = self.opts.clone();
        opts.extension.front_matter_delimiter = None;
        let root = parse_document(&arena, text, &opts);
        let Some(first) = root.first_child() else {
            return Ok(());
        };
        // The splitter lifts a leading heading's attributes; keep them on the heading
        // unless the current slide is still empty (§2).
        let kept = if self.cur_is_empty() {
            None
        } else {
            first.data_mut().attrs.take()
        };
        let raw = split_with_source(root, text, SplitMode::Rules)
            .map_err(|err| cell_error(err, &self.source.path, index, offset));
        if kept.is_some() {
            first.data_mut().attrs = kept;
        }
        let raw = raw?;
        let leading = !raw.slides.iter().any(|(slide, children)| {
            holds(slide, first) || children.iter().any(|child| holds(child, first))
        });
        let vertical = raw.diagnostics.iter().any(|d| d.code == ORPHAN_VERTICAL);
        let span = cell.span(index);
        let path = &self.source.path;
        self.diags.extend(
            raw.diagnostics
                .into_iter()
                .filter(|d| d.code == SEPARATOR_NOT_RECOGNIZED)
                .map(|d| relocate(d, Some(&span), offset, path)),
        );
        let part = Part {
            arena: &arena,
            cell,
            index,
            offset,
            fragment,
        };
        for (i, (slide, children)) in raw.slides.into_iter().enumerate() {
            match (i, leading) {
                (0, false) => {
                    self.apply_attrs(slide.attrs, &span);
                    self.render_part(&part, &slide.nodes, &span)?;
                }
                (0, true) => self.open_part(&part, slide, vertical, true)?,
                _ => self.open_part(&part, slide, false, false)?,
            }
            for child in children {
                self.open_part(&part, child, true, false)?;
            }
        }
        Ok(())
    }

    /// A slide opened by a separator inside a split markdown cell. A separator that starts
    /// the cell which just opened an empty slide opens nothing new (as a leading separator
    /// opens slide 1 itself in a Markdown deck); its attributes go to that slide. The first
    /// slide a separator opens receives the cell's deferred `metadata.presenter` (before
    /// the separator's own attributes, which win).
    fn open_part<'n>(
        &mut self,
        part: &Part<'n, '_>,
        raw: RawSlide<'n>,
        vertical: bool,
        leading: bool,
    ) -> Result<()> {
        let span = SourceSpan {
            start_line: raw.start_line + part.offset,
            end_line: raw.end_line + part.offset,
            cell: Some(part.index),
        };
        if !(leading && self.opened_here && self.cur_is_empty()) {
            self.open(vertical, span.clone());
            if let Some((at, value)) = self.deferred.take() {
                self.apply_presenter(value, &at);
            }
        }
        self.apply_attrs(raw.attrs, &span);
        self.render_part(part, &raw.nodes, &span)
    }

    fn render_part<'n>(
        &mut self,
        part: &Part<'n, '_>,
        nodes: &[&'n AstNode<'n>],
        span: &SourceSpan,
    ) -> Result<()> {
        let arena = part.arena;
        let mut rendered = self.render(Some(span), part.offset, |ctx, opts| {
            render_nodes(arena, nodes, opts, ctx)
        })?;
        self.resolve_rendered(part.index, part.cell, &mut rendered, span);
        self.push_rendered(rendered, part.fragment);
        Ok(())
    }

    /// A code cell (never both hidden: that cell is contentless) → [`Block::Cell`];
    /// `:::notes` of its Markdown outputs go to the slide's notes unless the outputs are
    /// hidden.
    fn code_cell(
        &mut self,
        index: usize,
        cell: &Cell,
        tags: Vec<String>,
        (hide_input, hide_output): (bool, bool),
        fragment: bool,
    ) -> Result<()> {
        let (outputs, notes) = self.outputs(index, cell)?;
        self.features.code = true;
        let block = Block::Cell(NotebookCell {
            index,
            id: cell.id.clone(),
            language: self.nb.language().to_owned(),
            source: cell.source.as_str().to_owned(),
            execution_count: cell.execution_count,
            outputs,
            hide_input,
            hide_output,
            fragment,
            tags,
            metadata: cell.metadata.clone(),
        });
        let cur = self.cur();
        cur.blocks.push(block);
        if let Some(notes) = notes.filter(|_| !hide_output) {
            append_notes(cur, &notes);
        }
        Ok(())
    }

    /// The cell's outputs (T32), with `text/html` URLs rewritten to assets, and the
    /// `:::notes` of its `text/markdown` outputs.
    fn outputs(&mut self, index: usize, cell: &Cell) -> Result<(Vec<CellOutput>, Option<String>)> {
        let span = cell.span(index);
        let mut local = Diagnostics::new();
        // `text/markdown` outputs render into their own registry (see `render_outputs`) and
        // their notes and features are collected here.
        let mut md_assets = AssetRegistry::new();
        let mut md_diags = Diagnostics::new();
        let mut md_notes: Option<String> = None;
        let mut md_features = Features::default();
        let (opts, base_dir) = (&self.opts, self.source.base_dir.as_path());
        let mut md = |text: &str| -> Result<String> {
            let mut ctx = RenderCtx {
                assets: &mut md_assets,
                base_dir,
                mode: RenderMode::Notebook,
                diagnostics: &mut md_diags,
                missing_asset_is_error: false,
            };
            let rendered = render_fragment(text, opts, &mut ctx)?;
            md_features.merge(rendered.features);
            if let Some(notes) = rendered.notes {
                md_notes.get_or_insert_with(String::new).push_str(&notes);
            }
            Ok(rendered.html)
        };
        let (mut outputs, features) = render_outputs(
            &cell.outputs,
            &self.config.notebook,
            self.assets,
            &mut local,
            &mut md,
        )?;
        self.assets.merge(md_assets);
        local.extend(md_diags);
        // `notes` follows the slides' notes (`finish_slide`), which hidden outputs skip.
        md_features.notes = false;
        self.features.merge(md_features);
        let mut ctx = RenderCtx {
            assets: &mut *self.assets,
            base_dir,
            mode: RenderMode::Notebook,
            diagnostics: &mut local,
            missing_asset_is_error: false,
        };
        for output in &mut outputs {
            if let CellOutput::Display { mime, html, .. } = output
                && mime == "text/html"
            {
                rewrite_html_assets(html, Some(&span), &mut ctx)?;
            }
        }
        self.features.merge(features);
        let path = &self.source.path;
        self.diags.extend(
            local
                .into_iter()
                .map(|d| d.with_path(path).with_span(span.clone())),
        );
        Ok((outputs, md_notes))
    }

    fn raw_cell(&mut self, index: usize, cell: &'a Cell, fragment: bool) -> Result<()> {
        if let Some(html) = self.raw_html(index, cell)? {
            let title = html_heading_text(&html);
            self.push_html(html, title, fragment);
        }
        Ok(())
    }

    /// The HTML of a raw cell with `raw_mimetype` `text/html` (attachments and relative
    /// URLs rewritten); `None` (with a warning unless the cell is blank) otherwise.
    fn raw_html(&mut self, index: usize, cell: &'a Cell) -> Result<Option<String>> {
        let source = cell.source.as_str();
        if source.trim().is_empty() {
            return Ok(None);
        }
        let span = cell.span(index);
        if !is_html_raw(cell) {
            let mimetype = cell.raw_mimetype();
            let why = match mimetype {
                Some(mime) => format!("its raw_mimetype is `{mime}`"),
                None => "it has no raw_mimetype".to_owned(),
            };
            let message = format!("raw cell skipped: {why}; only `text/html` raw cells are shown");
            self.warn(codes::RAW_CELL_SKIPPED, message, &span);
            return Ok(None);
        }
        let mut html = self.with_attachments(index, cell, source).into_owned();
        self.render(Some(&span), 0, |ctx, _| {
            rewrite_html_assets(&mut html, Some(&span), ctx)
        })?;
        self.resolve_attachments(index, cell, &mut html, &span);
        self.features.math |= has_math_markup(&html);
        Ok(Some(html))
    }

    /// A `notes` cell (slide type or tag): its rendering is appended to `cur.notes`.
    fn notes_cell(&mut self, index: usize, cell: &'a Cell) -> Result<()> {
        let html = match cell.cell_type {
            CellType::Markdown => {
                let (text, offset) = self.markdown_text(index, cell);
                let span = cell.span(index);
                let mut rendered = self.render(Some(&span), offset, |ctx, opts| {
                    render_fragment(&text, opts, ctx)
                })?;
                self.resolve_rendered(index, cell, &mut rendered, &span);
                self.features.merge(rendered.features);
                rendered.html + rendered.notes.as_deref().unwrap_or_default()
            }
            CellType::Code => code_notes(cell.source.as_str()),
            CellType::Raw => self.raw_html(index, cell)?.unwrap_or_default(),
            CellType::Unknown => String::new(),
        };
        if !html.trim().is_empty() {
            append_notes(self.cur(), &html);
        }
        Ok(())
    }

    /// `text` with every `attachment:NAME` URL of an attachment of `cell` replaced by the
    /// `asset:<id>` of the registered attachment. Only the name as written and its `encodeURI`
    /// spelling are known here; [`Self::resolve_attachments`] handles other spellings in the
    /// rendered HTML.
    fn with_attachments(&mut self, index: usize, cell: &Cell, text: &'a str) -> Cow<'a, str> {
        let mut text = Cow::Borrowed(text);
        for name in cell.attachments.keys() {
            let needles = attachment_needles(name);
            if !needles
                .iter()
                .any(|needle| url_starts(&text, needle).next().is_some())
            {
                continue;
            }
            let Some(id) = self.attachment_id(index, cell, name) else {
                continue;
            };
            let url = AssetRegistry::placeholder(&id);
            for needle in &needles {
                if let Some(replaced) = replace_url(&text, needle, &url) {
                    text = Cow::Owned(replaced);
                }
            }
        }
        text
    }

    /// Registers one attachment (the first MIME type in `mime_priority`, else the first
    /// one) by its bytes; `None` with a warning when it has no data or its payload cannot
    /// be decoded.
    fn register_attachment(
        &mut self,
        index: usize,
        cell: &Cell,
        name: &str,
        bundle: &BTreeMap<String, MultilineString>,
    ) -> Option<String> {
        let priority = &self.config.notebook.mime_priority;
        let chosen = priority
            .iter()
            .find_map(|mime| bundle.get_key_value(mime))
            .or_else(|| bundle.iter().next());
        let decoded = match chosen {
            Some((mime, payload)) => decode_attachment(payload.as_str()).map(|bytes| (mime, bytes)),
            None => Err("no data".to_owned()),
        };
        match decoded {
            Ok((mime, bytes)) => Some(self.assets.register_bytes(bytes, mime, name)),
            Err(reason) => {
                let message = format!("attachment '{name}' skipped: {reason}");
                self.warn(
                    nbformat::codes::ATTACHMENT_SKIPPED,
                    message,
                    &cell.span(index),
                );
                None
            }
        }
    }

    /// The asset id of `cell`'s attachment `name`, registered on first use
    /// ([`Self::register_attachment`]); `None` when it is not an attachment of `cell` or was
    /// skipped (reported once per cell and name, whichever spelling asks again).
    fn attachment_id(&mut self, index: usize, cell: &Cell, name: &str) -> Option<String> {
        let key = (index, name.to_owned());
        if let Some(id) = self.attachment_ids.get(&key) {
            return id.clone();
        }
        let id = cell
            .attachments
            .get(name)
            .and_then(|bundle| self.register_attachment(index, cell, name, bundle));
        self.attachment_ids.insert(key, id.clone());
        id
    }

    /// [`Self::resolve_attachments`] on a rendered markdown cell's body and notes.
    fn resolve_rendered(
        &mut self,
        index: usize,
        cell: &Cell,
        rendered: &mut Rendered,
        span: &SourceSpan,
    ) {
        self.resolve_attachments(index, cell, &mut rendered.html, span);
        if let Some(notes) = &mut rendered.notes {
            self.resolve_attachments(index, cell, notes, span);
        }
    }

    /// Resolves the `attachment:` URLs left in `html` ([`ATTACHMENT_URL`]) after
    /// [`Self::with_attachments`], which knows only two spellings of each name. A URL that
    /// names an attachment of `cell` as written, or once HTML entities and percent escapes
    /// are decoded (`attachment:a%2Bb.png` for `a+b.png`, lower-case hex, `a&amp;b.png` in raw
    /// HTML), becomes that attachment's `asset:<id>`. One whose attachment was skipped stays
    /// as written (already reported). Any other gets a [`codes::MISSING_ATTACHMENT`] warning
    /// naming it decoded, as written in the source; so no `attachment:` URL is left
    /// unreported.
    fn resolve_attachments(
        &mut self,
        index: usize,
        cell: &Cell,
        html: &mut String,
        span: &SourceSpan,
    ) {
        let mut resolved = String::new();
        let mut copied = None;
        let mut missing = Vec::new();
        for caps in ATTACHMENT_URL.captures_iter(html) {
            let Some(value) = caps.get(1).or_else(|| caps.get(2)) else {
                continue;
            };
            let written = value.as_str().trim_end();
            let unescaped = html_escape::decode_html_entities(written);
            let decoded = percent_decode(&unescaped);
            let found = [written, &unescaped, &decoded]
                .into_iter()
                .find(|name| cell.attachments.contains_key(*name))
                .map(str::to_owned);
            let Some(name) = found else {
                missing.push(decoded);
                continue;
            };
            if let Some(id) = self.attachment_id(index, cell, &name) {
                let start = value.start() - ATTACHMENT_SCHEME.len();
                resolved.push_str(&html[copied.unwrap_or(0)..start]);
                resolved.push_str(&AssetRegistry::placeholder(&id));
                copied = Some(value.start() + written.len());
            }
        }
        if let Some(copied) = copied {
            resolved.push_str(&html[copied..]);
            *html = resolved;
        }
        for name in missing {
            let message = format!("`attachment:{name}`: the cell has no attachment `{name}`");
            self.warn(codes::MISSING_ATTACHMENT, message, span);
        }
    }

    /// Closes the walk: one empty slide for a notebook without content (it receives the
    /// overrides of contentless cells), slide background assets, `empty-slide` warnings and
    /// the derived features.
    fn finish(mut self) -> Result<(Vec<Slide>, Features)> {
        if self.top.is_none() {
            self.top = Some(Slide::new());
            self.apply_pending();
        }
        if let Some(top) = self.top.take() {
            self.done.push(top);
        }
        let mut slides = std::mem::take(&mut self.done);
        for (i, slide) in slides.iter_mut().enumerate() {
            let label = (i + 1).to_string();
            self.finish_slide(slide, &label)?;
            for (j, child) in slide.children.iter_mut().enumerate() {
                self.finish_slide(child, &format!("{label}.{}", j + 1))?;
            }
        }
        self.features.notebook = true;
        Ok((slides, self.features))
    }

    fn finish_slide(&mut self, slide: &mut Slide, label: &str) -> Result<()> {
        let span = slide.span.clone();
        self.render(span.as_ref(), 0, |ctx, _| {
            rewrite_slide_assets(&mut slide.attrs, span.as_ref(), ctx)
        })?;
        if slide.blocks.is_empty() && slide.notes.is_none() {
            let has_attrs = slide.id.is_some()
                || slide.layout != Slide::new().layout
                || !slide.classes.is_empty()
                || !slide.attrs.is_empty();
            let only = if has_attrs {
                " (it only has attributes)"
            } else {
                ""
            };
            let mut diagnostic =
                Diagnostic::warning(codes::EMPTY_SLIDE, format!("slide {label} is empty{only}"))
                    .with_path(&self.source.path);
            diagnostic.span = span;
            self.diags.push(diagnostic);
        }
        self.features.notes |= slide.notes.is_some();
        Ok(())
    }
}

/// Moves a renderer diagnostic into a cell: path set; a span without a cell is shifted by
/// `offset` lines and gets `at`'s cell; no span becomes `at`.
fn relocate(
    mut diagnostic: Diagnostic,
    at: Option<&SourceSpan>,
    offset: usize,
    path: &Path,
) -> Diagnostic {
    if let Some(at) = at {
        match &mut diagnostic.span {
            Some(span) if span.cell.is_none() => {
                span.start_line += offset;
                span.end_line += offset;
                span.cell = at.cell;
            }
            Some(_) => {}
            None => diagnostic.span = Some(at.clone()),
        }
    }
    diagnostic.path.get_or_insert_with(|| path.to_path_buf());
    diagnostic
}

/// A splitter error inside a cell: the notebook path, the cell and the cell line in the
/// message (`Error::Parse.line` counts lines of the file, which a cell line is not).
fn cell_error(err: Error, path: &Path, index: usize, offset: usize) -> Error {
    match err {
        Error::Parse { line, message, .. } => {
            let at = match line {
                Some(line) => format!("cell {index}, line {}", line + offset),
                None => format!("cell {index}"),
            };
            Error::Parse {
                path: path.to_path_buf(),
                line: None,
                message: format!("{at}: {message}"),
            }
        }
        other => other,
    }
}

/// Whether `node` is one of the slide's top-level nodes.
fn holds<'n>(slide: &RawSlide<'n>, node: &'n AstNode<'n>) -> bool {
    slide.nodes.iter().any(|n| std::ptr::eq(*n, node))
}

fn has_tag(tags: &[String], tag: &str) -> bool {
    tags.iter().any(|t| t == tag)
}

/// A raw cell that is shown: `raw_mimetype` (or `format`) `text/html` (§2).
fn is_html_raw(cell: &Cell) -> bool {
    cell.raw_mimetype()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
}

/// The heading elements, for raw HTML titles.
const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// Plain text of the first `<h1>`–`<h6>` element of raw HTML, found by T12's HTML tokenizer
/// ([`html_start_tags`]): headings inside comments, raw-text elements (`<script>`,
/// `<textarea>`, …) and `<template>` content do not count. The element ends at the first
/// `</h1>`–`</h6>` (as in an HTML parser); comments and tags in it are removed (`<br>` → a
/// space), entities decoded, white space collapsed. `None` when there is no heading, it is
/// not closed or it is blank.
fn html_heading_text(html: &str) -> Option<String> {
    let mut inert_until = 0;
    for (name, end) in html_start_tags(html) {
        if end <= inert_until {
            continue;
        }
        if name == "template" {
            inert_until = end_tag(html, end, &["template"]).unwrap_or(html.len());
        } else if HEADINGS.contains(&name.as_str()) {
            let close = end_tag(html, end, &HEADINGS)?;
            return plain_text(&html[end..close]);
        }
    }
    None
}

/// Byte offset of the first end tag (`</name`, ASCII case-insensitive) of one of `names` at
/// or after byte `from`.
fn end_tag(html: &str, from: usize, names: &[&str]) -> Option<usize> {
    let bytes = html.as_bytes();
    html.get(from..)?
        .match_indices("</")
        .map(|(offset, _)| from + offset)
        .find(|&lt| {
            let after = &bytes[lt + 2..];
            names.iter().any(|name| {
                after.len() >= name.len()
                    && after[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                    && after
                        .get(name.len())
                        .is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
            })
        })
}

/// The text of an HTML fragment: comments and tags removed (`<br>` → a space), entities
/// decoded, white space collapsed; `None` when blank.
fn plain_text(html: &str) -> Option<String> {
    let html = HTML_COMMENT.replace_all(html, "");
    let text = HTML_TAG.replace_all(&html, |caps: &regex::Captures<'_>| {
        let tag = caps[0].to_ascii_lowercase();
        let br = tag.starts_with("<br") && !tag[3..].starts_with(|c: char| c.is_alphanumeric());
        if br { " " } else { "" }
    });
    let text = html_escape::decode_html_entities(&text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn has_any(tags: &[String], configured: &[String]) -> bool {
    tags.iter().any(|tag| configured.contains(tag))
}

/// `<div class="fragment">…</div>` around `html` when `fragment`.
fn wrap_fragment(mut html: String, fragment: bool) -> String {
    if !fragment {
        return html;
    }
    if !html.ends_with('\n') {
        html.push('\n');
    }
    format!("<div class=\"fragment\">\n{html}</div>\n")
}

fn append_notes(slide: &mut Slide, html: &str) {
    match &mut slide.notes {
        Some(notes) => notes.push_str(html),
        None => slide.notes = Some(html.to_owned()),
    }
}

/// Speaker notes from a code cell: its source as an escaped code block.
fn code_notes(source: &str) -> String {
    if source.trim().is_empty() {
        return String::new();
    }
    format!(
        "<pre><code>{}</code></pre>\n",
        html_escape::encode_text(source)
    )
}

/// `{ "layout", "classes", "attrs" }` as a raw attribute block (spec §3), and a message
/// for every key or value that is ignored.
fn presenter_attrs(value: &Value) -> (ParsedAttrs, Vec<String>) {
    let mut parsed = ParsedAttrs::default();
    let mut problems = Vec::new();
    let Some(object) = value.as_object() else {
        return (parsed, problems);
    };
    for (key, value) in object {
        match (key.as_str(), value) {
            (_, Value::Null) => {}
            ("layout", Value::String(layout)) => {
                parsed.pairs.push(("layout".to_owned(), layout.clone()));
            }
            ("classes", Value::String(classes)) => {
                parsed.classes.push(classes.clone());
            }
            ("classes", Value::Array(items)) => {
                for item in items {
                    match item.as_str() {
                        Some(class) => parsed.classes.push(class.to_owned()),
                        None => problems.push(format!("classes: {item} is not a string, ignored")),
                    }
                }
            }
            ("attrs", Value::Object(attrs)) => {
                for (name, value) in attrs {
                    match value {
                        Value::Null => {}
                        Value::String(text) => parsed.pairs.push((name.clone(), text.clone())),
                        Value::Number(_) | Value::Bool(_) => {
                            parsed.pairs.push((name.clone(), value.to_string()));
                        }
                        _ => {
                            problems.push(format!("attrs.{name}: {value} is not a scalar, ignored"))
                        }
                    }
                }
            }
            ("layout" | "classes" | "attrs", other) => {
                let expected = match key.as_str() {
                    "layout" => "a string",
                    "classes" => "a list of strings",
                    _ => "an object",
                };
                problems.push(format!("{key}: expected {expected}, got {other}, ignored"));
            }
            (other, _) => problems.push(format!(
                "unknown key `{other}` (expected layout, classes or attrs), ignored"
            )),
        }
    }
    (parsed, problems)
}

/// Decodes an attachment payload: base64 (whitespace ignored, padding optional), or the
/// text itself when it is markup (an SVG stored as text).
fn decode_attachment(payload: &str) -> std::result::Result<Vec<u8>, String> {
    if payload.trim_start().starts_with('<') {
        return Ok(payload.as_bytes().to_vec());
    }
    let compact: Vec<u8> = payload
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if compact.is_empty() {
        return Err("empty payload".to_owned());
    }
    STANDARD_PAD_INDIFFERENT
        .decode(&compact)
        .map_err(|err| format!("invalid base64: {err}"))
}

/// The spellings of an attachment URL: as named, and percent-encoded like JavaScript's
/// `encodeURI` (JupyterLab writes names with spaces as `attachment:my%20plot.png`).
fn attachment_needles(name: &str) -> Vec<String> {
    let mut needles = vec![format!("attachment:{name}")];
    let encoded = encode_uri(name);
    if encoded != name {
        needles.push(format!("attachment:{encoded}"));
    }
    needles
}

/// `encodeURI`: percent-encodes every byte except ASCII alphanumerics and `-_.!~*'();/?:@&=+$,#`.
fn encode_uri(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'();/?:@&=+$,#".contains(&byte) {
            out.push(char::from(byte));
        } else {
            // Writing to a String cannot fail.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Byte offsets of `needle` in `text` where it is a whole URL (followed by the end, white
/// space, a quote, `)`, `>`, `]`, `?` or `#`), so `attachment:a.png` never matches inside
/// `attachment:a.png.bak`.
fn url_starts<'t>(text: &'t str, needle: &'t str) -> impl Iterator<Item = usize> + 't {
    text.match_indices(needle).filter_map(move |(at, _)| {
        let next = text[at + needle.len()..].chars().next();
        next.is_none_or(|c| {
            c.is_whitespace() || matches!(c, ')' | '>' | ']' | '"' | '\'' | '?' | '#')
        })
        .then_some(at)
    })
}

/// `text` with every whole-URL occurrence of `needle` replaced by `url`; `None` when there
/// is none.
fn replace_url(text: &str, needle: &str, url: &str) -> Option<String> {
    let starts: Vec<usize> = url_starts(text, needle).collect();
    if starts.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for at in starts {
        out.push_str(&text[last..at]);
        out.push_str(url);
        last = at + needle.len();
    }
    out.push_str(&text[last..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn overlay_later_layers_win_and_config_tables_merge() {
        let mut meta = DeckMeta {
            title: Some("a".into()),
            author: Some("x".into()),
            ..DeckMeta::default()
        };
        meta.vars
            .insert("notebook".into(), json!({"show_prompts": false}));
        meta.vars.insert("event".into(), json!("one"));
        meta.reveal.insert("transition".into(), json!("fade"));
        let mut later = DeckMeta {
            title: Some("b".into()),
            ..DeckMeta::default()
        };
        later
            .vars
            .insert("notebook".into(), json!({"split_markdown_cells": true}));
        later.vars.insert("event".into(), json!("two"));
        later.vars.insert("slides".into(), Value::Null);
        later.reveal.insert("controls".into(), json!(false));
        overlay(&mut meta, later);
        assert_eq!(meta.title.as_deref(), Some("b"));
        assert_eq!(meta.author.as_deref(), Some("x"));
        assert_eq!(
            meta.vars["notebook"],
            json!({"show_prompts": false, "split_markdown_cells": true})
        );
        assert_eq!(meta.vars["event"], json!("two"));
        assert_eq!(meta.vars["slides"], Value::Null);
        assert_eq!(meta.reveal.len(), 2);
        // An empty table later keeps the earlier one.
        let mut empty = DeckMeta::default();
        empty.vars.insert("notebook".into(), Value::Null);
        overlay(&mut meta, empty);
        assert!(meta.vars["notebook"].is_object());
    }

    #[test]
    fn presenter_attrs_map_to_an_attribute_block() {
        let value = json!({
            "layout": "title",
            "classes": ["dark", 3],
            "attrs": {"background": "#111", "data-n": 2, "data-ok": true, "x": [1], "y": null},
            "id": "nope",
        });
        let (parsed, problems) = presenter_attrs(&value);
        assert_eq!(parsed.classes, ["dark"]);
        assert_eq!(
            parsed.pairs,
            [
                ("layout".to_owned(), "title".to_owned()),
                ("background".to_owned(), "#111".to_owned()),
                ("data-n".to_owned(), "2".to_owned()),
                ("data-ok".to_owned(), "true".to_owned()),
            ]
        );
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].starts_with("classes: 3"));
        assert!(problems[1].starts_with("attrs.x"));
        assert!(problems[2].starts_with("unknown key `id`"));
        let (parsed, problems) = presenter_attrs(&json!({"classes": "a b", "layout": 1}));
        assert_eq!(parsed.classes, ["a b"]);
        assert_eq!(problems, ["layout: expected a string, got 1, ignored"]);
        let attrs = SlideAttrs::from_parsed(presenter_attrs(&json!({"classes": "a b"})).0);
        assert_eq!(attrs.classes, ["a", "b"]);
    }

    #[test]
    fn attachment_urls_are_replaced_as_whole_urls() {
        let text =
            "![a](attachment:a.png) ![b](attachment:a.png.bak) <img src=\"attachment:a.png\">";
        assert_eq!(
            replace_url(text, "attachment:a.png", "asset:x").as_deref(),
            Some("![a](asset:x) ![b](attachment:a.png.bak) <img src=\"asset:x\">")
        );
        assert_eq!(
            replace_url("attachment:a.pngx", "attachment:a.png", "u"),
            None
        );
        assert_eq!(
            replace_url("attachment:a.png", "attachment:a.png", "u").as_deref(),
            Some("u")
        );
        assert_eq!(encode_uri("my plot (1).png"), "my%20plot%20(1).png");
        assert_eq!(encode_uri("ü.png"), "%C3%BC.png");
        assert_eq!(
            attachment_needles("a b.png"),
            ["attachment:a b.png", "attachment:a%20b.png"]
        );
        assert_eq!(attachment_needles("a.png"), ["attachment:a.png"]);
    }

    #[test]
    fn attachment_payloads_decode() {
        assert_eq!(decode_attachment("aGk=\n").unwrap(), b"hi");
        assert_eq!(decode_attachment("aGk").unwrap(), b"hi");
        assert_eq!(decode_attachment("<svg/>").unwrap(), b"<svg/>");
        assert!(decode_attachment("  ").is_err());
        assert!(decode_attachment("!!!").is_err());
        let html = "<img src=\"attachment:x.png\" /> <img src='attachment:y z.png'> \
                    <a HREF = \"attachment:w.svg\"> src=\"other:attachment:no\" \
                    <img src=\"attachment:s.png \"> <img src=attachment:u.png> \
                    <code>&lt;img src=attachment:c.png&gt; src=&quot;attachment:q.png&quot;</code>";
        let names: Vec<&str> = ATTACHMENT_URL
            .captures_iter(html)
            .filter_map(|caps| caps.get(1).or_else(|| caps.get(2)))
            .map(|name| {
                let at = name.start() - ATTACHMENT_SCHEME.len();
                assert_eq!(&html[at..name.start()], ATTACHMENT_SCHEME);
                name.as_str()
            })
            .collect();
        assert_eq!(names, ["x.png", "y z.png", "w.svg", "s.png "]);
        assert_eq!(percent_decode("my%20plot.png"), "my plot.png");
        assert_eq!(percent_decode("%C3%BC.png"), "ü.png");
        // T12's decoder (shared with missing-asset resolution): exactly two hex digits.
        assert_eq!(percent_decode("100%.png"), "100%.png");
        assert_eq!(percent_decode("%+F%4"), "%+F%4");
        assert_eq!(percent_decode("a%+Fb.png"), "a%+Fb.png");
        assert_eq!(percent_decode("%-1%2f"), "%-1/");
        assert_eq!(percent_decode("%FF"), "%FF", "not UTF-8: unchanged");
    }

    #[test]
    fn raw_html_heading_text() {
        let text = |html: &str| html_heading_text(html);
        assert_eq!(text("<h1>A</h1>").as_deref(), Some("A"));
        assert_eq!(
            text("<p>x</p><H2 id=\"t\">Hello <b>big</b><br>world &lt;3</H2><h1>no</h1>").as_deref(),
            Some("Hello big world <3")
        );
        assert_eq!(
            text("<h3>a<span>b</span><BR />c</h3>").as_deref(),
            Some("ab c")
        );
        assert_eq!(text("<header>x</header><hr><p>y</p>"), None);
        assert_eq!(text("<h2> <img src=x> </h2>"), None);
        assert_eq!(text("<h2>unclosed"), None);
        // Headings that are not shown do not count.
        assert_eq!(
            text("<!-- <h1>old</h1> --><h2>New</h2>").as_deref(),
            Some("New")
        );
        assert_eq!(text("<!-- <h1>Not</h1> --><p>x</p>"), None);
        assert_eq!(
            text("<script>document.write('<h1>js</h1>')</script><h3>Shown</h3>").as_deref(),
            Some("Shown")
        );
        assert_eq!(
            text("<TEXTAREA><h1>t</h1></textarea><style>h1{}</style><h4>S</h4>").as_deref(),
            Some("S")
        );
        assert_eq!(
            text("<template><h1>tpl</h1></Template ><h5>After</h5>").as_deref(),
            Some("After")
        );
        assert_eq!(text("<template><h1>tpl</h1>"), None);
        // Comments inside the heading and a closing tag of another level.
        assert_eq!(
            text("<h1 class='a>b'>One<!-- two --> three</h2>").as_deref(),
            Some("One three")
        );
        assert_eq!(text("<h1>a<!-- </h1> -->").as_deref(), Some("a"));
    }

    #[test]
    fn deck_metadata_predicates() {
        let meta = |yaml: &str| parse_front_matter(yaml).unwrap();
        assert!(sets_deck_metadata(&meta("title: x")));
        assert!(sets_deck_metadata(&meta("lang: de")));
        assert!(sets_deck_metadata(&meta("reveal: {controls: false}")));
        assert!(sets_deck_metadata(&meta("notebook:")));
        assert!(!sets_deck_metadata(&meta("Note: this matters")));
        assert!(!sets_deck_metadata(&meta("# a comment")));
        let front = |yaml| FrontMatter {
            cell: 2,
            yaml,
            rest: "",
            consumed: 3,
        };
        let warning = empty_front_matter(&front("# Intro to Pandas")).unwrap();
        assert_eq!(warning.code, codes::FRONT_MATTER_EMPTY);
        assert_eq!(
            warning.span,
            Some(SourceSpan {
                start_line: 1,
                end_line: 3,
                cell: Some(2)
            })
        );
        assert!(empty_front_matter(&front("")).is_none());
        assert!(empty_front_matter(&front("{}")).is_none());
        assert!(empty_front_matter(&front("# commented\ntitle: x")).is_none());
    }

    #[test]
    fn fragments_are_wrapped() {
        assert_eq!(wrap_fragment("<p>a</p>\n".into(), false), "<p>a</p>\n");
        assert_eq!(
            wrap_fragment("<p>a</p>\n".into(), true),
            "<div class=\"fragment\">\n<p>a</p>\n</div>\n"
        );
        assert_eq!(
            wrap_fragment("<video></video>".into(), true),
            "<div class=\"fragment\">\n<video></video>\n</div>\n"
        );
        assert_eq!(code_notes("a < b"), "<pre><code>a &lt; b</code></pre>\n");
        assert_eq!(code_notes(" \n"), "");
    }

    #[test]
    fn relocated_diagnostics_point_into_the_cell() {
        let at = SourceSpan {
            start_line: 1,
            end_line: 9,
            cell: Some(4),
        };
        let path = Path::new("/nb.ipynb");
        let inner = Diagnostic::warning("missing-asset", "m").with_span(SourceSpan {
            start_line: 2,
            end_line: 3,
            cell: None,
        });
        let moved = relocate(inner, Some(&at), 3, path);
        assert_eq!(
            moved.span,
            Some(SourceSpan {
                start_line: 5,
                end_line: 6,
                cell: Some(4)
            })
        );
        assert_eq!(moved.path.as_deref(), Some(path));
        let bare = relocate(Diagnostic::warning("x", "m"), Some(&at), 3, path);
        assert_eq!(bare.span, Some(at.clone()));
        let error = cell_error(
            Error::Parse {
                path: Default::default(),
                line: Some(2),
                message: "unbalanced".into(),
            },
            path,
            4,
            3,
        );
        assert_eq!(error.to_string(), "/nb.ipynb: cell 4, line 5: unbalanced");
    }
}
