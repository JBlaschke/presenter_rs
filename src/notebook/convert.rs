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
//!   key, `reveal` too. A first markdown cell that holds nothing but front matter is deck
//!   metadata only: it opens no slide and adds no content (like front matter in a Markdown
//!   file). A `---` block at the start of any *other* markdown cell is ordinary Markdown
//!   (what Jupyter shows), with a [`codes::FRONT_MATTER_IGNORED`] warning.
//! - **Removal** (§3): a cell tagged with one of `[notebook] remove_cell_tags`, a `skip`
//!   slide type and an unrecognized `cell_type` (already reported by `nbformat`) are ignored.
//! - **Structure** (§2): `slide` opens a top-level slide, `subslide` a vertical child of the
//!   current top-level slide (or a top-level slide when there is none yet). Anything before
//!   the first slide marker lands on slide 1, which the first such cell creates. The
//!   *current slide* (`cur`) is the last child of the last top-level slide, else that slide.
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
//!   `asset:<id>`. With `[notebook] split_markdown_cells = true`, `---` / `--` separators
//!   inside a cell open new slides exactly as in a Markdown deck ([`split_with_source`]);
//!   the part before the first separator continues `cur`.
//! - **Code cells**: [`Block::Cell`] with the outputs of [`render_outputs`] (T32); relative
//!   `src`/`poster` URLs in `text/html` outputs go through T12's raw-HTML asset rewriting.
//!   `hide_input` / `hide_output` come from the configured tag lists and
//!   `metadata.jupyter.source_hidden` / `outputs_hidden`; a cell with both is omitted.
//! - **Raw cells**: `raw_mimetype` (or `format`) `text/html` → the source as a
//!   [`Block::Html`], relative URLs rewritten like raw HTML in Markdown; any other raw cell
//!   is skipped with a [`codes::RAW_CELL_SKIPPED`] warning.
//! - **Per-cell `metadata.presenter`** (`{ "layout", "classes", "attrs" }`) goes through the
//!   slide-attribute normalization of Markdown separators (`SlideAttrs`, so shorthands such
//!   as `background` work) and applies to the slide the cell opens (a `slide`/`subslide`
//!   cell, or the cell that creates slide 1); on any other cell it is ignored with a
//!   [`codes::OVERRIDE_IGNORED`] warning. A lifted heading attribute wins over it, as a
//!   heading wins over a separator in Markdown.
//! - **Titles**: a slide's title is the first heading of its Markdown-rendered HTML blocks.
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
    RenderCtx, RenderMode, Rendered, has_math_markup, render_fragment, render_fragment_with_lift,
    render_nodes, rewrite_html_assets, rewrite_slide_assets,
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
    /// An `attachment:NAME` URL whose name is not among the cell's attachments.
    pub const MISSING_ATTACHMENT: &str = "missing-attachment";
    /// A `---` front matter block at the start of a markdown cell that is not the first
    /// markdown cell; it is rendered as Markdown.
    pub const FRONT_MATTER_IGNORED: &str = "front-matter-ignored";
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

/// `src`, `href` or `poster` values that still point at an attachment after rewriting.
static ATTACHMENT_URL: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r#"(?:src|href|poster)="attachment:([^"]*)""#).expect("attachment URL regex")
});

/// Converts a parsed notebook into a [`Deck`] (see the module docs).
///
/// `config` receives the deck metadata (`Config::merge_front_matter`), so afterwards it is
/// the config the notebook asked for. Assets (attachments, output images, files named by
/// raw HTML) are registered in `assets`; `Deck.assets` summarizes it. Warnings are appended
/// to `diags` with the notebook path and cell spans.
///
/// # Errors
/// [`Error::FrontMatter`] for invalid `metadata.presenter` or first-cell front matter (the
/// message names its origin), or front matter the config rejects; [`Error::Parse`] for a
/// separator with an unbalanced quote in a split markdown cell.
pub fn convert(
    nb: &Notebook,
    source: &SourceInfo,
    config: &mut Config,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
) -> Result<Deck> {
    let front = find_front_matter(nb);
    let meta = deck_meta(nb, front.as_ref())?;
    config.merge_front_matter(&meta)?;
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

/// Deck metadata from the three notebook layers of spec §5 (later wins).
fn deck_meta(nb: &Notebook, front: Option<&FrontMatter<'_>>) -> Result<DeckMeta> {
    let mut meta = DeckMeta {
        title: nb.title().map(str::to_owned),
        ..DeckMeta::default()
    };
    if let Some(presenter) = nb.presenter_meta() {
        let layer =
            meta_from_json(presenter).map_err(|err| in_context(err, "metadata.presenter"))?;
        overlay(&mut meta, layer);
    }
    if let Some(front) = front {
        let layer = parse_front_matter(front.yaml)
            .map_err(|err| in_context(err, &format!("cell {}", front.cell)))?;
        overlay(&mut meta, layer);
    }
    Ok(meta)
}

/// Prefixes a front matter error message with where the metadata came from.
fn in_context(err: Error, context: &str) -> Error {
    match err {
        Error::FrontMatter(message) => Error::FrontMatter(format!("{context}: {message}")),
        other => other,
    }
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
    features: Features,
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
            features: Features::default(),
        }
    }

    /// One iteration of the §2 walk.
    fn cell(&mut self, index: usize, cell: &'a Cell) -> Result<()> {
        let tags = cell.tags();
        let slide_type = cell.slide_type();
        if cell.cell_type == CellType::Unknown
            || slide_type == SlideType::Skip
            || has_any(&tags, &self.config.notebook.remove_cell_tags)
            || self.is_front_matter_only(index)
        {
            return Ok(());
        }
        self.opened_here = false;
        match slide_type {
            SlideType::Slide => self.open(false, cell.span(index)),
            SlideType::Subslide => self.open(true, cell.span(index)),
            _ => {}
        }
        if self.top.is_none() {
            // ensure_first_slide: content before the first slide marker is slide 1.
            self.open(false, cell.span(index));
        }
        self.apply_overrides(index, cell);
        if slide_type == SlideType::Notes || has_tag(&tags, NOTES_TAG) {
            return self.notes_cell(index, cell);
        }
        let fragment = slide_type == SlideType::Fragment || has_tag(&tags, FRAGMENT_TAG);
        match cell.cell_type {
            CellType::Markdown => self.markdown_cell(index, cell, fragment),
            CellType::Code => self.code_cell(index, cell, tags, fragment),
            CellType::Raw => self.raw_cell(index, cell, fragment),
            CellType::Unknown => Ok(()),
        }
    }

    /// The first markdown cell holds front matter and nothing else.
    fn is_front_matter_only(&self, index: usize) -> bool {
        self.front
            .is_some_and(|front| front.cell == index && front.rest.trim().is_empty())
    }

    /// Opens a top-level slide, or a vertical child of the current top-level slide.
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

    /// Per-cell `metadata.presenter` (spec §3).
    fn apply_overrides(&mut self, index: usize, cell: &Cell) {
        let Some(value) = cell.presenter_overrides() else {
            return;
        };
        let span = cell.span(index);
        if !self.opened_here {
            let message = "metadata.presenter only applies to the slide a cell opens \
                           (slide_type `slide` or `subslide`); this cell opens none, ignored";
            self.warn(codes::OVERRIDE_IGNORED, message, &span);
            return;
        }
        let (parsed, problems) = presenter_attrs(value);
        for problem in problems {
            self.warn(
                codes::OVERRIDE_INVALID,
                format!("metadata.presenter: {problem}"),
                &span,
            );
        }
        self.apply_attrs(SlideAttrs::from_parsed(parsed), &span);
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
        let cur = self.cur();
        if !rendered.html.is_empty() {
            if cur.title.is_none() {
                cur.title = rendered.title;
            }
            cur.blocks
                .push(Block::html(wrap_fragment(rendered.html, fragment)));
        }
        if let Some(notes) = rendered.notes {
            append_notes(cur, &notes);
        }
    }

    /// The Markdown source of a cell (front matter removed from the first markdown cell)
    /// with attachments rewritten, and the number of lines removed before it.
    fn markdown_text(&mut self, index: usize, cell: &'a Cell) -> (Cow<'a, str>, usize) {
        let front = self.front.filter(|front| front.cell == index);
        let (text, offset) = match front {
            Some(front) => (front.rest, front.consumed),
            None => {
                self.check_ignored_front_matter(index, cell);
                (cell.source.as_str(), 0)
            }
        };
        (self.with_attachments(index, cell, text), offset)
    }

    /// Warns about a `---` block that would be front matter in the first markdown cell.
    fn check_ignored_front_matter(&mut self, index: usize, cell: &Cell) {
        let Some((yaml, _, consumed)) = split_front_matter(cell.source.as_str()) else {
            return;
        };
        if parse_front_matter(yaml).is_ok_and(|meta| meta != DeckMeta::default()) {
            let message = "front matter is only read from the first markdown cell; this \
                           `---` block is rendered as Markdown";
            let span = SourceSpan {
                start_line: 1,
                end_line: consumed,
                cell: Some(index),
            };
            self.warn(codes::FRONT_MATTER_IGNORED, message, &span);
        }
    }

    fn markdown_cell(&mut self, index: usize, cell: &'a Cell, fragment: bool) -> Result<()> {
        let (text, offset) = self.markdown_text(index, cell);
        if self.config.notebook.split_markdown_cells {
            return self.split_cell(index, cell, &text, offset, fragment);
        }
        let lift = self.cur_is_empty();
        let span = cell.span(index);
        let (rendered, lifted) = self.render(Some(&span), offset, |ctx, opts| {
            render_fragment_with_lift(&text, opts, ctx, lift)
        })?;
        if let Some(attrs) = lifted {
            self.apply_attrs(SlideAttrs::from_parsed(attrs), &span);
        }
        self.check_attachments(&rendered.html, cell, &span);
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
    /// opens slide 1 itself in a Markdown deck); its attributes go to that slide.
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
        let rendered = self.render(Some(span), part.offset, |ctx, opts| {
            render_nodes(arena, nodes, opts, ctx)
        })?;
        self.check_attachments(&rendered.html, part.cell, span);
        self.push_rendered(rendered, part.fragment);
        Ok(())
    }

    fn code_cell(
        &mut self,
        index: usize,
        cell: &Cell,
        tags: Vec<String>,
        fragment: bool,
    ) -> Result<()> {
        let config = &self.config.notebook;
        let hide_input = cell.is_hidden_source() || has_any(&tags, &config.hide_input_tags);
        let hide_output = cell.is_hidden_outputs() || has_any(&tags, &config.hide_output_tags);
        if hide_input && hide_output {
            return Ok(());
        }
        let outputs = self.outputs(index, cell)?;
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
        self.cur().blocks.push(block);
        Ok(())
    }

    /// The cell's outputs (T32), with `text/html` URLs rewritten to assets.
    fn outputs(&mut self, index: usize, cell: &Cell) -> Result<Vec<CellOutput>> {
        let span = cell.span(index);
        let mut local = Diagnostics::new();
        // `text/markdown` outputs render into their own registry (see `render_outputs`).
        let mut md_assets = AssetRegistry::new();
        let mut md_diags = Diagnostics::new();
        let (opts, base_dir) = (&self.opts, self.source.base_dir.as_path());
        let mut md = |text: &str| -> Result<String> {
            let mut ctx = RenderCtx {
                assets: &mut md_assets,
                base_dir,
                mode: RenderMode::Notebook,
                diagnostics: &mut md_diags,
                missing_asset_is_error: false,
            };
            render_fragment(text, opts, &mut ctx).map(|rendered| rendered.html)
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
        Ok(outputs)
    }

    fn raw_cell(&mut self, index: usize, cell: &'a Cell, fragment: bool) -> Result<()> {
        if let Some(html) = self.raw_html(index, cell)? {
            self.cur()
                .blocks
                .push(Block::html(wrap_fragment(html, fragment)));
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
        let mimetype = cell.raw_mimetype();
        if !mimetype.is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html")) {
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
        self.check_attachments(&html, cell, &span);
        self.features.math |= has_math_markup(&html);
        Ok(Some(html))
    }

    /// A `notes` cell (slide type or tag): its rendering is appended to `cur.notes`.
    fn notes_cell(&mut self, index: usize, cell: &'a Cell) -> Result<()> {
        let html = match cell.cell_type {
            CellType::Markdown => {
                let (text, offset) = self.markdown_text(index, cell);
                let span = cell.span(index);
                let rendered = self.render(Some(&span), offset, |ctx, opts| {
                    render_fragment(&text, opts, ctx)
                })?;
                self.check_attachments(&rendered.html, cell, &span);
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
    /// `asset:<id>` of the registered attachment.
    fn with_attachments(&mut self, index: usize, cell: &Cell, text: &'a str) -> Cow<'a, str> {
        let mut text = Cow::Borrowed(text);
        for (name, bundle) in &cell.attachments {
            let needles = attachment_needles(name);
            if !needles
                .iter()
                .any(|needle| url_starts(&text, needle).next().is_some())
            {
                continue;
            }
            let Some(id) = self.register_attachment(index, cell, name, bundle) else {
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
    /// one) by its bytes; `None` with a warning when its payload cannot be decoded.
    fn register_attachment(
        &mut self,
        index: usize,
        cell: &Cell,
        name: &str,
        bundle: &BTreeMap<String, MultilineString>,
    ) -> Option<String> {
        let priority = &self.config.notebook.mime_priority;
        let (mime, payload) = priority
            .iter()
            .find_map(|mime| bundle.get_key_value(mime))
            .or_else(|| bundle.iter().next())?;
        match decode_attachment(payload.as_str()) {
            Ok(bytes) => Some(self.assets.register_bytes(bytes, mime, name)),
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

    /// Warns about `attachment:` URLs left in rendered HTML whose name the cell lacks.
    fn check_attachments(&mut self, html: &str, cell: &Cell, span: &SourceSpan) {
        let missing: Vec<String> = ATTACHMENT_URL
            .captures_iter(html)
            .map(|caps| caps[1].to_owned())
            .filter(|name| !cell.attachments.contains_key(name))
            .collect();
        for name in missing {
            let message = format!("`attachment:{name}`: the cell has no attachment `{name}`");
            self.warn(codes::MISSING_ATTACHMENT, message, span);
        }
    }

    /// Closes the walk: one empty slide for a notebook without content, slide background
    /// assets, `empty-slide` warnings and the derived features.
    fn finish(mut self) -> Result<(Vec<Slide>, Features)> {
        if let Some(top) = self.top.take() {
            self.done.push(top);
        }
        if self.done.is_empty() {
            self.done.push(Slide::new());
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
        assert!(ATTACHMENT_URL.is_match("<img src=\"attachment:x.png\" />"));
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
