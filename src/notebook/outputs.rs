//! Output rendering: each Jupyter output becomes one [`CellOutput`] with ready-to-insert HTML.
//!
//! Contract: `docs/spec/notebook-mapping.md` §4 and `docs/spec/ir.md` (`CellOutput`). The entry
//! point is [`render_outputs`], called by the converter (`notebook::convert`, T31) once per
//! code cell.
//!
//! - **Streams**: consecutive `stream` outputs with the same `name` are merged (Jupyter splits
//!   them arbitrarily). Like nbconvert's `CoalesceStreamsPreprocessor`, a line's text up to its
//!   last `\r` that is followed by more text is dropped (progress bars), then
//!   [`ansi_to_html()`] escapes the text and turns ANSI colors into `<span>`s.
//! - **Errors**: the traceback lines joined with `\n`, through [`ansi_to_html()`].
//! - **Rich outputs** (`display_data`, `execute_result`): [`choose_mime`] picks the first MIME
//!   type of `[notebook] mime_priority` present in the bundle and [`render_display`] renders it
//!   per the §4 table; `text/markdown` goes through the caller's Markdown closure instead.
//!   When the chosen payload cannot be rendered (invalid base64, a non-text payload, a MIME
//!   type without a renderer), a [`codes::OUTPUT_INVALID`] warning is recorded and the next
//!   MIME type of the priority list present in the bundle is tried, so a broken PNG falls back
//!   to its `text/plain` repr exactly as a bundle without the PNG would. When nothing renders,
//!   the output is [`CellOutput::Unsupported`] with every MIME type of the bundle and a
//!   [`codes::OUTPUT_UNSUPPORTED`] warning. A widget bundle rendered through a fallback type
//!   gets a [`codes::OUTPUT_WIDGET`] warning.
//!
//! Diagnostics are pushed without `path` or `span` (this module does not know them); their
//! messages start with `output <i>: `, where `i` is the 0-based position in the `outputs`
//! slice. That slice holds the outputs `nbformat` could parse, so after an output it skipped
//! (its `notebook-output-skipped` warning counts positions in the `.ipynb` array) `i` is
//! smaller than the output's position in the file. The caller attaches the notebook path and
//! the cell (`notebook::convert`: the span of the whole cell, also for the local-file warnings
//! `missing-asset` and [`codes::OUTPUT_FILE_OUTSIDE`]; an output is not a line of the cell's
//! source, so a report quotes no source line for a missing file the line does not mention).
//!
//! **Local files** (`docs/spec/notebook-mapping.md` §4): [`render_outputs`] leaves relative
//! URLs of `text/html` as written and renders `text/markdown` with the caller's closure.
//! [`render_cell_outputs`], which the notebook frontend uses, resolves them against the
//! notebook's directory ([`OutputFiles`]): relative `src`/`poster` URLs of `text/html` (the
//! Markdown renderer's raw-HTML rule) and the images and raw HTML of `text/markdown` become
//! `asset:<id>` placeholders (ir.md invariant 4) **only for files inside the notebook's
//! directory** (subdirectories included, symbolic links resolved). Outputs are written by
//! executed code, not by the author, so a file outside it (`../secret.txt`, a link pointing
//! elsewhere) is not embedded: the URL stays as written and a [`codes::OUTPUT_FILE_OUTSIDE`]
//! warning names it (audit F08). A missing file is a `missing-asset` warning, as everywhere.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use comrak::Options;
use regex::Regex;
use serde_json::Value;

use crate::assets::registry::{AssetData, AssetRegistry};
use crate::config::NotebookConfig;
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{CellOutput, Features};
use crate::markdown::render::{
    RenderCtx, RenderMode, html_attr_values, render_fragment, rewrite_html_assets,
};
use crate::notebook::nbformat::{MimeBundle, Output, mime_text};

/// The diagnostic codes [`render_outputs`] records (all `Level::Warning`).
pub mod codes {
    /// A `text/html` output contains `<script`; it is kept verbatim and runs when the deck
    /// loads.
    pub const OUTPUT_SCRIPT: &str = "output-script";
    /// No MIME type of the bundle could be rendered (e.g. a widget view without a
    /// `text/plain` fallback); the output became `CellOutput::Unsupported`.
    pub const OUTPUT_UNSUPPORTED: &str = "output-unsupported";
    /// One MIME type of a bundle could not be rendered (invalid base64, a payload that is not
    /// text, no renderer for a type listed in `mime_priority`); the next type was tried.
    pub const OUTPUT_INVALID: &str = "output-invalid";
    /// A bundle with an interactive widget view
    /// (`application/vnd.jupyter.widget-view+json`) was rendered through another MIME type,
    /// usually its `text/plain` repr (spec §4: "interactive widget rendered as text").
    pub const OUTPUT_WIDGET: &str = "output-widget";
    /// A `text/html` or `text/markdown` output names a local file outside the notebook's
    /// directory; it is not embedded and the URL stays as written ([`super::OutputFiles`]).
    pub const OUTPUT_FILE_OUTSIDE: &str = "output-file-outside";
}

/// Where [`render_cell_outputs`] resolves the local files outputs name, and how it renders
/// `text/markdown` (module docs).
#[derive(Debug, Clone, Copy)]
pub struct OutputFiles<'a> {
    /// The notebook's directory (`SourceInfo.base_dir`): relative URLs resolve here, and a
    /// file is only embedded when it is inside it.
    pub base_dir: &'a Path,
    /// The comrak options `text/markdown` outputs are rendered with (notebook mode).
    pub markdown: &'a Options<'static>,
}

/// What [`render_cell_outputs`] produced for one code cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CellOutputs {
    /// One [`CellOutput`] per output (consecutive streams merged).
    pub outputs: Vec<CellOutput>,
    /// `math` (as [`render_outputs`] sets it) and the `code` and `math` of `text/markdown`
    /// outputs (fenced code, math). `notes` is never set: whether the notes count depends on
    /// whether the outputs are shown.
    pub features: Features,
    /// The `:::notes` of `text/markdown` outputs, rendered and concatenated.
    pub notes: Option<String>,
}

/// The MIME type of an interactive widget view; never rendered, see [`codes::OUTPUT_WIDGET`].
const WIDGET_VIEW: &str = "application/vnd.jupyter.widget-view+json";

/// Renders the outputs of one code cell (spec §4).
///
/// `md` renders Markdown to HTML (T12's renderer in notebook mode); it is called for each
/// output whose chosen MIME type is `text/markdown`, and its errors are returned unchanged.
/// Images are registered in `assets` and referenced as `asset:<id>`. Warnings (codes in
/// [`codes`]) are appended to `diags` without path or span, see the module docs.
///
/// The returned [`Features`] only ever sets `math`: when a `text/latex` output was rendered
/// or the markup of a `text/html`, `text/markdown` or `image/svg+xml` output contains
/// `data-math-style` (e.g. Markdown with `$x$`). Escaped text never sets it. The caller sets
/// `notebook` and `code`.
///
/// # The Markdown closure
/// `md` cannot borrow `assets` or `diags` itself (they are already mutably borrowed by this
/// call), so a closure over T12's `render_fragment` renders into its own [`AssetRegistry`]
/// and [`Diagnostics`], which the caller merges afterwards. Forgetting the merge leaves
/// `asset:<id>` placeholders that the emitter cannot resolve. The closure returns only HTML:
/// if the fragment's other results matter (`Features.code` for fenced code in a Markdown
/// output, speaker notes), the closure must capture them; this function only infers `math`.
///
/// ```
/// use presenter::assets::registry::AssetRegistry;
/// use presenter::config::NotebookConfig;
/// use presenter::error::{Diagnostics, Result};
/// use presenter::notebook::nbformat::Output;
/// use presenter::notebook::outputs::render_outputs;
///
/// # fn main() -> Result<()> {
/// let outputs: Vec<Output> = Vec::new(); // a code cell's outputs
/// let (mut assets, mut diags) = (AssetRegistry::new(), Diagnostics::new());
/// let (mut md_assets, mut md_diags) = (AssetRegistry::new(), Diagnostics::new());
/// let mut md = |text: &str| -> Result<String> {
///     // A RenderCtx over `&mut md_assets` and `&mut md_diags` goes here.
///     let _ = (&mut md_assets, &mut md_diags);
///     Ok(text.to_owned())
/// };
/// let cfg = NotebookConfig::default();
/// let (rendered, features) = render_outputs(&outputs, &cfg, &mut assets, &mut diags, &mut md)?;
/// assets.merge(md_assets);
/// diags.extend(md_diags);
/// # assert!(rendered.is_empty() && !features.math);
/// # Ok(())
/// # }
/// ```
///
/// `text/html` URLs are not rewritten; see the module docs ([`render_cell_outputs`] does).
///
/// # Errors
/// Only the errors of `md`. Malformed payloads are warnings, never errors.
pub fn render_outputs(
    outputs: &[Output],
    cfg: &NotebookConfig,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
    md: &mut dyn FnMut(&str) -> Result<String>,
) -> Result<(Vec<CellOutput>, Features)> {
    render_with(outputs, cfg, assets, diags, &mut Closure(md))
}

/// [`render_outputs`] for a code cell of a notebook in `files.base_dir`: `text/markdown`
/// is rendered with T12's renderer in notebook mode (`files.markdown`), and the local files
/// that `text/html` and `text/markdown` outputs name are resolved, inside the notebook's
/// directory only (module docs). Assets go to `assets`; warnings (also those of the Markdown
/// renderer) to `diags`, each message starting with `output <i>: `.
///
/// # Errors
/// Those of the Markdown renderer (an unbalanced quote in a directive's attributes, …).
/// Malformed payloads, missing files and files outside the directory are warnings.
pub fn render_cell_outputs(
    outputs: &[Output],
    cfg: &NotebookConfig,
    files: &OutputFiles<'_>,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
) -> Result<CellOutputs> {
    let mut local = LocalFiles {
        files,
        notes: None,
        features: Features::default(),
    };
    let (outputs, mut features) = render_with(outputs, cfg, assets, diags, &mut local)?;
    features.merge(local.features);
    features.notes = false;
    Ok(CellOutputs {
        outputs,
        features,
        notes: local.notes,
    })
}

/// The shared implementation of [`render_outputs`] and [`render_cell_outputs`].
fn render_with(
    outputs: &[Output],
    cfg: &NotebookConfig,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
    hooks: &mut dyn Hooks,
) -> Result<(Vec<CellOutput>, Features)> {
    let mut renderer = Renderer {
        priority: &cfg.mime_priority,
        assets,
        diags,
        hooks,
        features: Features::default(),
    };
    let mut rendered = Vec::with_capacity(outputs.len());
    // The stream being merged: (name, text so far).
    let mut stream: Option<(&str, String)> = None;
    for (index, output) in outputs.iter().enumerate() {
        let (data, metadata, execution_count) = match output {
            Output::Stream { name, text } => {
                match &mut stream {
                    Some((open, buffer)) if *open == name.as_str() => {
                        buffer.push_str(text.as_str());
                    }
                    _ => {
                        flush_stream(&mut stream, &mut rendered);
                        stream = Some((name, text.as_str().to_owned()));
                    }
                }
                continue;
            }
            Output::Error {
                ename,
                evalue,
                traceback,
            } => {
                flush_stream(&mut stream, &mut rendered);
                rendered.push(CellOutput::Error {
                    ename: ename.clone(),
                    evalue: evalue.clone(),
                    html: ansi_to_html(&traceback.join("\n")),
                });
                continue;
            }
            Output::DisplayData { data, metadata } => (data, metadata, None),
            Output::ExecuteResult {
                data,
                metadata,
                execution_count,
            } => (data, metadata, *execution_count),
        };
        flush_stream(&mut stream, &mut rendered);
        rendered.push(renderer.bundle(index, data, metadata, execution_count)?);
    }
    flush_stream(&mut stream, &mut rendered);
    Ok((rendered, renderer.features))
}

/// Pushes the merged stream, if any, as a [`CellOutput::Stream`].
fn flush_stream(stream: &mut Option<(&str, String)>, rendered: &mut Vec<CellOutput>) {
    if let Some((name, text)) = stream.take() {
        rendered.push(CellOutput::Stream {
            name: name.to_owned(),
            html: ansi_to_html(&apply_carriage_returns(&text)),
        });
    }
}

/// Everything rendering a MIME bundle needs, so the helpers stay small.
struct Renderer<'a> {
    priority: &'a [String],
    assets: &'a mut AssetRegistry,
    diags: &'a mut Diagnostics,
    hooks: &'a mut dyn Hooks,
    features: Features,
}

/// What differs between [`render_outputs`] and [`render_cell_outputs`]: how `text/markdown`
/// is rendered and whether local files of `text/html` are resolved. Warnings go to `diags`
/// (the renderer prefixes them with the output), assets to `assets`.
trait Hooks {
    /// The HTML of a `text/markdown` payload.
    fn markdown(
        &mut self,
        text: &str,
        assets: &mut AssetRegistry,
        diags: &mut Diagnostics,
    ) -> Result<String>;

    /// Resolves the local files a `text/html` payload names, in place.
    fn html(
        &mut self,
        html: &mut String,
        assets: &mut AssetRegistry,
        diags: &mut Diagnostics,
    ) -> Result<()>;
}

/// [`render_outputs`]' hooks: the caller's Markdown closure (with its own registry and
/// diagnostics), `text/html` as is.
struct Closure<'m>(&'m mut dyn FnMut(&str) -> Result<String>);

impl Hooks for Closure<'_> {
    fn markdown(
        &mut self,
        text: &str,
        _: &mut AssetRegistry,
        _: &mut Diagnostics,
    ) -> Result<String> {
        (self.0)(text)
    }

    fn html(&mut self, _: &mut String, _: &mut AssetRegistry, _: &mut Diagnostics) -> Result<()> {
        Ok(())
    }
}

/// [`render_cell_outputs`]' hooks: local files inside the notebook's directory only.
struct LocalFiles<'f> {
    files: &'f OutputFiles<'f>,
    /// The `:::notes` of the `text/markdown` outputs so far.
    notes: Option<String>,
    /// The features of the `text/markdown` outputs so far.
    features: Features,
}

impl Hooks for LocalFiles<'_> {
    fn markdown(
        &mut self,
        text: &str,
        assets: &mut AssetRegistry,
        diags: &mut Diagnostics,
    ) -> Result<String> {
        let base_dir = self.files.base_dir;
        let render = |base_dir: &Path, assets: &mut AssetRegistry, diags: &mut Diagnostics| {
            let mut ctx = RenderCtx {
                assets,
                base_dir,
                mode: RenderMode::Notebook,
                diagnostics: diags,
                missing_asset_is_error: false,
            };
            render_fragment(text, self.files.markdown, &mut ctx)
        };
        let mut scratch = AssetRegistry::new();
        let mut rendered = render(base_dir, &mut scratch, diags)?;
        let outside = split_outside(&mut scratch, base_dir);
        if !outside.is_empty() {
            // The same text with no local file resolved: every URL as written. No directory
            // has a NUL in its name, so nothing below this one exists.
            let nowhere = base_dir.join("\0");
            let written = render(&nowhere, &mut AssetRegistry::new(), &mut Diagnostics::new())?;
            let mut restored = Vec::new();
            rendered.html = restore_urls(&rendered.html, &written.html, &outside, &mut restored);
            if let (Some(notes), Some(as_written)) = (&rendered.notes, &written.notes) {
                rendered.notes = Some(restore_urls(notes, as_written, &outside, &mut restored));
            }
            warn_outside(&restored, &outside, base_dir, diags);
        }
        assets.merge(scratch);
        self.features.merge(rendered.features);
        if let Some(notes) = rendered.notes {
            self.notes.get_or_insert_with(String::new).push_str(&notes);
        }
        Ok(rendered.html)
    }

    fn html(
        &mut self,
        html: &mut String,
        assets: &mut AssetRegistry,
        diags: &mut Diagnostics,
    ) -> Result<()> {
        let base_dir = self.files.base_dir;
        let written = html.clone();
        let mut scratch = AssetRegistry::new();
        let mut ctx = RenderCtx {
            assets: &mut scratch,
            base_dir,
            mode: RenderMode::Notebook,
            diagnostics: diags,
            missing_asset_is_error: false,
        };
        rewrite_html_assets(html, None, &mut ctx)?;
        let outside = split_outside(&mut scratch, base_dir);
        if !outside.is_empty() {
            let mut restored = Vec::new();
            *html = restore_urls(html, &written, &outside, &mut restored);
            warn_outside(&restored, &outside, base_dir, diags);
        }
        assets.merge(scratch);
        Ok(())
    }
}

/// Removes from `registry` the file assets that are not inside `base_dir` (canonical paths,
/// symbolic links resolved; a path that cannot be resolved counts as outside) and returns
/// them by id.
fn split_outside(registry: &mut AssetRegistry, base_dir: &Path) -> BTreeMap<String, PathBuf> {
    let root = std::fs::canonicalize(base_dir).ok();
    let mut outside = BTreeMap::new();
    let mut inside = AssetRegistry::new();
    for asset in std::mem::take(registry).iter() {
        match &asset.data {
            AssetData::File(path) => {
                let within = root.as_ref().is_some_and(|root| {
                    std::fs::canonicalize(path).is_ok_and(|path| path.starts_with(root))
                });
                if within {
                    inside.register_file(path);
                } else {
                    outside.insert(asset.id.clone(), path.clone());
                }
            }
            AssetData::Bytes(bytes) => {
                inside.register_bytes(bytes.clone(), &asset.mime, &asset.file_name_hint);
            }
        }
    }
    *registry = inside;
    outside
}

/// `rewritten` (HTML whose local URLs became placeholders) with every attribute value that
/// names one of the `outside` ids put back as `written` (the same HTML before, or rendered
/// without resolving files) has it; each value put back is appended to `restored` with the
/// id, entities decoded. Both have the same start tags and attributes in the same order, as
/// only attribute values were rewritten; should they not, `written` is returned whole
/// (nothing from this output is embedded) and nothing is appended.
fn restore_urls(
    rewritten: &str,
    written: &str,
    outside: &BTreeMap<String, PathBuf>,
    restored: &mut Vec<(String, String)>,
) -> String {
    let (new, old) = (html_attr_values(rewritten), html_attr_values(written));
    let same_shape = new.len() == old.len() && new.iter().zip(&old).all(|(a, b)| a.0 == b.0);
    if !same_shape {
        tracing::debug!("output HTML changed shape while resolving files; keeping it as written");
        return written.to_owned();
    }
    let mut out = String::with_capacity(rewritten.len());
    let mut last = 0;
    for ((_, value), (_, original)) in new.iter().zip(&old) {
        let text = &rewritten[value.clone()];
        let Some(id) = outside
            .keys()
            .find(|id| text.contains(&AssetRegistry::placeholder(id)))
        else {
            continue;
        };
        out.push_str(&rewritten[last..value.start]);
        out.push_str(&written[original.clone()]);
        last = value.end;
        let url = html_escape::decode_html_entities(&written[original.clone()]).into_owned();
        restored.push((url, id.clone()));
    }
    out.push_str(&rewritten[last..]);
    out
}

/// One [`codes::OUTPUT_FILE_OUTSIDE`] warning per URL in `restored` (`(URL as written, id)`,
/// in document order, each URL once), naming the file it resolved to; for the ids of
/// `outside` that no restored URL names (the output was kept as written whole), one per file.
fn warn_outside(
    restored: &[(String, String)],
    outside: &BTreeMap<String, PathBuf>,
    base_dir: &Path,
    diags: &mut Diagnostics,
) {
    let mut warn = |what: String, path: &Path| {
        diags.warn(
            codes::OUTPUT_FILE_OUTSIDE,
            format!(
                "{what} {}, which is outside the notebook's directory ({}): an output may only \
                 embed files inside it, so it is not embedded and its URL is left as written",
                path.display(),
                base_dir.display()
            ),
        );
    };
    let mut seen: Vec<&str> = Vec::new();
    for (url, id) in restored {
        if seen.contains(&url.as_str()) {
            continue;
        }
        seen.push(url);
        if let Some(path) = outside.get(id) {
            warn(format!("`{url}` names"), path);
        }
    }
    for (id, path) in outside {
        if !restored.iter().any(|(_, named)| named == id) {
            warn("an output names".to_owned(), path);
        }
    }
}

impl Renderer<'_> {
    /// Output `index` of the cell, a `display_data` (`execution_count` `None`) or an
    /// `execute_result`.
    fn bundle(
        &mut self,
        index: usize,
        data: &MimeBundle,
        metadata: &Value,
        execution_count: Option<i64>,
    ) -> Result<CellOutput> {
        for (mime, payload) in candidates(data, self.priority) {
            let Some((html, sets_math)) = self.try_mime(index, mime, payload, metadata)? else {
                continue;
            };
            self.check_rendered(index, data, mime, &html);
            // Only markup can hold math elements; escaped text that mentions the attribute
            // must not pull KaTeX into the deck.
            let markup = matches!(mime, "text/html" | "text/markdown" | "image/svg+xml");
            self.features.math |= sets_math || (markup && html.contains("data-math-style"));
            return Ok(CellOutput::Display {
                mime: mime.to_owned(),
                html,
                execution_count,
            });
        }
        let mimes: Vec<String> = data.keys().cloned().collect();
        let found = if mimes.is_empty() {
            "none".to_owned()
        } else {
            mimes.join(", ")
        };
        self.diags.warn(
            codes::OUTPUT_UNSUPPORTED,
            format!(
                "output {index}: no renderable MIME type (found: {found}); \
                 [notebook] mime_priority decides what is rendered"
            ),
        );
        Ok(CellOutput::Unsupported { mimes })
    }

    /// The warnings about a rendered output: scripts in `text/html`
    /// ([`codes::OUTPUT_SCRIPT`]) and a widget shown through a fallback type
    /// ([`codes::OUTPUT_WIDGET`]).
    fn check_rendered(&mut self, index: usize, data: &MimeBundle, mime: &str, html: &str) {
        if mime == "text/html" && html.to_ascii_lowercase().contains("<script") {
            self.diags.warn(
                codes::OUTPUT_SCRIPT,
                format!(
                    "output {index}: text/html contains <script>; \
                     it is kept and runs when the slides load"
                ),
            );
        }
        if mime != WIDGET_VIEW && data.contains_key(WIDGET_VIEW) {
            self.diags.warn(
                codes::OUTPUT_WIDGET,
                format!(
                    "output {index}: interactive widget rendered as {mime} \
                     (widgets need a live kernel)"
                ),
            );
        }
    }

    /// Renders one MIME type; `None` (with an [`codes::OUTPUT_INVALID`] warning) when its
    /// payload cannot be rendered. Errors are only those of the Markdown closure.
    fn try_mime(
        &mut self,
        index: usize,
        mime: &str,
        payload: &Value,
        metadata: &Value,
    ) -> Result<Option<(String, bool)>> {
        let attempt = if mime == "text/markdown" {
            match payload_text(payload) {
                Ok(text) => Ok((
                    self.with_hooks(index, |h, a, d| h.markdown(&text, a, d))?,
                    false,
                )),
                Err(err) => Err(err),
            }
        } else {
            render_display(mime, payload, metadata, self.assets)
        };
        match attempt {
            Ok((mut html, sets_math)) => {
                if mime == "text/html" {
                    self.with_hooks(index, |hooks, assets, diags| {
                        hooks.html(&mut html, assets, diags)
                    })?;
                }
                Ok(Some((html, sets_math)))
            }
            Err(err) => {
                let reason = match err {
                    Error::Other(message) => message,
                    other => other.to_string(),
                };
                self.diags.warn(
                    codes::OUTPUT_INVALID,
                    format!("output {index}: {mime} not rendered: {reason}"),
                );
                Ok(None)
            }
        }
    }
}

impl Renderer<'_> {
    /// Runs a hook for output `index`; its warnings get the `output <index>: ` prefix.
    fn with_hooks<T>(
        &mut self,
        index: usize,
        hook: impl FnOnce(&mut dyn Hooks, &mut AssetRegistry, &mut Diagnostics) -> Result<T>,
    ) -> Result<T> {
        let mut local = Diagnostics::new();
        let result = hook(&mut *self.hooks, &mut *self.assets, &mut local);
        for mut diagnostic in local {
            diagnostic.message = format!("output {index}: {}", diagnostic.message);
            self.diags.push(diagnostic);
        }
        result
    }
}

/// The MIME types of `priority` present in `bundle`, in priority order, with their payloads.
fn candidates<'a>(
    bundle: &'a MimeBundle,
    priority: &[String],
) -> impl Iterator<Item = (&'a str, &'a Value)> {
    priority
        .iter()
        .filter_map(|mime| bundle.get_key_value(mime.as_str()))
        .map(|(mime, payload)| (mime.as_str(), payload))
}

/// The first entry of `priority` present in `bundle`, or `None` when no entry is (spec §4).
pub fn choose_mime<'a>(bundle: &'a MimeBundle, priority: &[String]) -> Option<&'a str> {
    candidates(bundle, priority).next().map(|(mime, _)| mime)
}

/// Renders one MIME payload per the spec §4 table; the `bool` is true when the result needs
/// KaTeX (`text/latex`), i.e. sets `Features.math`.
///
/// `metadata` is the output's whole `metadata` object. Image and SVG sizes come from
/// `metadata[mime].width|height`, falling back to `metadata.width|height` (nbconvert's
/// `get_metadata`), and are used only when numeric and non-negative.
///
/// | MIME | HTML |
/// |---|---|
/// | `text/html` | the payload verbatim |
/// | `image/svg+xml` | `<figure class="output-image">` + the payload, with `width`/`height` set on its root `<svg>` start tag when known |
/// | `image/png`, `image/jpeg`, `image/gif`, `image/webp` | base64-decoded (ASCII whitespace ignored, padding optional), registered as `output.<ext>`: `<img class="output-image" src="asset:<id>" alt="">` + `width`/`height` |
/// | `text/latex` | one surrounding `$$…$$`, `$…$`, `\[…\]` or `\(…\)` stripped, trimmed: `<div data-math-style="display">` + escaped TeX |
/// | `application/json` | `<pre class="output-json">` + pretty-printed, escaped JSON |
/// | `text/plain` | `<pre class="output-text">` + [`ansi_to_html()`] of the text (escaped; ANSI colors kept, as nbconvert does) |
///
/// `text/markdown` needs the Markdown renderer, which only [`render_outputs`] has. URLs in
/// `text/html` are not rewritten (see the module docs).
///
/// # Errors
/// [`Error::Other`] for a payload that is not text (a string or list of strings) where text
/// is required, invalid or empty base64, `text/markdown`, and any MIME type not in the table.
pub fn render_display(
    mime: &str,
    payload: &Value,
    metadata: &Value,
    assets: &mut AssetRegistry,
) -> Result<(String, bool)> {
    let html = match mime {
        "text/html" => payload_text(payload)?,
        "image/svg+xml" => svg_html(&payload_text(payload)?, metadata, mime),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => {
            image_html(mime, &payload_text(payload)?, metadata, assets)?
        }
        "text/latex" => {
            let tex = strip_math_delimiters(&payload_text(payload)?).to_owned();
            let html = format!(
                "<div data-math-style=\"display\">{}</div>",
                html_escape::encode_text(&tex)
            );
            return Ok((html, true));
        }
        "application/json" => format!(
            "<pre class=\"output-json\">{}</pre>",
            html_escape::encode_text(&serde_json::to_string_pretty(payload)?)
        ),
        "text/plain" => format!(
            "<pre class=\"output-text\">{}</pre>",
            ansi_to_html(&payload_text(payload)?)
        ),
        "text/markdown" => {
            return Err(Error::Other(
                "text/markdown needs the Markdown renderer (render_outputs' `md`)".to_owned(),
            ));
        }
        _ => return Err(Error::Other(format!("no renderer for {mime}"))),
    };
    Ok((html, false))
}

/// The text of a payload (a string, or a list of strings joined as-is).
fn payload_text(payload: &Value) -> Result<String> {
    mime_text(payload)
        .ok_or_else(|| Error::Other("payload is not a string or a list of strings".to_owned()))
}

/// Decodes a base64 image, registers it and returns its `<img>` tag.
fn image_html(
    mime: &str,
    base64: &str,
    metadata: &Value,
    assets: &mut AssetRegistry,
) -> Result<String> {
    // Jupyter wraps base64 payloads in lines (a trailing "\n", or a list of lines).
    let compact: Vec<u8> = base64
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if compact.is_empty() {
        return Err(Error::Other("empty base64 payload".to_owned()));
    }
    let bytes = STANDARD_PAD_INDIFFERENT
        .decode(&compact)
        .map_err(|err| Error::Other(format!("invalid base64: {err}")))?;
    let ext = match mime {
        "image/jpeg" => "jpg",
        other => other.trim_start_matches("image/"),
    };
    let id = assets.register_bytes(bytes, mime, &format!("output.{ext}"));
    let mut html = format!(
        "<img class=\"output-image\" src=\"{}\" alt=\"\"",
        AssetRegistry::placeholder(&id)
    );
    for key in ["width", "height"] {
        if let Some(value) = dimension(metadata, mime, key) {
            html.push_str(&format!(" {key}=\"{value}\""));
        }
    }
    html.push('>');
    Ok(html)
}

/// `metadata[mime][key]`, else `metadata[key]`, as attribute text when it is a non-negative
/// number (`64`, `64.5`); `None` otherwise.
fn dimension(metadata: &Value, mime: &str, key: &str) -> Option<String> {
    let value = metadata
        .get(mime)
        .and_then(|per_mime| per_mime.get(key))
        .filter(|value| !value.is_null())
        .or_else(|| metadata.get(key))?;
    if let Some(integer) = value.as_u64() {
        return Some(integer.to_string());
    }
    let number = value.as_f64()?;
    // `Display` for f64 prints integral values without a fraction ("64", not "64.0").
    (number.is_finite() && number >= 0.0).then(|| number.to_string())
}

/// The SVG wrapped in `<figure class="output-image">`, with metadata sizes on the root tag.
fn svg_html(svg: &str, metadata: &Value, mime: &str) -> String {
    let sizes: Vec<(&str, String)> = ["width", "height"]
        .into_iter()
        .filter_map(|key| dimension(metadata, mime, key).map(|value| (key, value)))
        .collect();
    let svg = if sizes.is_empty() {
        Cow::Borrowed(svg)
    } else {
        set_root_svg_attrs(svg, &sizes)
    };
    format!("<figure class=\"output-image\">{svg}</figure>")
}

/// The root `<svg` start tag at the start of the haystack (see [`root_tag_start`]), parsed
/// quote-aware: group `attrs` is its attribute list, the rest is `\s*/?>`.
static SVG_ROOT_TAG: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(concat!(
        r#"\A<svg(?P<attrs>(?:\s+[^\s=/>"']+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+))?)*)"#,
        r"\s*/?>",
    ))
    .expect("SVG root tag regex is valid")
});

/// One attribute inside [`SVG_ROOT_TAG`]'s `attrs`, with its leading whitespace.
static SVG_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r#"\s+(?P<name>[^\s=/>"']+)(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+))?"#)
        .expect("SVG attribute regex is valid")
});

/// Where the root element's start tag begins: the first `<` that does not open a comment
/// (`<!-- … -->`), a processing instruction (`<?xml … ?>`) or a declaration (`<!DOCTYPE …>`,
/// internal subset included). `None` when one of those is not terminated or no tag follows.
fn root_tag_start(svg: &str) -> Option<usize> {
    let mut at = 0;
    loop {
        at += svg[at..].find('<')?;
        let rest = &svg[at..];
        let skipped = if let Some(comment) = rest.strip_prefix("<!--") {
            comment
                .find("-->")
                .map(|end| "<!--".len() + end + "-->".len())
        } else if rest.starts_with("<?") {
            rest.find("?>").map(|end| end + "?>".len())
        } else if rest.starts_with("<!") {
            declaration_len(rest)
        } else {
            return Some(at);
        };
        at += skipped?;
    }
}

/// The length of the `<!…>` declaration at the start of `text`: up to the first `>` outside
/// quotes and outside an internal subset (`[…]`); `None` when it is not terminated.
fn declaration_len(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    for (at, byte) in text.bytes().enumerate() {
        match (quote, byte) {
            (Some(open), _) if byte == open => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'[') => depth += 1,
            (None, b']') => depth = depth.saturating_sub(1),
            (None, b'>') if depth == 0 => return Some(at + 1),
            (None, _) => {}
        }
    }
    None
}

/// Sets (replaces or appends) `attrs` on the root `<svg>` start tag. Best effort: when the
/// root element is not an `<svg>` start tag or that tag is not well-formed, the SVG is
/// returned unchanged.
fn set_root_svg_attrs<'a>(svg: &'a str, attrs: &[(&str, String)]) -> Cow<'a, str> {
    let Some(start) = root_tag_start(svg) else {
        return Cow::Borrowed(svg);
    };
    let tail = &svg[start..];
    let Some(tag) = SVG_ROOT_TAG.captures(tail) else {
        return Cow::Borrowed(svg);
    };
    let (Some(whole), Some(list)) = (tag.get(0), tag.name("attrs")) else {
        return Cow::Borrowed(svg);
    };
    let mut pending: Vec<&(&str, String)> = attrs.iter().collect();
    let mut rebuilt = String::with_capacity(whole.len() + 32);
    rebuilt.push_str("<svg");
    for attr in SVG_ATTR.captures_iter(list.as_str()) {
        let name = attr.name("name").map_or("", |name| name.as_str());
        match pending.iter().position(|(key, _)| *key == name) {
            Some(at) => {
                let (key, value) = pending.remove(at);
                rebuilt.push_str(&format!(" {key}=\"{value}\""));
            }
            None => rebuilt.push_str(attr.get(0).map_or("", |all| all.as_str())),
        }
    }
    for (key, value) in pending {
        rebuilt.push_str(&format!(" {key}=\"{value}\""));
    }
    rebuilt.push_str(&whole.as_str()[list.end()..]);
    let mut out = String::with_capacity(svg.len() + 32);
    out.push_str(&svg[..start]);
    out.push_str(&rebuilt);
    out.push_str(&tail[whole.end()..]);
    Cow::Owned(out)
}

/// Trims `tex` and strips one surrounding `$$…$$`, `\[…\]`, `\(…\)` or `$…$` pair, then trims
/// again. A pair is only stripped around non-blank content; a `$$` pair only when the inside
/// has no unescaped `$$` (`$$a$$ and $$b$$` stays whole, `$$\text{for all $x$}$$` is one
/// formula), and a `$` pair only when the inside has no unescaped `$` at all (`$a$ and $b$`
/// stays whole).
fn strip_math_delimiters(tex: &str) -> &str {
    let tex = tex.trim();
    for (open, close) in [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)"), ("$", "$")] {
        let Some(inner) = tex
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
            .map(str::trim)
        else {
            continue;
        };
        let dollars = unescaped_dollars(inner);
        let ambiguous = match open {
            "$$" => dollars.windows(2).any(|pair| pair[1] == pair[0] + 1),
            "$" => !dollars.is_empty(),
            _ => false,
        };
        if inner.is_empty() || ambiguous {
            continue;
        }
        return inner;
    }
    tex
}

/// The byte positions of the `$` in `text` that a backslash does not escape.
fn unescaped_dollars(text: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        match c {
            '$' if !escaped => positions.push(at),
            '\\' => {
                escaped = !escaped;
                continue;
            }
            _ => {}
        }
        escaped = false;
    }
    positions
}

/// nbconvert's carriage-return rule (`CoalesceStreamsPreprocessor`, regex `.*\r(?=[^\n])`):
/// on each line, everything up to and including the last `\r` that is followed by more
/// text of that line is dropped, so progress bars show their final state.
fn apply_carriage_returns(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        // `\r` is ASCII, so `at + 1` is a char boundary.
        let keep_from = content.as_bytes()[..content.len().saturating_sub(1)]
            .iter()
            .rposition(|&byte| byte == b'\r')
            .map_or(0, |at| at + 1);
        out.push_str(&line[keep_from..]);
    }
    Cow::Owned(out)
}

/// The byte that starts every escape sequence.
const ESC: u8 = 0x1b;

/// Escapes `text` for HTML and turns ANSI SGR codes (colors, bold, …) into `<span>`/`<b>`/…
/// tags via `ansi_to_html::convert`, which escapes `& < > " '` itself.
///
/// The converter passes sequences it does not parse through as text, `ESC` byte included, so
/// the text is cleaned first. SGR sequences are rewritten into the form the converter parses
/// (an empty parameter becomes `0`, the colon forms `38:5:n`/`38:2:r:g:b` (also `48`) and
/// `4:n` become their `;` equivalents) and are dropped one by one when it rejects them (an
/// 8-bit color above 255, SGR 56, private `ESC [>…m`), keeping the colors around them. Every
/// other escape sequence (cursor movement, erase line, OSC hyperlinks and titles, character
/// sets, two-byte escapes such as `ESC 7`) and every stray `ESC` is dropped, as nbconvert
/// drops them. The result never contains an `ESC` byte.
///
/// Text without an ANSI colour of its own on an 8-bit or RGB background (`48;5;n` with
/// n > 15, `48;2;r;g;b`, which the converter writes as hex values that CSS cannot read) is
/// given `color:#000` or `color:#fff`, whichever has the higher contrast with it; the 16
/// four-bit colours are `var(--red,#a00)` spans the theme styles.
///
/// Never fails: should the converter still return an error, the text is rendered without
/// styling, escaped the same way (`& < > " '`).
pub fn ansi_to_html(text: &str) -> String {
    let html = match ::ansi_to_html::convert(&sanitize_ansi(text, true)) {
        Ok(html) => readable_text_on_hex_backgrounds(html),
        Err(err) => {
            tracing::debug!(%err, "ANSI conversion failed; rendering the text without colors");
            ::ansi_to_html::Esc(sanitize_ansi(text, false)).to_string()
        }
    };
    // Unreachable (the converter only sees sequences it accepts); a cheap last guard.
    if html.contains('\u{1b}') {
        html.replace('\u{1b}', "")
    } else {
        html
    }
}

/// The converter's HTML with a black or white text colour for text without an ANSI colour
/// on an 8-bit or RGB background.
///
/// The converter writes the 16 four-bit colours as `var(--red,#a00)`, which a theme sets for
/// text and backgrounds, but 8-bit and RGB colours (`48;5;n` with n > 15, `48;2;r;g;b`) as
/// hex values (`background:#eee`), which CSS cannot read. On such a background, text without
/// an ANSI colour would keep the slide's text colour, which may not read there (white text on
/// `48;5;231`), so it gets `#000` or `#fff`, whichever has the higher contrast (WCAG): a
/// `background:#…` span gets `;color:…` appended, and reverse video, which writes its default
/// text as `color:var(--black,#000)` in the span of its background (`38;5;100;7`), gets the
/// colour in its place. Text in an ANSI colour, of the span or an enclosing one, keeps it.
///
/// That reverse span's `color:#…` is the colour notebook.css passes down to nested spans in
/// `--ansi-fg` (it takes every span that starts with `color:#` for an 8-bit text colour), so
/// a four-bit background opened inside it without a colour of its own (`38;5;17;7`, then `33`:
/// reverse video draws the new text colour as the background) would get the white or black
/// chosen for the hex background: white on yellow. Such a span gets `;--ansi-fg:initial`, so
/// its text is the theme's black or white for its own background, as in a terminal
/// (audit L-W6-1).
///
/// The converter's tags nest (text is escaped, so every `<` starts a tag), so a stack of the
/// open tags tells whether the text has an ANSI colour and whether it is inside such a
/// reverse span.
fn readable_text_on_hex_backgrounds(html: String) -> String {
    if !html.contains("background:#") {
        return html;
    }
    let mut out = String::with_capacity(html.len() + 32);
    // For each open tag: the text's state inside it.
    let mut open: Vec<TextState> = Vec::new();
    let mut rest = html.as_str();
    while let Some(lt) = rest.find('<') {
        let gt = rest[lt..].find('>').map_or(rest.len(), |at| lt + at + 1);
        out.push_str(&rest[..lt]);
        let tag = &rest[lt..gt];
        rest = &rest[gt..];
        if tag.starts_with("</") {
            open.pop();
            out.push_str(tag);
            continue;
        }
        let parent = open.last().copied().unwrap_or_default();
        let (tag, state) = with_readable_text(tag, parent);
        out.push_str(&tag);
        open.push(state);
    }
    out.push_str(rest);
    out
}

/// The text inside an open tag, for [`readable_text_on_hex_backgrounds`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TextState {
    /// It has an ANSI colour (of its span or an enclosing one).
    coloured: bool,
    /// It is inside a reverse-video span whose default text got a `color:#…` here.
    in_hex_reverse: bool,
}

/// One start tag of the converter for [`readable_text_on_hex_backgrounds`], given the state of
/// the enclosing text: the tag, with a text colour (or a `--ansi-fg` reset) when it needs one,
/// and the state of its own text. The reversed default text (`color:var(--black,#000)` in a
/// span that also sets the background) has no ANSI colour.
fn with_readable_text(tag: &str, parent: TextState) -> (Cow<'_, str>, TextState) {
    let Some(style) = tag
        .strip_prefix("<span style='")
        .and_then(|rest| rest.strip_suffix("'>"))
    else {
        return (Cow::Borrowed(tag), parent);
    };
    let value = |property: &str| {
        style
            .split(';')
            .find_map(|declaration| declaration.strip_prefix(property)?.strip_prefix(':'))
    };
    let (colour, background) = (value("color"), value("background"));
    let reverse = colour.is_some() && background.is_some();
    let state = TextState {
        coloured: match colour {
            Some(colour) => !(reverse && colour.starts_with("var(--black,")),
            None => parent.coloured,
        },
        ..parent
    };
    let Some(rgb) = background.and_then(hex_rgb).filter(|_| !state.coloured) else {
        // A background without a colour of its own inside a hex reverse span: its text
        // must not take that span's colour (see above).
        if background.is_some() && colour.is_none() && !state.coloured && parent.in_hex_reverse {
            return (
                Cow::Owned(format!("<span style='{style};--ansi-fg:initial'>")),
                state,
            );
        }
        return (Cow::Borrowed(tag), state);
    };
    let text = readable_on(rgb);
    let background = background.unwrap_or_default();
    if reverse {
        let tag = format!("<span style='color:{text};background:{background}'>");
        let state = TextState {
            in_hex_reverse: true,
            ..state
        };
        return (Cow::Owned(tag), state);
    }
    (
        Cow::Owned(format!("<span style='{style};color:{text}'>")),
        state,
    )
}

/// The channels of a `#rgb` or `#rrggbb` colour; `None` for any other value.
fn hex_rgb(value: &str) -> Option<[u8; 3]> {
    let digits = value.strip_prefix('#')?;
    let channel = |at: usize, len: usize| {
        let byte = u8::from_str_radix(digits.get(at..at + len)?, 16).ok()?;
        Some(if len == 1 { byte * 17 } else { byte })
    };
    match digits.len() {
        3 => Some([channel(0, 1)?, channel(1, 1)?, channel(2, 1)?]),
        6 => Some([channel(0, 2)?, channel(2, 2)?, channel(4, 2)?]),
        _ => None,
    }
}

/// `#000` or `#fff`, whichever has the higher WCAG contrast ratio with `rgb`.
fn readable_on(rgb: [u8; 3]) -> &'static str {
    let linear = |channel: u8| {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = rgb.map(linear);
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    // Contrast with black, (L + 0.05) / 0.05, against contrast with white, 1.05 / (L + 0.05).
    if (luminance + 0.05) / 0.05 >= 1.05 / (luminance + 0.05) {
        "#000"
    } else {
        "#fff"
    }
}

/// `text` without escape sequences, except, when `keep_sgr`, the SGR sequences the
/// converter accepts, normalized by [`normalize_sgr`]. See [`escape_sequence`] for how a
/// sequence ends.
fn sanitize_ansi(text: &str, keep_sgr: bool) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    if !bytes.contains(&ESC) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(offset) = bytes[at..].iter().position(|&byte| byte == ESC) {
        let start = at + offset;
        out.push_str(&text[at..start]);
        let (end, sgr) = escape_sequence(text, start);
        if let Some(sequence) = sgr.filter(|_| keep_sgr).and_then(normalize_sgr) {
            out.push_str(&sequence);
        }
        at = end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// The end (exclusive) of the escape sequence that starts with the `ESC` at `start`, and the
/// parameters of a complete SGR sequence (`ESC [ params m`).
///
/// A malformed or truncated sequence ends before the first byte that does not fit it, so
/// that byte stays text; a lone `ESC` is a sequence of its own. A control string (OSC such as
/// a hyperlink or a title, DCS, …) ends after its BEL or ST (`ESC \`) terminator or before
/// any other `ESC` (which aborts it, as in a terminal); one that is still open at the end of
/// the text loses only its two-byte introducer, so a stray `ESC ]` cannot hide output.
/// Every returned index follows an ASCII byte or is the text's length: a char boundary.
fn escape_sequence(text: &str, start: usize) -> (usize, Option<&str>) {
    let bytes = text.as_bytes();
    let next = start + 1;
    match bytes.get(next).copied() {
        // CSI: parameter bytes, intermediate bytes, one final byte.
        Some(b'[') => {
            let params_end = skip_bytes(bytes, next + 1, 0x30..=0x3f);
            let end = skip_bytes(bytes, params_end, 0x20..=0x2f);
            match bytes.get(end).copied() {
                Some(b'm') if end == params_end => (end + 1, Some(&text[next + 1..params_end])),
                Some(0x40..=0x7e) => (end + 1, None),
                _ => (end, None),
            }
        }
        // Control strings: OSC, DCS, SOS, PM, APC.
        Some(b']' | b'P' | b'X' | b'^' | b'_') => (
            control_string_end(bytes, next + 1).unwrap_or(next + 1),
            None,
        ),
        // nF escapes such as `ESC ( B` (character set): intermediate bytes, one final byte.
        Some(0x20..=0x2f) => {
            let end = skip_bytes(bytes, next, 0x20..=0x2f);
            match bytes.get(end).copied() {
                Some(0x30..=0x7e) => (end + 1, None),
                _ => (end, None),
            }
        }
        // Two-byte escapes: `ESC 7`, `ESC 8`, `ESC M`, `ESC c`, …
        Some(0x30..=0x7e) => (next + 1, None),
        // A lone `ESC`: at the end of the text, or before a byte that cannot follow it.
        _ => (next, None),
    }
}

/// The index of the first byte at or after `from` that is not in `range`.
fn skip_bytes(bytes: &[u8], from: usize, range: std::ops::RangeInclusive<u8>) -> usize {
    bytes
        .get(from..)
        .and_then(|rest| rest.iter().position(|byte| !range.contains(byte)))
        .map_or(bytes.len(), |offset| from + offset)
}

/// Where a control string whose content starts at `from` ends: after a BEL or ST (`ESC \`),
/// or before another `ESC`. `None` when the text ends first.
fn control_string_end(bytes: &[u8], from: usize) -> Option<usize> {
    let offset = bytes
        .get(from..)?
        .iter()
        .position(|&byte| byte == 0x07 || byte == ESC)?;
    let at = from + offset;
    Some(match bytes[at] {
        ESC if bytes.get(at + 1) == Some(&b'\\') => at + 2,
        ESC => at,
        _ => at + 1,
    })
}

/// An SGR sequence (`ESC [ params m`) rewritten into the form `ansi-to-html` parses (numbers
/// separated by single `;`), or `None` to drop it: when it is private (a `<`, `=`, `>` or `?`
/// parameter byte, e.g. xterm's `ESC [>4;2m`), when no parameter is left, or when the
/// converter rejects one of its codes.
///
/// Rewrites: an empty parameter becomes `0` (its ECMA-48 default; one trailing `;` is ignored,
/// as the converter does); the colon forms `38:5:n` and `38:2:[cs:]r:g:b` (also `48`) become
/// their `;` forms; the underline styles `4:0` (off), `4:2` (double) and other `4:n` become
/// `24`, `21` and `4`; any other colon form is dropped.
fn normalize_sgr(params: &str) -> Option<String> {
    if params.is_empty() {
        return Some("\u{1b}[m".to_owned());
    }
    if params.bytes().any(|byte| matches!(byte, b'<'..=b'?')) {
        return None;
    }
    let params = params.strip_suffix(';').unwrap_or(params);
    let fields: Vec<Cow<'_, str>> = params
        .split(';')
        .filter_map(|field| match field {
            "" => Some(Cow::Borrowed("0")),
            _ if field.contains(':') => colon_sgr(field).map(Cow::Owned),
            _ => Some(Cow::Borrowed(field)),
        })
        .collect();
    if fields.is_empty() {
        return None;
    }
    let sequence = format!("\u{1b}[{}m", fields.join(";"));
    // Codes are parsed per sequence, so one the converter accepts alone it accepts anywhere.
    ::ansi_to_html::convert(&sequence)
        .is_ok()
        .then_some(sequence)
}

/// One SGR parameter with `:` sub-parameters in its `;` form (see [`normalize_sgr`]); `None`
/// for forms the converter has no equivalent for.
fn colon_sgr(field: &str) -> Option<String> {
    let number = |part: &&str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let parts: Vec<&str> = field.split(':').collect();
    match parts.as_slice() {
        [color @ ("38" | "48"), "5", index] if number(index) => Some(format!("{color};5;{index}")),
        [color @ ("38" | "48"), "2", .., r, g, b]
            if parts.len() <= 6 && [*r, *g, *b].iter().all(number) =>
        {
            Some(format!("{color};2;{r};{g};{b}"))
        }
        ["4", "0"] => Some("24".to_owned()),
        ["4", "2"] => Some("21".to_owned()),
        ["4", style] if number(style) => Some("4".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn carriage_returns_follow_nbconvert() {
        let cases = [
            ("plain\n", "plain\n"),
            ("10%\r50%\r100%\n", "100%\n"),
            ("a\rb\nc\rd", "b\nd"),
            // A `\r` at the end of a line (Windows line endings) is kept.
            ("x\r\ny\r\n", "x\r\ny\r\n"),
            ("a\r\r\n", "\r\n"),
            ("\rtail", "tail"),
            ("é\rü", "ü"),
        ];
        for (input, expected) in cases {
            assert_eq!(apply_carriage_returns(input), expected, "{input:?}");
        }
    }

    #[test]
    fn math_delimiters() {
        let cases = [
            ("$$x^2$$", "x^2"),
            ("  $$ x^2 $$\n", "x^2"),
            ("$x$", "x"),
            ("\\[a+b\\]", "a+b"),
            ("\\(a\\)", "a"),
            ("$\\displaystyle x$", "\\displaystyle x"),
            ("$\\$5$", "\\$5"),
            ("x^2", "x^2"),
            ("$a$ and $b$", "$a$ and $b$"),
            ("$$", "$$"),
            ("$", "$"),
            ("$$$", "$$$"),
            ("$ $", "$ $"),
            ("$$\\$$$", "\\$"),
            ("\\begin{align}a\\end{align}", "\\begin{align}a\\end{align}"),
            // A single `$` inside display math (`\text{…}` switches back to text mode).
            ("$$\\text{a $b$ c}$$", "\\text{a $b$ c}"),
            ("$$\\text{for all $x$}$$", "\\text{for all $x$}"),
            // Two display formulas, or `$$` inside: ambiguous, kept whole.
            ("$$a$$ and $$b$$", "$$a$$ and $$b$$"),
            ("$$a $$ b$$", "$$a $$ b$$"),
            // An escaped `$` next to an unescaped one is not `$$`.
            ("$$\\$$x$$", "\\$$x"),
        ];
        for (input, expected) in cases {
            assert_eq!(strip_math_delimiters(input), expected, "{input:?}");
        }
    }

    #[test]
    fn dimensions_are_numeric_only() {
        let metadata = json!({
            "image/png": { "width": 64, "height": 32.5 },
            "image/gif": { "width": "50%", "height": -3 },
            "image/webp": { "width": null },
            "width": 10
        });
        assert_eq!(
            dimension(&metadata, "image/png", "width").as_deref(),
            Some("64")
        );
        assert_eq!(
            dimension(&metadata, "image/png", "height").as_deref(),
            Some("32.5")
        );
        assert_eq!(dimension(&metadata, "image/gif", "width"), None);
        assert_eq!(dimension(&metadata, "image/gif", "height"), None);
        // Absent or null per-MIME values fall back to the top level (nbconvert).
        assert_eq!(
            dimension(&metadata, "image/webp", "width").as_deref(),
            Some("10")
        );
        assert_eq!(
            dimension(&metadata, "image/jpeg", "width").as_deref(),
            Some("10")
        );
        assert_eq!(dimension(&metadata, "image/jpeg", "height"), None);
        assert_eq!(dimension(&json!(64.0), "image/png", "width"), None);
        assert_eq!(
            dimension(&json!({"width": 64.0}), "image/png", "width").as_deref(),
            Some("64")
        );
    }

    fn sizes(width: &str, height: &str) -> Vec<(&'static str, String)> {
        vec![("width", width.to_owned()), ("height", height.to_owned())]
    }

    #[test]
    fn root_svg_attrs_are_replaced_or_appended() {
        let cases = [
            (
                r#"<svg viewBox="0 0 1 1"><rect width="1" height="1"/></svg>"#,
                r#"<svg viewBox="0 0 1 1" width="12" height="6"><rect width="1" height="1"/></svg>"#,
            ),
            (
                "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\">\n<svg\n  width=\"460.8pt\" height='345.6pt'\n  stroke-width=\"2\" style=\"a width=3\">x</svg>",
                "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\">\n<svg width=\"12\" height=\"6\"\n  stroke-width=\"2\" style=\"a width=3\">x</svg>",
            ),
            ("<svg/>", r#"<svg width="12" height="6"/>"#),
            ("<svg>", r#"<svg width="12" height="6">"#),
            (
                "<svg data-x=\"a>b\" width=1 >",
                "<svg data-x=\"a>b\" width=\"12\" height=\"6\" >",
            ),
            // An `<svg` inside a leading comment, PI or doctype subset is not the root.
            (
                "<!-- <svg width=\"1\"> --><svg viewBox=\"0 0 1 1\"></svg>",
                "<!-- <svg width=\"1\"> --><svg viewBox=\"0 0 1 1\" width=\"12\" height=\"6\"></svg>",
            ),
            (
                "<?xml version=\"1.0\"?><?note <svg x=\"1\"> ?>\n<!DOCTYPE svg [\n  <!ENTITY e \"<svg a='>'>\">\n]>\n<!-- Created with matplotlib -->\n<svg>",
                "<?xml version=\"1.0\"?><?note <svg x=\"1\"> ?>\n<!DOCTYPE svg [\n  <!ENTITY e \"<svg a='>'>\">\n]>\n<!-- Created with matplotlib -->\n<svg width=\"12\" height=\"6\">",
            ),
            // Text before the root element is skipped (a byte-order mark, stray words).
            (
                "\u{feff}hi <svg>",
                "\u{feff}hi <svg width=\"12\" height=\"6\">",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                set_root_svg_attrs(input, &sizes("12", "6")),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn malformed_svg_is_left_alone() {
        for input in [
            "no svg here",
            "<svgx width=\"1\">",
            "<svg width=\"1></svg>",
            "",
            // The root element is not an `<svg>`: nested ones are left alone.
            "<div><svg width=\"1\"></svg></div>",
            // An unterminated comment, PI or doctype hides everything after it.
            "<!-- <svg>",
            "<?xml <svg>",
            "<!DOCTYPE svg [ <svg>",
        ] {
            assert_eq!(
                set_root_svg_attrs(input, &sizes("12", "6")),
                input,
                "{input}"
            );
        }
    }

    #[test]
    fn escape_sequences_end_where_terminals_end_them() {
        let cases = [
            // (text starting with ESC, end, SGR parameters)
            ("\u{1b}[31mx", 5, Some("31")),
            ("\u{1b}[mx", 3, Some("")),
            ("\u{1b}[;4:3mx", 7, Some(";4:3")),
            ("\u{1b}[>4;2mx", 7, Some(">4;2")),
            ("\u{1b}[2Kx", 4, None),
            ("\u{1b}[?25lx", 6, None),
            ("\u{1b}[1 qx", 5, None),
            // Truncated CSI: up to the first byte that does not fit.
            ("\u{1b}[31", 4, None),
            ("\u{1b}[31\nx", 4, None),
            ("\u{1b}[31é", 4, None),
            // Control strings.
            ("\u{1b}]0;title\u{7}x", 10, None),
            ("\u{1b}]8;;http://x\u{1b}\\x", 15, None),
            ("\u{1b}]8;;é\u{1b}[31m", 7, None),
            ("\u{1b}Pdata\u{1b}\\x", 8, None),
            ("\u{1b}]8;;open", 2, None),
            // nF, two-byte escapes, lone ESC.
            ("\u{1b}(Bx", 3, None),
            ("\u{1b}(", 2, None),
            ("\u{1b}7x", 2, None),
            ("\u{1b}Mx", 2, None),
            ("\u{1b}", 1, None),
            ("\u{1b}\u{1b}[31m", 1, None),
            ("\u{1b}éx", 1, None),
        ];
        for (text, end, sgr) in cases {
            assert_eq!(escape_sequence(text, 0), (end, sgr), "{text:?}");
            assert!(text.is_char_boundary(end), "{text:?}");
        }
    }

    #[test]
    fn sgr_parameters_are_normalized_or_dropped() {
        let cases = [
            ("", Some("\u{1b}[m")),
            ("31", Some("\u{1b}[31m")),
            ("0;", Some("\u{1b}[0m")),
            (";31", Some("\u{1b}[0;31m")),
            ("1;;31", Some("\u{1b}[1;0;31m")),
            (";", Some("\u{1b}[0m")),
            ("38:5:241", Some("\u{1b}[38;5;241m")),
            ("1;48:2::10:20:30", Some("\u{1b}[1;48;2;10;20;30m")),
            ("38:2:10:20:30", Some("\u{1b}[38;2;10;20;30m")),
            ("4:3", Some("\u{1b}[4m")),
            ("4:2", Some("\u{1b}[21m")),
            ("4:0", Some("\u{1b}[24m")),
            ("1;58:5:3", Some("\u{1b}[1m")),
            // Nothing left, private, or rejected by the converter.
            ("58:5:3", None),
            ("38:5:", None),
            ("38:2:1:2:3:4:5", None),
            (">4;2", None),
            ("?1", None),
            ("38;5;999", None),
            ("56", None),
            ("1;300", None),
        ];
        for (params, expected) in cases {
            assert_eq!(normalize_sgr(params).as_deref(), expected, "{params:?}");
        }
    }

    #[test]
    fn sanitizing_keeps_text_and_accepted_sgr_only() {
        let text = "a\u{1b}[2Kb\u{1b}[;31mc\u{1b}]8;;u\u{7}d\u{1b}7e\u{1b}[56mf\u{1b}";
        assert_eq!(sanitize_ansi(text, true), "ab\u{1b}[0;31mcdef");
        assert_eq!(sanitize_ansi(text, false), "abcdef");
        assert!(matches!(
            sanitize_ansi("plain", true),
            Cow::Borrowed("plain")
        ));
    }

    /// Text without an ANSI colour on an 8-bit or RGB background, also reverse video's
    /// default text, is black or white by the background's luminance; text in an ANSI colour
    /// keeps it, and four-bit backgrounds are left to the theme (notebook.css).
    #[test]
    fn default_text_on_hex_backgrounds_is_black_or_white() {
        let html = |sgr: &str| ansi_to_html(&format!("\u{1b}[{sgr}mx"));
        let cases = [
            (
                "48;5;255",
                "<span style='background:#eee;color:#000'>x</span>",
            ),
            (
                "48;5;17",
                "<span style='background:#00005f;color:#fff'>x</span>",
            ),
            (
                "48;5;22",
                "<span style='background:#005f00;color:#fff'>x</span>",
            ),
            // Monokai's background, as rich writes it.
            (
                "48;2;39;40;34",
                "<span style='background:#272822;color:#fff'>x</span>",
            ),
            (
                "48;2;255;255;200",
                "<span style='background:#ffffc8;color:#000'>x</span>",
            ),
            // Reverse video of an 8-bit or RGB text colour: the default text on it.
            (
                "38;5;100;7",
                "<span style='color:#878700'><span style='color:#000;background:#878700'>x\
                 </span></span>",
            ),
            (
                "38;5;241;7",
                "<span style='color:#626262'><span style='color:#fff;background:#626262'>x\
                 </span></span>",
            ),
            (
                "38;2;200;100;0;7",
                "<span style='color:#c86400'><span style='color:#000;background:#c86400'>x\
                 </span></span>",
            ),
            // Reverse video, then an 8-bit text colour (written as the background).
            (
                "7;38;5;17",
                "<span style='color:var(--black,#000);background:var(--bright-white,#fff)'>\
                 <span style='background:#00005f;color:#fff'>x</span></span>",
            ),
            // Text in an ANSI colour, of the span or an enclosing one, keeps it.
            (
                "31;48;5;255",
                "<span style='color:var(--red,#a00)'><span style='background:#eee'>x</span>\
                 </span>",
            ),
            (
                "38;5;100;48;2;0;0;0",
                "<span style='color:#878700'><span style='background:#000000'>x</span></span>",
            ),
            (
                "48;5;17;30",
                "<span style='background:#00005f;color:#fff'><span style='color:var(--black,\
                 #000)'>x</span></span>",
            ),
            (
                "41;38;5;100;7",
                "<span style='background:var(--red,#a00)'><span style='color:#878700'><span \
                 style='color:var(--red,#a00);background:#878700'>x</span></span></span>",
            ),
            // Four-bit backgrounds, also `48;5;n` with n < 16, are the theme's.
            ("41", "<span style='background:var(--red,#a00)'>x</span>"),
            (
                "48;5;1",
                "<span style='background:var(--red,#a00)'>x</span>",
            ),
            (
                "7",
                "<span style='color:var(--black,#000);background:var(--bright-white,#fff)'>x\
                 </span>",
            ),
        ];
        for (sgr, expected) in cases {
            assert_eq!(html(sgr), expected, "SGR {sgr}");
        }
        // A colour that ends gives the text back to the background.
        assert_eq!(
            ansi_to_html("\u{1b}[31;48;5;17mx\u{1b}[39my"),
            "<span style='color:var(--red,#a00)'><span style='background:#00005f'>x</span>\
             </span><span style='background:#00005f;color:#fff'>y</span>"
        );
        assert_eq!(hex_rgb("#c8f"), Some([0xcc, 0x88, 0xff]));
        assert_eq!(hex_rgb("#c86400"), Some([0xc8, 0x64, 0x00]));
        assert_eq!(hex_rgb("var(--red,#a00)"), None);
        assert_eq!(hex_rgb("#c8"), None);
        assert_eq!(hex_rgb("#gg0"), None);
        // Black and white change at a luminance of about 0.18 (#767676 is 0.181).
        assert_eq!(readable_on([0x76; 3]), "#000");
        assert_eq!(readable_on([0x75; 3]), "#fff");
    }

    /// Audit L-W6-1: after reverse video of an 8-bit or RGB colour, a four-bit colour draws a
    /// four-bit background; its text must be the theme's black or white for that background,
    /// not the hex reverse span's colour, which notebook.css passes down in `--ansi-fg`.
    #[test]
    fn four_bit_backgrounds_inside_hex_reverse_video_choose_their_own_text() {
        assert_eq!(
            ansi_to_html("\u{1b}[38;5;17;7m rev navy \u{1b}[33m then 33 \u{1b}[0m"),
            "<span style='color:#00005f'><span style='color:#fff;background:#00005f'> rev navy \
             <span style='background:var(--yellow,#a60);--ansi-fg:initial'> then 33 </span>\
             </span></span>"
        );
        let html = ansi_to_html("\u{1b}[38;5;100;7mx\u{1b}[34my\u{1b}[97mz");
        assert!(
            html.contains("<span style='background:var(--blue,#00a);--ansi-fg:initial'>y"),
            "{html}"
        );
        assert!(
            html.contains("<span style='background:var(--bright-white,#fff);--ansi-fg:initial'>z"),
            "{html}"
        );
        // Text with an ANSI colour of its own keeps it; four-bit reverse video and spans
        // outside a hex reverse span are unchanged.
        for sgr in ["38;5;241;43", "31;7", "7", "41"] {
            assert!(!html_of(sgr).contains("--ansi-fg"), "{sgr}");
        }
        let coloured = ansi_to_html("\u{1b}[38;5;17;7mx\u{1b}[43my");
        assert!(!coloured.contains("--ansi-fg"), "{coloured}");
    }

    fn html_of(sgr: &str) -> String {
        ansi_to_html(&format!("\u{1b}[{sgr}mx\u{1b}[33my"))
    }
}
