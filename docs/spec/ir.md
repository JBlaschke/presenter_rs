# Spec: Intermediate representation (IR)

**Status:** contract. Owned by T01. Changes require a "contract change" note and an
update to `tests/fixtures/ir/sample-deck.json`.

The IR is the only thing frontends (Markdown, notebook) produce and the only thing the
renderer consumes. It is plain data with `serde` derives; its JSON form is what
`presenter dump` prints and what the template context embeds under `deck`.

## 1. Types (`src/ir.rs`, authoritative)

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A whole presentation. One source file produces one `Deck`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deck {
    pub meta: DeckMeta,
    /// Top-level (horizontal) slides, in order.
    pub slides: Vec<Slide>,
    /// Which optional front-end assets the output needs (drives KaTeX inclusion etc.).
    pub features: Features,
    pub source: SourceInfo,
    /// Summary of registered assets without their bytes (for `dump` and `check`).
    pub assets: Vec<AssetInfo>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeckMeta {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
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
    pub blocks: Vec<Block>,
    /// Speaker notes, already rendered to HTML. `None` when absent.
    pub notes: Option<String>,
    /// Vertical sub-slides. Only top-level slides may have children; children never do.
    pub children: Vec<Slide>,
    pub span: Option<SourceSpan>,
}

impl Default for Slide {
    fn default() -> Self {
        Slide { id: None, layout: "default".to_string(), classes: vec![], attrs: BTreeMap::new(),
                title: None, blocks: vec![], notes: None, children: vec![], span: None }
    }
}

/// Ordered content of a slide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Rendered HTML (from Markdown). Image URLs inside use the `asset:<id>` scheme (§3).
    Html { html: String },
    /// A notebook code cell with its outputs. Rendered by the theme's `cell.html`.
    Cell(NotebookCell),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotebookCell {
    /// 0-based index of the cell in the notebook (counting all cells, including skipped ones).
    pub index: usize,
    pub id: Option<String>,
    /// Language for highlight.js, e.g. `"python"`; from `metadata.language_info.name`,
    /// else `metadata.kernelspec.language`, else `"text"`.
    pub language: String,
    /// Raw source (not escaped; templates auto-escape).
    pub source: String,
    pub execution_count: Option<i64>,
    pub outputs: Vec<CellOutput>,
    /// From tags `hide-input`/`remove-input` or `metadata.jupyter.source_hidden`.
    pub hide_input: bool,
    /// From tags `hide-output`/`remove-output` or `metadata.jupyter.outputs_hidden`.
    pub hide_output: bool,
    /// `slideshow.slide_type == "fragment"`. The template wraps the cell in `.fragment`.
    pub fragment: bool,
    pub tags: Vec<String>,
    /// `cell.metadata` verbatim (a JSON object; `Value::Null` if absent).
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellOutput {
    /// stdout/stderr. `html` is escaped text with ANSI colors converted to `<span>`s, no `<pre>`.
    Stream { name: String, html: String },
    /// An exception. `html` is the joined traceback, escaped, ANSI converted, no `<pre>`.
    Error { ename: String, evalue: String, html: String },
    /// `display_data` or `execute_result`, reduced to one MIME type (see notebook-mapping.md §4).
    /// `html` is a complete, insertable fragment (`<img>`, raw HTML, `<pre>`, math span, …).
    /// `execution_count` is `Some` only for `execute_result`.
    Display { mime: String, html: String, execution_count: Option<i64> },
    /// No renderable MIME type was present (e.g. only a widget view).
    Unsupported { mimes: Vec<String> },
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Absolute path of the source file.
    pub path: PathBuf,
    pub kind: SourceKind,
    /// Directory relative paths (images, includes) resolve against. Normally `path.parent()`.
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind { Markdown, Notebook }

/// 1-based inclusive line range. For notebooks, lines are within the cell's source and
/// `cell` is the cell index; for Markdown `cell` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan { pub start_line: usize, pub end_line: usize, pub cell: Option<usize> }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInfo {
    pub id: String,
    pub mime: String,
    /// `"file:<path relative to base_dir>"` or `"inline:<byte length>"`.
    pub origin: String,
}
```

## 2. Invariants (tests in T01 enforce the checkable ones)

1. `Deck.slides[i].children[j].children` is always empty (reveal supports two levels).
2. `Slide.attrs` never contains the keys `id`, `class`, `layout`, `style`-less duplicates of
   `classes`; keys are lowercase; shorthand keys are already expanded (dialect §3.2).
3. `Slide.layout` is non-empty. Unknown layouts are allowed here; the renderer falls back
   to `layouts/default.html` and `check` warns.
4. All URLs in `Block::Html` and `CellOutput::Display.html` that point at local files use
   the `asset:<id>` scheme (§3). Remote URLs (`http:`, `https:`, `//`, `data:`, `mailto:`)
   and root-absolute paths (`/x.png`) are left untouched.
5. `Slide.notes`, `Block::Html.html`, `CellOutput::*.html` are HTML, never Markdown.
6. `Features` is derived from content, never set by configuration. Configuration may *add*
   assets (e.g. force KaTeX) at render time, not here.
7. JSON serialization is stable: `cargo insta` snapshots of fixtures must not change when
   unrelated code changes. Use `BTreeMap`, never `HashMap`, in the IR.
8. Line numbers in `SourceSpan` are 1-based, as editors show them.

## 3. Asset placeholders and the registry (`src/assets/registry.rs`)

> **Contract change (coordinator, 2026-10-08):** the core registry API below is implemented
> by **T01** (so wave-2 frontends can register assets without waiting for T14). T14 then owns
> the file and adds `merge`, `len`, `is_empty`, `read` and everything in `src/assets/emit.rs`.

Frontends never read image bytes or decide URLs. They *register* assets and write a
placeholder; the emitter later rewrites placeholders to `data:` URIs (standalone) or
relative paths (site mode).

```rust
pub struct AssetRegistry { /* BTreeMap<String, Asset> */ }

pub struct Asset {
    pub id: String,                // 12 lowercase hex chars
    pub mime: String,              // e.g. "image/png"
    pub data: AssetData,
    /// Preferred output file name in site mode, e.g. "plot.png" or "output.png".
    pub file_name_hint: String,
}
pub enum AssetData { File(PathBuf) /* absolute */, Bytes(Vec<u8>) }

impl AssetRegistry {
    pub fn new() -> Self;
    /// Registers a file by absolute path. id = first 12 hex chars of blake3(path as UTF-8).
    /// MIME from the extension via `mime_guess`, default "application/octet-stream".
    /// Does NOT read the file. Returns the id.
    pub fn register_file(&mut self, abs_path: &Path) -> String;
    /// Registers in-memory bytes. id = first 12 hex chars of blake3(bytes).
    pub fn register_bytes(&mut self, bytes: Vec<u8>, mime: &str, file_name_hint: &str) -> String;
    pub fn get(&self, id: &str) -> Option<&Asset>;
    pub fn iter(&self) -> impl Iterator<Item = &Asset>;
    pub fn infos(&self, base_dir: &Path) -> Vec<AssetInfo>;   // fills Deck.assets
    pub fn placeholder(id: &str) -> String { format!("asset:{id}") }
}
```

Placeholder grammar: `asset:` followed by exactly 12 `[0-9a-f]`. The emitter replaces
every occurrence inside attribute values of the rendered document (`src`, `href`,
`data-background-image`, `data-background-video`, `poster`, `srcset`, inline `style`
`url(...)`) and inside compiled CSS `url(...)`. Unresolvable placeholders are a build error.
Text content is never rewritten: a literal `asset:<12 hex>` in prose or code is left alone
(audit F16). In site mode the emitted file name uses a hash of the asset's **bytes**, not its
id, so outputs do not depend on the checkout location (audit F20, PLAN goal 7).

## 4. Frontend interface (implemented by T12 and T31; called by T16's pipeline)

```rust
/// Output of any frontend. Defined in `src/ir.rs` (T01).
pub struct Parsed { pub deck: Deck, pub assets: AssetRegistry, pub diagnostics: Diagnostics }

// src/markdown/mod.rs
pub fn parse(source: &SourceInfo, text: &str, config: &Config) -> crate::error::Result<Parsed>;
// src/notebook/mod.rs
pub fn parse(source: &SourceInfo, json: &str, config: &Config) -> crate::error::Result<Parsed>;
```

Both must be pure functions of their inputs (no reading other files except that the
Markdown frontend may `stat` referenced images to decide between registering them and
reporting `MissingAsset` — see dialect §6).

> **Contract change (coordinator, 2026-10-08):**
> - `Parsed` carries the frontend's `Diagnostics` (`crate::error::Diagnostics`): `empty-slide`,
>   `orphan-vertical`, `unknown-slide-attr`, `missing-asset`, notebook warnings, … The pipeline
>   and `check` read them from here.
> - Frontends **never fail** on a missing local asset: they record a `Level::Warning`
>   diagnostic with code `missing-asset` (path + span) and leave the URL as written. The
>   pipeline (`pipeline::build_deck`, T16) turns the first such diagnostic into
>   `Error::MissingAsset` when `BuildOptions.missing_asset_is_error` is set (`build`), keeps it
>   a warning in `serve`, and `check` reports it as a lint.
> - Front matter: frontends apply `Config::merge_front_matter(&deck.meta)` to a *clone* of the
>   config they were given (so e.g. `slides.split` from front matter affects splitting). The
>   pipeline applies it again to its own config after parsing; the call is idempotent.

## 5. Error type (`src/error.rs`, owned by T01)

```rust
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{}:{} {message}", .path.display(), .line.map(|l| l.to_string()).unwrap_or_default())]
    Parse { path: PathBuf, line: Option<usize>, message: String },
    #[error("front matter: {0}")]
    FrontMatter(String),
    #[error("config: {0}")]
    Config(String),
    #[error("theme '{name}': {message}")]
    Theme { name: String, message: String },
    #[error("template '{template}'{}: {message}", .line.map(|l| format!(" line {l}")).unwrap_or_default())]
    Template { template: String, line: Option<usize>, message: String },
    #[error("style '{file}': {message}")]
    Style { file: String, message: String },
    #[error("asset not found: {}", .path.display())]
    MissingAsset { path: PathBuf, span: Option<SourceSpan> },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}
pub type Result<T> = std::result::Result<T, Error>;
```

Note the thiserror conventions: `PathBuf` is not `Display`, so paths go through
`.path.display()` as an extra format argument, and fields used inside expressions are
written with the leading-dot shorthand (`.line`).

`From<minijinja::Error>` and `From<toml::de::Error>` conversions live in `error.rs` too
(T13 and T01 add them respectively) and must preserve line information where available.

## 6. JSON example (`tests/fixtures/ir/sample-deck.json` is the canonical, larger version)

```json
{
  "meta": { "title": "Rust for data people", "author": "J. Blaschke", "lang": "en",
            "theme": "./theme", "reveal": { "transition": "fade" }, "vars": { "venue": "PyData" } },
  "slides": [
    { "id": null, "layout": "title", "classes": [], "attrs": {}, "title": "Rust for data people",
      "blocks": [ { "type": "html", "html": "<h1>Rust for data people</h1>" } ],
      "notes": null, "children": [], "span": { "start_line": 8, "end_line": 8, "cell": null } },
    { "layout": "default", "classes": ["dark"], "attrs": { "data-background-color": "#111" },
      "title": "Code",
      "blocks": [ { "type": "html", "html": "<h2>Code</h2>\n<pre><code class=\"language-python\" data-trim data-line-numbers=\"1-2|4\">import polars as pl\n…</code></pre>" } ],
      "notes": "<p>Mention compile times.</p>",
      "children": [ { "layout": "default", "title": "A vertical sub-slide",
                      "blocks": [ { "type": "html", "html": "<h2>A vertical sub-slide</h2><div class=\"fragment\"><p>Appears on click.</p></div>" } ] } ] },
    { "layout": "default", "title": "Two columns",
      "blocks": [ { "type": "html", "html": "<h2>Two columns</h2><div class=\"columns\"><div class=\"column\"><p>Left: <span data-math-style=\"inline\">E = mc^2</span></p></div><div class=\"column\"><p><img src=\"asset:3f9a1c2b7e0d\" alt=\"\"></p></div></div>" } ] },
    { "layout": "default", "title": null,
      "blocks": [ { "type": "cell", "index": 3, "language": "python", "source": "df.describe()",
                    "execution_count": 2, "hide_input": false, "hide_output": false, "fragment": false, "tags": [],
                    "metadata": {},
                    "outputs": [ { "kind": "display", "mime": "text/html", "html": "<table>…</table>", "execution_count": 2 } ] } ] }
  ],
  "features": { "math": true, "code": true, "notes": true, "notebook": true },
  "source": { "path": "/abs/talk/slides.md", "kind": "markdown", "base_dir": "/abs/talk" },
  "assets": [ { "id": "3f9a1c2b7e0d", "mime": "image/png", "origin": "file:figures/plot.png" } ]
}
```
