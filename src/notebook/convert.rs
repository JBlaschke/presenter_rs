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
//!   in `<div class="fragment">` for fragments. `:::notes` go to `cur.notes`.
//!   With `[notebook] split_markdown_cells = true`, `---` / `--` separators inside a cell
//!   open new slides exactly as in a Markdown deck ([`split_with_source`]); the part before
//!   the first separator continues `cur`.
//! - **Attachments** of markdown and raw cells are resolved after rendering, in the cell's
//!   HTML (body and notes) and in the slide attributes the cell sets: an `attachment:NAME`
//!   URL that is the value of a start tag's attribute (quoted or not; `src`, `href`,
//!   `poster`, reveal.js' `data-preview-image`/`-video`/`-link`, any `data-*`, … but not
//!   the text attributes `alt`, `title`, `aria-*` and the like), a `srcset` candidate, a CSS
//!   `url(…)` in a `style` attribute or `<style>` element, a URL written inside a
//!   `<script>` element, an `on…` attribute or another attribute value
//!   (`'attachment:a.png'`, `srcdoc="<img src='attachment:a.png'>"`,
//!   `content="0; url=attachment:a.png"`), or a `data-background`/`-image`/`-video`/`-iframe`
//!   slide attribute is registered as an inline asset and becomes `asset:<id>`. Inside a
//!   script or another value, a URL starts after a quote, `(` or `=` (also written as a
//!   character reference; not in another URL's query or fragment, `?q=attachment:x`,
//!   `?ids=1,2&u=attachment:x`), and one that names an attachment of the cell also at the
//!   start or after white space; script code that only uses the word (`{attachment:true}`,
//!   `// see attachment:notes`, `` `attachment:${name}` ``, `'attachment:' + name`) is left
//!   alone. An attribute whose value is one URL (`src`, `href`, `data-preview-image`, …) is
//!   only resolved as a whole: `href="https://example.org/?q=attachment:x"` stays as
//!   written, without a warning; a `javascript:` or `data:` URL there is a script or a
//!   document and is scanned like one (`href="javascript:show('attachment:a.png')"`). HTML
//!   shown in code is text, not a tag, and stays as written. The name is matched as written,
//!   then with HTML entities and percent escapes decoded (`attachment:a%2Bb.png` names
//!   `a+b.png`), then without a `?query` or `#fragment`, which is kept after the
//!   placeholder. A URL whose attachment was skipped stays as written
//!   (`notebook-attachment-skipped`, reported once per name); any other unresolved one warns
//!   [`codes::MISSING_ATTACHMENT`].
//! - **Code cells**: [`Block::Cell`] with the outputs of [`render_cell_outputs`] (T32), which
//!   also resolves the local files `text/html` and `text/markdown` outputs name, but only
//!   inside the notebook's directory (outputs are written by executed code; audit F08).
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
//! - **Post-processing** (audit F17): the Markdown frontend's post-processing
//!   (`markdown::postprocess`) makes `<!-- .element: -->` comments, `[slides]
//!   incremental_lists` and the slide classes `.incremental` / `.no-incremental` work as on a
//!   Markdown slide. `.element:` comments are applied to each HTML block and each piece of
//!   notes when its cell adds it, so their warnings name that cell; incremental lists when
//!   the slide is finished and its classes are known.
//! - **One slide** (audit F28): when no cell has the slide type `slide` or `subslide` and the
//!   whole notebook, with at least [`SINGLE_SLIDE_MIN_CELLS`] shown cells, became one slide,
//!   a [`codes::SINGLE_SLIDE`] warning says how to set slide types.
//!
//! Every diagnostic carries the notebook path and a span with `cell: Some(index)`; lines are
//! lines of the cell source (front matter included). The warnings about a code cell's
//! outputs have the span of the whole cell and name the output in their message
//! (`output 1: …`; audit F38). The notebook-level [`codes::SINGLE_SLIDE`] has no span.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use comrak::nodes::AstNode;
use comrak::{Arena, Options, parse_document};
use regex::Regex;
use serde_json::Value;

use crate::assets::emit::is_text_attribute;
use crate::assets::registry::AssetRegistry;
use crate::config::Config;
use crate::error::{Diagnostic, Diagnostics, Error, Result};
use crate::ir::{
    Block, CellOutput, Deck, DeckMeta, Features, NotebookCell, Slide, SourceInfo, SourceSpan,
};
use crate::markdown::attrs::{ParsedAttrs, SlideAttrs};
use crate::markdown::frontmatter::{meta_from_json, parse_front_matter, split_front_matter};
use crate::markdown::render::{
    RenderCtx, RenderMode, Rendered, has_math_markup, html_attr_values, html_start_tags,
    percent_decode, render_fragment, render_fragment_with_lift, render_nodes, rewrite_html_assets,
    rewrite_slide_assets,
};
use crate::markdown::split::{RawSlide, SplitMode, split_with_source};
use crate::markdown::{comrak_options, postprocess};
use crate::notebook::nbformat::{self, Cell, CellType, MultilineString, Notebook, SlideType};
use crate::notebook::outputs::{OutputFiles, render_cell_outputs};

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
    /// No cell has the slide type `slide` or `subslide`, so the whole notebook (at least
    /// [`super::SINGLE_SLIDE_MIN_CELLS`] shown cells) is one slide (audit F28).
    pub const SINGLE_SLIDE: &str = "notebook-single-slide";
}

/// The fewest shown cells (notes cells not counted) for which one slide without slide types
/// is worth a [`codes::SINGLE_SLIDE`] warning: a title and a cell or two fit on a slide.
pub const SINGLE_SLIDE_MIN_CELLS: usize = 3;

/// The tag that makes a cell a fragment (spec §3; not configurable).
const FRAGMENT_TAG: &str = "fragment";
/// The tag that makes a cell speaker notes (spec §3; not configurable).
const NOTES_TAG: &str = "notes";
/// The config tables front matter may override (config.md §3); merged key by key.
const CONFIG_TABLES: [&str; 4] = ["slides", "markdown", "math", "notebook"];
/// The config tables the conversion needs to go on when other deck metadata is rejected
/// (`markdown.*` options, `notebook.*` tag lists, …; see `convert_deferring_config`). It
/// also reads `slides.incremental_lists`, which a rejected deck never gets to use.
const CONVERSION_TABLES: [&str; 2] = ["markdown", "notebook"];
/// Splitter diagnostics kept for split markdown cells (the others are recomputed per deck).
const SEPARATOR_NOT_RECOGNIZED: &str = "separator-not-recognized";
/// The splitter's report of a leading `--`, which in a cell opens a vertical slide.
const ORPHAN_VERTICAL: &str = "orphan-vertical";

/// The scheme of a notebook attachment URL (`attachment:NAME`).
const ATTACHMENT_SCHEME: &str = "attachment:";

/// The HTML attributes whose value is one URL, as are `data-background` and every
/// `data-background-*` attribute ([`is_url_attr`]): only the whole value (white space
/// trimmed) can be an `attachment:` URL. An `attachment:` elsewhere in it is part of another
/// URL (`href="https://example.org/?q=attachment:x"`) and left as written.
const URL_ATTRS: [&str; 13] = [
    "src",
    "href",
    "poster",
    "data-src",
    "data",
    "xlink:href",
    "action",
    "formaction",
    "cite",
    "background",
    "data-preview-image",
    "data-preview-video",
    "data-preview-link",
];

/// The slide attributes whose `attachment:` URLs are resolved (`data-background-video` is a
/// comma-separated list), as [`rewrite_slide_assets`] resolves files.
const SLIDE_URL_ATTRS: [&str; 4] = [
    "data-background",
    "data-background-image",
    "data-background-video",
    "data-background-iframe",
];

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
    convert_with(nb, source, config, assets, diags, false).map(|(deck, _)| deck)
}

/// [`convert`] for a caller that merges the deck metadata into its own config afterwards
/// (the pipeline, ir.md §4): deck metadata the conversion does not read (`reveal` options,
/// the `slides` and `math` tables) that the config rejects does not stop it. The error
/// [`convert`] would return for it, naming its layer, comes back with the deck, so that the
/// caller can report it with the diagnostics; `config` then receives the deck metadata
/// without those values. A rejected `markdown` or `notebook` table still fails.
pub(crate) fn convert_deferring_config(
    nb: &Notebook,
    source: &SourceInfo,
    config: &mut Config,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
) -> Result<(Deck, Option<Error>)> {
    convert_with(nb, source, config, assets, diags, true)
}

/// [`convert`]; with `defer`, [`convert_deferring_config`].
fn convert_with(
    nb: &Notebook,
    source: &SourceInfo,
    config: &mut Config,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
    defer: bool,
) -> Result<(Deck, Option<Error>)> {
    let front = find_front_matter(nb);
    let (meta, layers) = deck_meta(nb, front.as_ref())?;
    let deferred = match merge_config(config, &meta, &layers) {
        Ok(()) => None,
        Err(error) if defer => {
            let layers: Vec<Layer> = layers
                .iter()
                .map(|(context, layer)| (context.clone(), conversion_meta(layer)))
                .collect();
            merge_config(config, &conversion_meta(&meta), &layers)?;
            Some(error)
        }
        Err(error) => return Err(error),
    };
    if let Some(warning) = front.as_ref().and_then(empty_front_matter) {
        diags.push(warning.with_path(&source.path));
    }
    let mut converter = Converter::new(nb, source, config, front, assets, diags);
    for (index, cell) in nb.cells.iter().enumerate() {
        converter.cell(index, cell)?;
    }
    let (slides, features) = converter.finish()?;
    let deck = Deck {
        meta,
        slides,
        features,
        source: source.clone(),
        assets: assets.infos(&source.base_dir),
        includes: Vec::new(),
    };
    Ok((deck, deferred))
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

/// `meta` with only what the conversion reads: no `reveal` options and, of the config
/// tables, only [`CONVERSION_TABLES`] (other vars stay; the config ignores them).
fn conversion_meta(meta: &DeckMeta) -> DeckMeta {
    let mut meta = meta.clone();
    meta.reveal.clear();
    meta.vars.retain(|key, _| {
        !CONFIG_TABLES.contains(&key.as_str()) || CONVERSION_TABLES.contains(&key.as_str())
    });
    meta
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
        file: None,
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
    /// Cells that added content to a slide (not removed, contentless or notes).
    shown: usize,
    /// A cell that is not removed has the slide type `slide` or `subslide`.
    structured: bool,
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
            shown: 0,
            structured: false,
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
        self.structured |= matches!(slide_type, SlideType::Slide | SlideType::Subslide);
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
        self.shown += 1;
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

    /// Applies slide attributes to the current slide, reporting unknown and invalid keys and
    /// resolving attachment URLs against the attachments of the cell that set them
    /// (`span.cell`).
    fn apply_attrs(&mut self, mut attrs: SlideAttrs, span: &SourceSpan) {
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
        self.resolve_slide_attachments(&mut attrs.attrs, span);
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

    /// Adds a rendered Markdown fragment of the cell (part) at `span` to the current slide.
    fn push_rendered(
        &mut self,
        rendered: Rendered,
        fragment: bool,
        span: &SourceSpan,
    ) -> Result<()> {
        self.features.merge(rendered.features);
        if !rendered.html.is_empty() {
            self.push_html(rendered.html, rendered.title, fragment, span)?;
        }
        if let Some(notes) = rendered.notes {
            self.add_notes(&notes, span)?;
        }
        Ok(())
    }

    /// Adds an HTML block of the cell (part) at `span` to the current slide, its
    /// `<!-- .element: -->` comments applied ([`Self::element_comments`]); `title` (the
    /// heading of this block) is the slide's title when this is the slide's first HTML
    /// block (§2).
    fn push_html(
        &mut self,
        html: String,
        title: Option<String>,
        fragment: bool,
        span: &SourceSpan,
    ) -> Result<()> {
        let html = self.element_comments(&wrap_fragment(html, fragment), span)?;
        let cur = self.cur();
        if !cur
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Html { .. }))
        {
            cur.title = title;
        }
        cur.blocks.push(Block::html(html));
        Ok(())
    }

    /// Appends speaker notes of the cell (part) at `span` to the current slide, their
    /// `<!-- .element: -->` comments applied ([`Self::element_comments`]).
    fn add_notes(&mut self, html: &str, span: &SourceSpan) -> Result<()> {
        let html = self.element_comments(html, span)?;
        append_notes(self.cur(), &html);
        Ok(())
    }

    /// `html` (from the cell, or the part of a split cell, at `span`) with its
    /// `<!-- .element: -->` comments applied as on a Markdown slide (audit F17); their
    /// warnings are located at `span`, the cell that has the comment.
    fn element_comments(&mut self, html: &str, span: &SourceSpan) -> Result<String> {
        let mut local = Diagnostics::new();
        let html = postprocess::element_attributes(html, Some(span), &mut local)?;
        let path = &self.source.path;
        self.diags
            .extend(local.into_iter().map(|d| d.with_path(path)));
        Ok(html)
    }

    /// The Markdown source of a cell (front matter removed) and the number of lines removed
    /// before it.
    fn markdown_text(&mut self, index: usize, cell: &'a Cell) -> (&'a str, usize) {
        let front = self.front.filter(|front| front.cell == index);
        match front {
            Some(front) => (front.rest, front.consumed),
            None => self.without_ignored_front_matter(index, cell),
        }
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
            file: None,
        };
        self.warn(codes::FRONT_MATTER_IGNORED, message, &span);
        (rest, consumed)
    }

    fn markdown_cell(&mut self, index: usize, cell: &'a Cell, fragment: bool) -> Result<()> {
        let (text, offset) = self.markdown_text(index, cell);
        if self.config.notebook.split_markdown_cells {
            self.split_cell(index, cell, text, offset, fragment)?;
            if let Some((span, _)) = self.deferred.take() {
                self.override_ignored(&span, true);
            }
            return Ok(());
        }
        let lift = self.cur_is_empty();
        let span = cell.span(index);
        let (mut rendered, lifted) = self.render(Some(&span), offset, |ctx, opts| {
            render_fragment_with_lift(text, opts, ctx, lift)
        })?;
        if let Some(attrs) = lifted {
            self.apply_attrs(SlideAttrs::from_parsed(attrs), &span);
        }
        self.resolve_rendered(index, cell, &mut rendered, &span);
        self.push_rendered(rendered, fragment, &span)
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
        // The splitter lifts a leading heading's attributes (also after comments and
        // `:::notes`); keep them on the heading unless the current slide is still empty (§2).
        let heading = crate::markdown::split::lifted_heading(root);
        let kept = match heading {
            Some(heading) if !self.cur_is_empty() => heading.data_mut().attrs.take(),
            _ => None,
        };
        let raw = split_with_source(root, text, SplitMode::Rules)
            .map_err(|err| cell_error(err, &self.source.path, index, offset));
        if let (Some(heading), Some(kept)) = (heading, kept) {
            heading.data_mut().attrs = Some(kept);
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
            file: None,
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
        self.push_rendered(rendered, part.fragment, span)
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
        self.cur().blocks.push(block);
        if let Some(notes) = notes.filter(|_| !hide_output) {
            self.add_notes(&notes, &cell.span(index))?;
        }
        Ok(())
    }

    /// The cell's outputs (T32) with their local files resolved inside the notebook's
    /// directory ([`render_cell_outputs`]), and the `:::notes` of its `text/markdown`
    /// outputs. Warnings get the notebook path and the cell's span; their messages name the
    /// output ([`output_diagnostic`]).
    fn outputs(&mut self, index: usize, cell: &Cell) -> Result<(Vec<CellOutput>, Option<String>)> {
        let files = OutputFiles {
            base_dir: &self.source.base_dir,
            markdown: &self.opts,
        };
        let mut local = Diagnostics::new();
        let rendered = render_cell_outputs(
            &cell.outputs,
            &self.config.notebook,
            &files,
            self.assets,
            &mut local,
        )?;
        // `notes` follows the slides' notes (`finish_slide`), which hidden outputs skip.
        self.features.merge(rendered.features);
        let (path, span) = (&self.source.path, cell.span(index));
        self.diags
            .extend(local.into_iter().map(|d| output_diagnostic(d, path, &span)));
        Ok((rendered.outputs, rendered.notes))
    }

    fn raw_cell(&mut self, index: usize, cell: &'a Cell, fragment: bool) -> Result<()> {
        if let Some(html) = self.raw_html(index, cell)? {
            let title = html_heading_text(&html);
            self.push_html(html, title, fragment, &cell.span(index))?;
        }
        Ok(())
    }

    /// The HTML of a raw cell with `raw_mimetype` `text/html` (relative URLs and attachments
    /// resolved); `None` (with a warning unless the cell is blank) otherwise.
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
        let mut html = source.to_owned();
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
                    render_fragment(text, opts, ctx)
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
            self.add_notes(&html, &cell.span(index))?;
        }
        Ok(())
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

    /// Resolves the `attachment:` URLs of `html` (a markdown cell's rendered body or notes,
    /// or a raw HTML cell) found by [`attachment_url_candidates`], in document order, with
    /// [`Self::attachment_url`].
    fn resolve_attachments(
        &mut self,
        index: usize,
        cell: &Cell,
        html: &mut String,
        span: &SourceSpan,
    ) {
        // Longest first, for the names written in scripts and other values (`embedded_urls`).
        let mut names: Vec<&str> = cell.attachments.keys().map(String::as_str).collect();
        names.retain(|name| !name.is_empty());
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for url in attachment_url_candidates(html, &names) {
            if let Some(resolved) = self.attachment_url(index, cell, &html[url.clone()], span) {
                edits.push((url, resolved));
            }
        }
        for (range, text) in edits.into_iter().rev() {
            html.replace_range(range, &text);
        }
    }

    /// Resolves the `attachment:` URLs of the slide attributes a cell sets
    /// ([`SLIDE_URL_ATTRS`]; `span.cell` is the cell) like [`Self::resolve_attachments`].
    fn resolve_slide_attachments(
        &mut self,
        attrs: &mut BTreeMap<String, String>,
        span: &SourceSpan,
    ) {
        let nb = self.nb;
        let Some((index, cell)) = span.cell.and_then(|i| Some((i, nb.cells.get(i)?))) else {
            return;
        };
        for key in SLIDE_URL_ATTRS {
            let Some(value) = attrs.get(key).cloned() else {
                continue;
            };
            let parts: Vec<&str> = if key == "data-background-video" {
                value.split(',').collect()
            } else {
                vec![value.as_str()]
            };
            let mut changed = false;
            let mut resolved = Vec::with_capacity(parts.len());
            for part in parts {
                let url = self.attachment_url(index, cell, part.trim(), span);
                changed |= url.is_some();
                resolved.push(url.unwrap_or_else(|| part.to_owned()));
            }
            if changed {
                attrs.insert(key.to_owned(), resolved.join(","));
            }
        }
    }

    /// The `asset:<id>` replacing one URL (as written in HTML) when it names an attachment
    /// of `cell`: as written, once HTML entities and percent escapes are decoded
    /// (`attachment:a%2Bb.png` for `a+b.png`, any hex case, `a&amp;b.png`), or without the
    /// `?query` / `#fragment` it ends with, which is kept after the placeholder. `None`
    /// for a URL that is not an `attachment:` URL, one whose attachment was skipped (already
    /// reported) and one that names no attachment of `cell`, which warns
    /// [`codes::MISSING_ATTACHMENT`] (naming it decoded, as written in the source).
    fn attachment_url(
        &mut self,
        index: usize,
        cell: &Cell,
        url: &str,
        span: &SourceSpan,
    ) -> Option<String> {
        if !is_attachment_url(url) {
            return None;
        }
        let written = &url[ATTACHMENT_SCHEME.len()..];
        let cut = written.find(['?', '#']).unwrap_or(written.len());
        let (path, suffix) = written.split_at(cut);
        let found = attachment_name(cell, written)
            .map(|name| (name, ""))
            .or_else(|| Some((attachment_name(cell, path)?, suffix)));
        let Some((name, suffix)) = found else {
            let decoded = |text: &str| percent_decode(&html_escape::decode_html_entities(text));
            let (url, name) = (decoded(written), decoded(path));
            let message = format!("`attachment:{url}`: the cell has no attachment `{name}`");
            self.warn(codes::MISSING_ATTACHMENT, message, span);
            return None;
        };
        let id = self.attachment_id(index, cell, &name)?;
        Some(format!("{}{suffix}", AssetRegistry::placeholder(&id)))
    }

    /// Closes the walk: one empty slide for a notebook without content (it receives the
    /// overrides of contentless cells), the [`codes::SINGLE_SLIDE`] hint, slide background
    /// assets, incremental lists and `.element:` comments ([`Self::finish_slide`]),
    /// `empty-slide` warnings and the derived features.
    fn finish(mut self) -> Result<(Vec<Slide>, Features)> {
        if self.top.is_none() {
            self.top = Some(Slide::new());
            self.apply_pending();
        }
        if let Some(top) = self.top.take() {
            self.done.push(top);
        }
        self.single_slide_hint();
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

    /// The [`codes::SINGLE_SLIDE`] warning (notebook-level, no span) when the whole notebook
    /// became one slide only because no cell has a slide type: one slide without vertical
    /// slides, at least [`SINGLE_SLIDE_MIN_CELLS`] shown cells and no `slide` or `subslide`
    /// cell. (With `split_markdown_cells`, separators that open slides make it more than one.)
    fn single_slide_hint(&mut self) {
        let one = matches!(self.done.as_slice(), [only] if only.children.is_empty());
        if !one || self.structured || self.shown < SINGLE_SLIDE_MIN_CELLS {
            return;
        }
        let message = format!(
            "all {} cells are on one slide because no cell has a slide type (as in `jupyter \
             nbconvert --to slides`); set the Slide Type of the cells that start a slide to \
             Slide or Sub-Slide (JupyterLab: the property inspector, the gear icon in the right \
             sidebar; Notebook 7: View > Slideshow), or set `[notebook] split_markdown_cells = \
             true` and separate slides with `---` in markdown cells",
            self.shown
        );
        self.diags
            .push(Diagnostic::warning(codes::SINGLE_SLIDE, message).with_path(&self.source.path));
    }

    /// Finishes one slide: its background files (`rewrite_slide_assets`), incremental lists
    /// in its HTML blocks when `[slides] incremental_lists` or the slide's `.incremental` /
    /// `.no-incremental` class say so, exactly as for a Markdown slide (audit F17; the
    /// `.element:` comments were applied when each block was added, located at its cell)
    /// and the `empty-slide` warning.
    fn finish_slide(&mut self, slide: &mut Slide, label: &str) -> Result<()> {
        let span = slide.span.clone();
        self.render(span.as_ref(), 0, |ctx, _| {
            rewrite_slide_assets(&mut slide.attrs, span.as_ref(), ctx)
        })?;
        if postprocess::incremental_enabled(self.config.slides.incremental_lists, &slide.classes) {
            for block in &mut slide.blocks {
                if let Block::Html { html } = block {
                    *html = postprocess::incremental_lists(html)?;
                }
            }
        }
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

/// A warning of [`render_cell_outputs`] with the notebook `path` and the `span` of the
/// whole cell, the span of every cell-level warning ([`Cell::span`]), so that the cell is
/// known wherever the warning goes (check's JSON `cell`, the `-->` line of a build that fails
/// on a missing file). Its message names the output (`output <i>: …`). The span replaces
/// any line the Markdown renderer gave (a line of a `text/markdown` output's text, not of
/// the cell's source); for a missing file no source line is quoted unless it mentions the
/// file ([`crate::error::Error::excerpt`]; audit F38).
fn output_diagnostic(diagnostic: Diagnostic, path: &Path, span: &SourceSpan) -> Diagnostic {
    diagnostic.with_path(path).with_span(span.clone())
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

/// The name of `cell`'s attachment that `written` (an `attachment:` URL without its scheme,
/// as written in HTML) names: as written, with HTML entities decoded, or with percent
/// escapes decoded too.
fn attachment_name(cell: &Cell, written: &str) -> Option<String> {
    if cell.attachments.contains_key(written) {
        return Some(written.to_owned());
    }
    let unescaped = html_escape::decode_html_entities(written);
    if cell.attachments.contains_key(unescaped.as_ref()) {
        return Some(unescaped.into_owned());
    }
    let decoded = percent_decode(&unescaped);
    cell.attachments.contains_key(&decoded).then_some(decoded)
}

/// The byte ranges of the URLs in `html` that may be `attachment:` URLs, in document order:
///
/// - the value (white space trimmed) of every attribute that holds one URL, when it is an
///   `attachment:` URL: any attribute but `srcset`, `style`, the `on…` event handlers and
///   the text attributes ([`is_text_attribute`]: `alt`, `title`, `aria-*`, …), so `src`,
///   `href`, `poster`, reveal.js' `data-preview-image` / `-video` / `-link`, `data-src` and
///   any other `data-*` attribute count ([`Converter::attachment_url`] then resolves or reports it);
/// - the `attachment:` URLs written inside another value of such an attribute (an
///   `<iframe srcdoc>`, a `<meta>` refresh's `content="0; url=attachment:a.png"`, JSON in a
///   `data-*` attribute; [`embedded_urls`]), unless the attribute is one whose value is a
///   URL ([`is_url_attr`]: `src`, `href`, …), where an `attachment:` inside the value is
///   part of that URL (`href="https://example.org/?q=attachment:x"`), but for a
///   `javascript:` or `data:` URL ([`is_code_url`]), whose content is a script or a
///   document;
/// - each `srcset` candidate ([`srcset_urls`]), and each CSS `url(…)` ([`css_urls`]) of a
///   `style` attribute or a `<style>` element;
/// - the `attachment:` URLs written inside scripts, `<script>` elements and `on…`
///   attributes ([`embedded_urls`]).
///
/// `names` are the cell's attachment names, longest first. Attributes and elements are
/// those of the start tags T12's HTML tokenizer finds ([`html_attr_values`]): text,
/// comments and HTML shown in code (`&lt;img src=…`, quotes included) are not tags, so their
/// URLs are not candidates.
fn attachment_url_candidates(html: &str, names: &[&str]) -> Vec<Range<usize>> {
    let shift = |base: usize| move |url: Range<usize>| base + url.start..base + url.end;
    let mut urls = Vec::new();
    for (name, value) in html_attr_values(html) {
        let text = &html[value.clone()];
        let found = match name.as_str() {
            "srcset" => srcset_urls(text),
            "style" => css_urls(text),
            // Text for people (`alt="attachment:x"` stays as written), as for the emitter.
            name if is_text_attribute(name) => continue,
            name if name.starts_with("on") => embedded_urls(text, names, Written::Attribute),
            name => {
                let url = trimmed(text);
                let whole = &text[url.clone()];
                if is_attachment_url(whole) {
                    vec![url]
                } else if is_url_attr(name) && !is_code_url(whole) {
                    Vec::new()
                } else {
                    embedded_urls(text, names, Written::Attribute)
                }
            }
        };
        urls.extend(found.into_iter().map(shift(value.start)));
    }
    for content in element_contents(html, "style") {
        let found = css_urls(&html[content.clone()]);
        urls.extend(found.into_iter().map(shift(content.start)));
    }
    for content in element_contents(html, "script") {
        let found = embedded_urls(&html[content.clone()], names, Written::Script);
        urls.extend(found.into_iter().map(shift(content.start)));
    }
    urls.sort_by_key(|url| url.start);
    urls
}

/// Whether the attribute `name` (lowercase) holds one URL: one of [`URL_ATTRS`],
/// `data-background` or a `data-background-*` attribute.
fn is_url_attr(name: &str) -> bool {
    URL_ATTRS.contains(&name) || name.starts_with("data-background")
}

/// Whether `url` is a `javascript:` or `data:` URL (any ASCII case): its content is a script
/// or a document (`javascript:show('attachment:a.png')`, `data:text/html,<img src=…>`), not
/// the rest of one URL, so [`embedded_urls`] scans it.
fn is_code_url(url: &str) -> bool {
    ["javascript:", "data:"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// Whether `url` starts with the `attachment:` scheme (any ASCII case).
fn is_attachment_url(url: &str) -> bool {
    url.get(..ATTACHMENT_SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(ATTACHMENT_SCHEME))
}

/// The byte ranges of the contents of the `name` elements of `html` (`style`, `script`:
/// raw-text elements), each from the end of the start tag to the `</name` that closes it,
/// or the end of `html`.
fn element_contents(html: &str, name: &str) -> Vec<Range<usize>> {
    // ASCII lowercasing keeps every byte offset.
    let lower = html.to_ascii_lowercase();
    let close = format!("</{name}");
    html_start_tags(html)
        .into_iter()
        .filter(|(tag, _)| tag == name)
        .map(|(_, end)| end..lower[end..].find(&close).map_or(html.len(), |at| end + at))
        .collect()
}

/// How a text scanned by [`embedded_urls`] is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    /// A `<script>` element's content: its characters are the script's.
    Script,
    /// An attribute value as written in HTML: a character reference (`&amp;`, `&quot;`,
    /// `&#39;`) stands for the character it encodes.
    Attribute,
}

/// The byte ranges of the `attachment:` URLs written inside a text that is not one URL: a
/// script (a `<script>` element, an `on…` attribute) or another attribute value (`srcdoc`,
/// a `<meta>` refresh's `content`, JSON in a `data-*` attribute). Such a text has no URL
/// syntax, so an `attachment:` (any ASCII case) counts only where a URL can start: at the
/// start of the text, or after white space or one of `` ' " ` ( = `` ([`url_may_start`];
/// not inside a word or another URL, `/x/attachment:a.png`, `/x?q=attachment:a.png`,
/// `/x?ids=1,2&u=attachment:a.png` (a query reaches back to white space, a quote, `<`,
/// `>` or one of `[ ] { }`, [`in_query_or_fragment`]), nor as an object key or a label,
/// `{attachment:x}`, `f(a,attachment:x)`). Then:
///
/// - the longest of the cell's attachment `names` written there (in an attribute value
///   also with character references, `a&amp;b.png`) is the URL when it ends after the
///   name ([`ends_embedded_url`], or `?` / `#`, which stay after the placeholder);
/// - otherwise, only where a string or a URL starts (after a quote, `(` or `=`, also
///   written as a character reference), the URL runs to the first [`ends_embedded_url`]
///   character, and [`Converter::attachment_url`] decodes it or reports it as missing.
///   An `attachment:` at the start or after white space that names no attachment is
///   text or a comment (`// see attachment:notes`), not a URL.
///
/// A bare `attachment:` and one followed by `${` (a script that builds the URL,
/// `'attachment:' + name`, `` `attachment:${name}` ``) are not candidates.
fn embedded_urls(text: &str, names: &[&str], written: Written) -> Vec<Range<usize>> {
    let entities = written == Written::Attribute;
    // ASCII lowercasing keeps every byte offset.
    let lower = text.to_ascii_lowercase();
    let mut urls = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(ATTACHMENT_SCHEME) {
        let start = from + at;
        let rest = start + ATTACHMENT_SCHEME.len();
        from = rest;
        let Some(string) = url_may_start(&text[..start], entities) else {
            continue;
        };
        let exact = names
            .iter()
            .find_map(|name| written_name_len(&text[rest..], name, entities));
        let len = match exact {
            Some(len) => len,
            None if string => unknown_name_len(&text[rest..], entities),
            None => 0,
        };
        if len > 0 {
            urls.push(start..rest + len);
            from = rest + len;
        }
    }
    urls
}

/// Whether a URL can start at the end of `before` ([`embedded_urls`]): `None` if not;
/// `Some(true)` after a quote, `(` or `=` (a string, a CSS `url(`, an HTML attribute
/// written in a string), but not after a `(` or `=` in another URL's query or fragment
/// ([`in_query_or_fragment`]: `?q=attachment:x`); `Some(false)` at the start of the text
/// and after white space. With `entities`, the characters before may be written as
/// character references (`&quot;`, `&amp;`).
fn url_may_start(before: &str, entities: bool) -> Option<bool> {
    let Some((at, last)) = char_before(before, before.len(), entities) else {
        return Some(false);
    };
    match last {
        '\'' | '"' | '`' => Some(true),
        '(' | '=' => (!in_query_or_fragment(&before[..at], entities)).then_some(true),
        c if c.is_whitespace() => Some(false),
        _ => None,
    }
}

/// Whether the end of `text` is inside a URL's query or fragment: a `?` or `#` follows the
/// last character that cannot be part of a URL ([`outside_url`]; `,` `;` `(` `)` can,
/// `https://example.org/?ids=1,2&amp;q` before `=attachment:x`). With `entities`, character
/// references count as the character they encode, also when written twice in HTML written
/// in an attribute (`&amp;amp;` in a `srcdoc` is the `&amp;` of the inner HTML, an `&`).
fn in_query_or_fragment(text: &str, entities: bool) -> bool {
    let mut end = text.len();
    while let Some((start, c)) = char_before(text, end, entities) {
        if matches!(c, '?' | '#') {
            return true;
        }
        if outside_url(c) {
            return false;
        }
        end = start;
    }
    false
}

/// Whether `c` cannot be part of a URL written in a script or an attribute value (character
/// references already decoded): white space, quotes, `<` and `>`, `\`, and the brackets
/// `[ ] { }` (which a URL percent-encodes). Unlike [`ends_embedded_url`], `,` `;` `(` `)`
/// can be, in a path or a query.
fn outside_url(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '[' | ']' | '{' | '}' | '\\'
        )
}

/// The last character of `text[..end]` and its byte offset; with `entities`, a character
/// reference that ends there (`&quot;`) is the one character it encodes
/// ([`char_reference`]). `None` when `end` is 0.
fn char_before(text: &str, end: usize, entities: bool) -> Option<(usize, char)> {
    let head = &text[..end];
    let last = head.chars().next_back()?;
    if entities && last == ';' {
        // `char_reference` reads at most 40 bytes, so its `&` is among the last 40.
        let amp = head.bytes().rev().take(40).position(|b| b == b'&');
        let reference = amp.map(|back| end - 1 - back).and_then(|amp| {
            let (len, c) = char_reference(&head[amp..])?;
            (amp + len == end).then_some((amp, c))
        });
        if reference.is_some() {
            return reference;
        }
    }
    Some((end - last.len_utf8(), last))
}

/// The length in `text` of the attachment `name` written at its start (as is, or with
/// `entities` also with character references, `a&amp;b.png` for `a&b.png`), when the URL
/// ends after it: at the end of `text`, an [`ends_embedded_url`] character, `?` or `#`.
fn written_name_len(text: &str, name: &str, entities: bool) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let mut chars = chars_of(text, entities);
    let mut len = 0;
    for expected in name.chars() {
        let (range, c) = chars.next()?;
        if c != expected {
            return None;
        }
        len = range.end;
    }
    match chars.next() {
        Some((_, c)) if !(ends_embedded_url(c) || matches!(c, '?' | '#')) => None,
        _ => Some(len),
    }
}

/// The length in `text` of a name that is none of the cell's attachments
/// ([`embedded_urls`]): up to the first [`ends_embedded_url`] character; 0 when that is
/// the first one, or when the name starts with `${` (a template literal builds the URL).
fn unknown_name_len(text: &str, entities: bool) -> usize {
    let mut lead = chars_of(text, entities).map(|(_, c)| c);
    if lead.next() == Some('$') && lead.next() == Some('{') {
        return 0;
    }
    chars_of(text, entities)
        .take_while(|(_, c)| !ends_embedded_url(*c))
        .last()
        .map_or(0, |(range, _)| range.end)
}

/// Whether `c` ends an `attachment:` URL written in a script or an attribute value
/// ([`embedded_urls`]; character references already decoded): white space, the quotes
/// that delimit a string, `<` and `>` (a tag written in a string), brackets (a call, a
/// CSS `url(…)`, an array, an object), `\` (an escape), `,` and `;` (an argument list, a
/// statement).
fn ends_embedded_url(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' | ',' | ';'
        )
}

/// The characters of `text` with their byte ranges in it: as written, or with `entities`
/// with each character reference (`&amp;`, `&#39;`, `&quot;`) read as the one character
/// it encodes ([`char_reference`]).
fn chars_of(text: &str, entities: bool) -> impl Iterator<Item = (Range<usize>, char)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        let c = text[at..].chars().next()?;
        let start = at;
        let reference = (entities && c == '&').then(|| char_reference(&text[at..]));
        match reference.flatten() {
            Some((len, decoded)) => {
                at += len;
                Some((start..at, decoded))
            }
            None => {
                at += c.len_utf8();
                Some((start..at, c))
            }
        }
    })
}

/// The HTML character reference at the start of `text` (`&amp;`, `&#39;`, `&#x27;`; with
/// its `;`, as [`html_escape::decode_html_entities`] decodes them) and the one character it
/// encodes: its length and that character. `None` for anything else.
fn char_reference(text: &str) -> Option<(usize, char)> {
    if !text.starts_with('&') {
        return None;
    }
    // The longest named reference, `&CounterClockwiseContourIntegral;`, has 33 bytes.
    let semicolon = text.bytes().take(40).position(|b| b == b';')?;
    let reference = &text[..=semicolon];
    if reference[1..].contains('&') {
        return None;
    }
    let decoded = html_escape::decode_html_entities(reference);
    let mut chars = decoded.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if decoded != reference => Some((reference.len(), c)),
        _ => None,
    }
}

/// The byte ranges of the URLs of the CSS `url(…)` functions in `css` (a `style` attribute
/// value as written in HTML, or a `<style>` element): white space and one pair of quotes
/// (`"`, `'`, or `&quot;` / `&#39;` in an attribute) removed. A `)` ends the function,
/// also inside quotes.
fn css_urls(css: &str) -> Vec<Range<usize>> {
    const FUNCTION: &str = "url(";
    const QUOTES: [&str; 5] = ["\"", "'", "&quot;", "&#39;", "&#x27;"];
    // ASCII lowercasing keeps every byte offset; CSS function names ignore ASCII case.
    let lower = css.to_ascii_lowercase();
    let mut urls = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(FUNCTION) {
        let start = from + at + FUNCTION.len();
        let Some(len) = css[start..].find(')') else {
            break;
        };
        let inner = trimmed(&css[start..start + len]);
        let mut url = start + inner.start..start + inner.end;
        let text = &css[url.clone()];
        if let Some(quote) = QUOTES.iter().find(|quote| {
            text.len() >= 2 * quote.len() && text.starts_with(**quote) && text.ends_with(**quote)
        }) {
            url = url.start + quote.len()..url.end - quote.len();
        }
        urls.push(url);
        from = start + len + 1;
    }
    urls
}

/// The byte range of `value` without leading and trailing ASCII white space (which URL
/// parsing ignores).
fn trimmed(value: &str) -> Range<usize> {
    let start = value.len()
        - value
            .trim_start_matches(|c: char| c.is_ascii_whitespace())
            .len();
    let end = value
        .trim_end_matches(|c: char| c.is_ascii_whitespace())
        .len();
    start..end.max(start)
}

/// The byte ranges of the URLs in a `srcset` value, by HTML's parsing rules: candidates
/// are separated by commas; a candidate's URL is its first run of non-white-space
/// characters without trailing commas, the descriptors after it (`2x`, `100w`) run to the
/// next comma outside parentheses.
fn srcset_urls(value: &str) -> Vec<Range<usize>> {
    let bytes = value.as_bytes();
    let mut urls = Vec::new();
    let mut at = 0;
    loop {
        while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b',') {
            at += 1;
        }
        if at == bytes.len() {
            return urls;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let mut end = at;
        while end > start && bytes[end - 1] == b',' {
            end -= 1;
        }
        urls.push(start..end);
        if end < at {
            continue; // `url,` ends the candidate
        }
        let mut depth = 0usize;
        while at < bytes.len() && !(bytes[at] == b',' && depth == 0) {
            match bytes[at] {
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            at += 1;
        }
    }
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
    fn srcset_urls_follow_the_html_rules() {
        let urls = |value: &str| -> Vec<String> {
            srcset_urls(value)
                .into_iter()
                .map(|range| value[range].to_owned())
                .collect()
        };
        assert_eq!(urls("a.png"), ["a.png"]);
        assert_eq!(
            urls(" attachment:a%2Bb.png 2x,\n  b.png 100w , c.png"),
            ["attachment:a%2Bb.png", "b.png", "c.png"]
        );
        // A URL may end with commas (no descriptors); a comma inside it is part of it.
        assert_eq!(urls("a.png,b.png 2x"), ["a.png,b.png"]);
        assert_eq!(urls("a.png, b.png,, c.png"), ["a.png", "b.png", "c.png"]);
        // Descriptors run to the next comma outside parentheses.
        assert_eq!(urls("a.png f(1,2), b.png"), ["a.png", "b.png"]);
        assert!(urls(" , ").is_empty());
        assert!(urls("").is_empty());
        let trim = |value: &str| value[trimmed(value)].to_owned();
        assert_eq!(trim(" \tattachment:a.png \n"), "attachment:a.png");
        assert_eq!(trim("   "), "");
        assert_eq!(trim("ü"), "ü");
    }

    #[test]
    fn css_and_attribute_url_candidates() {
        let texts = |html: &str, ranges: Vec<Range<usize>>| -> Vec<String> {
            ranges.into_iter().map(|r| html[r].to_owned()).collect()
        };
        let css = "background: URL( a.png ) , url(\"b c.png\") url('d.png'), \
                   url(&quot;e.png&quot;) url(&#39;f.png&#39;) url(\"\") url(g.png";
        assert_eq!(
            texts(css, css_urls(css)),
            ["a.png", "b c.png", "d.png", "e.png", "f.png", ""]
        );
        assert!(css_urls("color: red").is_empty());

        let html = "<p style=\"background:url(attachment:a.png)\" title=\"attachment:t\">\
                    <img alt=attachment:x SRC = attachment:b.png srcset='c.png 2x, d.png'>\
                    <svg><image xlink:href=\"attachment:e.png\"/></svg><a href=e.png>\
                    <STYLE>.x { background: url(f.png) } <img src=no.png></style>\
                    <!-- <img src=no.png> --><script>'<img src=no.png>'</script>\
                    <code>&lt;img src=\"no.png\"&gt;</code><style>url(g.png)";
        assert_eq!(
            texts(html, attachment_url_candidates(html, &[])),
            [
                "attachment:a.png",
                "attachment:b.png",
                "c.png",
                "d.png",
                "attachment:e.png",
                "f.png",
                "g.png"
            ]
        );
    }

    #[test]
    fn every_url_attribute_and_scripts_have_url_candidates() {
        let texts = |html: &str, ranges: Vec<Range<usize>>| -> Vec<String> {
            ranges.into_iter().map(|r| html[r].to_owned()).collect()
        };
        // The cell's attachment names, longest first.
        let names = ["my plot.png", "a.png", "a"];
        let html = "<img data-preview-image=' attachment:a.png ' aria-label=\"attachment:a.png\" \
                    label=attachment:a.png onclick=\"show(&quot;attachment:a.png&quot;)\">\
                    <span data-preview-video=attachment:v.mp4>v</span>\
                    <script>document.write('<img src=\"attachment:a.png\">'); \
                    f(`attachment:my plot.png`, 'ATTACHMENT:a?x', \"attachment:a.pngx\", \
                    'attachment:' + n, 'attachment:%61.png');</script>\
                    <code>&lt;script&gt;'attachment:a.png'&lt;/script&gt;</code>";
        assert_eq!(
            texts(html, attachment_url_candidates(html, &names)),
            [
                "attachment:a.png",
                "attachment:a.png",
                "attachment:v.mp4",
                "attachment:a.png",
                "attachment:my plot.png",
                "ATTACHMENT:a",
                "attachment:a.pngx",
                "attachment:%61.png",
            ]
        );
        // Without the names, a name with a space ends at the space.
        let script = "x = 'attachment:my plot.png'";
        let urls = embedded_urls(script, &[], Written::Script);
        assert_eq!(texts(script, urls), ["attachment:my"]);
        assert!(embedded_urls("'attachment:' + name", &names, Written::Script).is_empty());
    }

    /// `attachment:` URLs inside a larger attribute value (`srcdoc`, a refresh, JSON) are
    /// candidates; so are names written with character references in an attribute.
    #[test]
    fn attachment_urls_inside_other_attribute_values_are_candidates() {
        let texts = |html: &str| -> Vec<String> {
            let names = ["a&b.png", "a.png"];
            let ranges = attachment_url_candidates(html, &names);
            ranges.into_iter().map(|r| html[r].to_owned()).collect()
        };
        let html = "<iframe srcdoc=\"<img src='attachment:a.png'><p>attachment:a.png</p>\" \
                    data-x='{\"img\":\"attachment:a.png\",\"n\":\"attachment:no.png\"}'>\
                    </iframe><iframe srcdoc=\"<img src=&quot;attachment:a.png&quot; \
                    srcset=&quot;attachment:a.png 1x, attachment:a&amp;b.png 2x&quot;>\">\
                    </iframe><meta http-equiv=refresh content=\"0; url=attachment:a.png\">\
                    <meta http-equiv=refresh content=\"0;URL='attachment:gone.png'\">";
        // The `<p>` in the first `srcdoc` is text, not a URL.
        assert_eq!(
            texts(html),
            [
                "attachment:a.png",
                "attachment:a.png",
                "attachment:no.png",
                "attachment:a.png",
                "attachment:a.png",
                "attachment:a&amp;b.png",
                "attachment:a.png",
                "attachment:gone.png",
            ]
        );
        // `on…` attributes: names with character references, quotes written as references.
        let html = "<b onclick=\"show('attachment:a&amp;b.png'); show(&#39;attachment:a&#x26;b.png&#39;); \
                    show(&quot;attachment:no&amp;pe.png&quot;)\">";
        assert_eq!(
            texts(html),
            [
                "attachment:a&amp;b.png",
                "attachment:a&#x26;b.png",
                "attachment:no&amp;pe.png"
            ]
        );
        // Not URLs: inside another URL or a word, and text in a value that names no
        // attachment.
        let html = "<a href=\"https://example.org/attachment:a.png\" \
                    data-note=\"see attachment:notes, [attachment:x] {attachment:y}\">a</a>";
        assert!(texts(html).is_empty(), "{:?}", texts(html));
    }

    /// An attribute whose value is one URL (`href`, `src`, `data-preview-image`, …) is a
    /// candidate only as a whole: an `attachment:` in its query or fragment, or after white
    /// space, is part of that URL. Inside other values, so is an `attachment:` after a `=` or
    /// `(` in a URL's query or fragment, also after `,` `;` `(` `)` in the query and after
    /// an `&` written twice escaped in a `srcdoc`. None of these is resolved or reported. A
    /// `javascript:` or `data:` URL holds a script or a document, scanned like one.
    #[test]
    fn attachment_inside_another_url_is_no_candidate() {
        let texts = |html: &str| -> Vec<String> {
            let names = ["a&b.png", "a.png"];
            let ranges = attachment_url_candidates(html, &names);
            ranges.into_iter().map(|r| html[r].to_owned()).collect()
        };
        let html = "<a href=\"https://example.org/search?q=attachment:foo\">q</a>\
                    <a href=\"https://example.org/?u=attachment:a.png\">u</a>\
                    <a href='https://example.org/#k=attachment:a.png'>k</a>\
                    <img src=\"x.png?v=(attachment:a.png)\" poster=\"see attachment:a.png\" \
                    data-preview-image=\"x.png#attachment:a.png\" \
                    data-background-image=\"x.png?u=attachment:a.png\">\
                    <div data-url=\"https://example.org/?a=1&amp;u=attachment:a.png#f(attachment:a.png)\" \
                    data-cfg='{\"u\":\"https://example.org/?q=attachment:nope.png\"}'></div>\
                    <iframe srcdoc=\"<a href='https://example.org/?u=attachment:a&amp;b.png'>u</a>\
                    <a href=https://example.org/?x&#61;attachment:a.png>v</a>\"></iframe>\
                    <div data-url=\"https://example.org/?ids=1,2&amp;u=attachment:a.png\" \
                    data-a=\"https://example.org/?a=1;u=attachment:nope.png\" \
                    data-b=\"https://example.org/?q=(x)&amp;u=attachment:a.png\"></div>\
                    <iframe srcdoc=\"<a href='https://x/?a=1&amp;amp;u=attachment:nope.png'>\"></iframe>\
                    <a href=\"javascript:go('https://example.org/?q=attachment:a.png')\">go</a>";
        assert!(texts(html).is_empty(), "{:?}", texts(html));
        // The whole value still is one, and so is a URL after a query in another value.
        let html = "<a href=\" attachment:a.png?x=attachment:a.png \" \
                    data-preview-video=attachment:a&amp;b.png>v</a>\
                    <div data-cfg='{\"u\":\"https://example.org/?q=1\",\"v\":\"attachment:a.png\"}' \
                    data-x=\"https://example.org/?q=1 u=attachment:no.png\"></div>\
                    <a href=\"javascript:show('attachment:a.png')\" \
                    data-preview-link=\" JavaScript:show(&quot;attachment:gone.png&quot;)\">js</a>\
                    <iframe src=\"DATA:text/html,<img src='attachment:a&amp;b.png'>\"></iframe>";
        assert_eq!(
            texts(html),
            [
                "attachment:a.png?x=attachment:a.png",
                "attachment:a&amp;b.png",
                "attachment:a.png",
                "attachment:no.png",
                "attachment:a.png",
                "attachment:gone.png",
                "attachment:a&amp;b.png",
            ]
        );
        assert!(is_code_url("javascript:x()") && is_code_url("Data:,x"));
        assert!(!is_code_url("https://example.org/data:x") && !is_code_url("java"));
    }

    #[test]
    fn urls_may_start_after_quotes_and_white_space_but_not_in_a_query() {
        for (before, entities, expected) in [
            ("", false, Some(false)),
            ("x = ", false, Some(false)),
            ("f(", false, Some(true)),
            ("show(&quot;", true, Some(true)),
            ("show(&quot;", false, None),
            ("0; url=", true, Some(true)),
            ("https://x/attachment", false, None),
            ("https://x/?q=", true, None),
            ("https://x/?a=1&amp;u=", true, None),
            ("https://x/?a=f(", false, None),
            ("https://x/#k=", false, None),
            ("x&#63;q=", true, None),
            ("x&quest;q=", true, None),
            ("x&quest;q=", false, Some(true)),
            ("https://x/?q=1 u=", false, Some(true)),
            ("'https://x/?q=1', u=", false, Some(true)),
            // A query goes on past `,` `;` `(` `)`, and `&amp;amp;` (twice escaped).
            ("https://x/?ids=1,2&amp;u=", true, None),
            ("https://x/?a=1;u=", false, None),
            ("https://x/?q=(x)&u=", false, None),
            ("https://x/?a=1&amp;amp;u=", true, None),
            // It ends at `<`, `>`, `[ ] { }`, and quotes.
            ("x?y>z=", false, Some(true)),
            ("x?y}z=", false, Some(true)),
            ("x?y&quot;z=", true, Some(true)),
        ] {
            assert_eq!(
                url_may_start(before, entities),
                expected,
                "{before:?} {entities}"
            );
        }
        assert_eq!(char_before("a&quot;", 7, true), Some((1, '"')));
        assert_eq!(char_before("a&quot;", 7, false), Some((6, ';')));
        assert_eq!(char_before("aé", 3, false), Some((1, 'é')));
        assert_eq!(char_before("a", 0, true), None);
    }

    /// Scripts that use the word `attachment:` without writing an attachment URL: object
    /// keys (also one whose value is written like an attachment's name), comments,
    /// template literals and string concatenation.
    #[test]
    fn script_code_that_is_not_an_attachment_url_is_no_candidate() {
        let names = ["a&b.png", "a.png"];
        let script = "var o = {attachment:true}; f({attachment:x}); g([attachment:z]);\n\
                      var k = {attachment:a.png}, l = {n,attachment:a.png};\n\
                      // TODO: see attachment:notes later\n\
                      /* attachment:old.png */ var u = `attachment:${name}`;\n\
                      var v = 'attachment:' + name, w = \"attachment:\" + other;";
        for written in [Written::Script, Written::Attribute] {
            let urls = embedded_urls(script, &names, written);
            assert!(urls.is_empty(), "{written:?}: {urls:?}");
        }
        // An exact name resolves after white space too (a comment that names it is
        // harmless); `&` is part of a name in a `<script>`, which decodes no references.
        let script = "// shows attachment:a.png\nload('attachment:a&b.png', 'attachment:x&y.png')";
        let urls = embedded_urls(script, &names, Written::Script);
        let texts: Vec<&str> = urls.into_iter().map(|r| &script[r]).collect();
        assert_eq!(
            texts,
            [
                "attachment:a.png",
                "attachment:a&b.png",
                "attachment:x&y.png"
            ]
        );
    }

    #[test]
    fn character_references_decode_to_one_character() {
        assert_eq!(char_reference("&amp;b"), Some((5, '&')));
        assert_eq!(char_reference("&#39;"), Some((5, '\'')));
        assert_eq!(char_reference("&#x27;x"), Some((6, '\'')));
        assert_eq!(char_reference("&quot;"), Some((6, '"')));
        assert_eq!(char_reference("&nope;"), None);
        assert_eq!(char_reference("&amp"), None);
        assert_eq!(char_reference("&a&amp;"), None);
        assert_eq!(char_reference("x&amp;"), None);
        let chars: Vec<char> = chars_of("a&amp;b&x", true).map(|(_, c)| c).collect();
        assert_eq!(chars, ['a', '&', 'b', '&', 'x']);
        let ranges: Vec<Range<usize>> = chars_of("&lt;é", true).map(|(r, _)| r).collect();
        assert_eq!(ranges, [0..4, 4..6]);
        assert_eq!(chars_of("a&amp;", false).count(), 6);
    }

    #[test]
    fn attachment_payloads_decode() {
        assert_eq!(decode_attachment("aGk=\n").unwrap(), b"hi");
        assert_eq!(decode_attachment("aGk").unwrap(), b"hi");
        assert_eq!(decode_attachment("<svg/>").unwrap(), b"<svg/>");
        assert!(decode_attachment("  ").is_err());
        assert!(decode_attachment("!!!").is_err());
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
                cell: Some(2),
                file: None
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
            file: None,
        };
        let path = Path::new("/nb.ipynb");
        let inner = Diagnostic::warning("missing-asset", "m").with_span(SourceSpan {
            start_line: 2,
            end_line: 3,
            cell: None,
            file: None,
        });
        let moved = relocate(inner, Some(&at), 3, path);
        assert_eq!(
            moved.span,
            Some(SourceSpan {
                start_line: 5,
                end_line: 6,
                cell: Some(4),
                file: None
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
