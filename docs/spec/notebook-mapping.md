# Spec: Jupyter notebook → slides

**Status:** contract. Implemented by T30 (model), T31 (cells → slides), T32 (outputs),
T33 (theme), T34 (lints). Compatible with `jupyter nbconvert --to slides` conventions so
existing notebooks work unchanged.

## 1. Input model (`src/notebook/nbformat.rs`, T30)

nbformat 4.x JSON, deserialized leniently with `serde_json`:

```rust
#[derive(Deserialize)] pub struct Notebook {
    pub cells: Vec<Cell>,
    #[serde(default)] pub metadata: serde_json::Value,
    pub nbformat: u32, #[serde(default)] pub nbformat_minor: u32,
}
#[derive(Deserialize)] pub struct Cell {
    pub cell_type: CellType,                    // markdown | code | raw
    #[serde(default)] pub id: Option<String>,
    #[serde(default)] pub metadata: serde_json::Value,
    #[serde(default)] pub source: MultilineString,
    #[serde(default)] pub outputs: Vec<Output>,            // code cells only
    #[serde(default)] pub execution_count: Option<i64>,
    #[serde(default)] pub attachments: BTreeMap<String, BTreeMap<String, MultilineString>>, // markdown cell images
}
#[derive(Deserialize)] #[serde(tag = "output_type", rename_all = "snake_case")]
pub enum Output {
    Stream { name: String, text: MultilineString },
    DisplayData { data: MimeBundle, #[serde(default)] metadata: Value },
    ExecuteResult { data: MimeBundle, #[serde(default)] metadata: Value, execution_count: Option<i64> },
    Error { ename: String, evalue: String, traceback: Vec<String> },
}
/// `"text"` or `["li","ne","s"]` → one String (segments concatenated, nothing inserted).
pub struct MultilineString(pub String);
/// MIME type → payload. Payload is a string or list of strings (text), or arbitrary JSON
/// (e.g. application/json, plotly). Keep as `Value` and convert lazily.
pub type MimeBundle = BTreeMap<String, serde_json::Value>;
```

- `nbformat < 4` → `Error::Parse { message: "nbformat 4 required" }`.
- Unknown `output_type` → ignore the output, emit a warning diagnostic.
- Unknown fields anywhere are ignored (`serde_json` default), never errors.
- `metadata.slideshow.slide_type` accessor: `cell.slide_type() -> SlideType`
  with `SlideType::{Slide, Subslide, Fragment, Skip, Notes, Continue}`; absent or `"-"` →
  `Continue`; unknown string → `Continue` plus a warning.
- Language: `metadata.language_info.name`, else `metadata.kernelspec.language`, else `"text"`.

> **Clarifications (T30, 2026-10-08):** `""` and `null` slide types are treated like `"-"`
> (JupyterLab 4 writes `""`). `CellType` has a fourth variant `Unknown` for cell types from newer
> nbformat minors (kept so indices align with `SourceSpan.cell`; converters skip it with a warning).
> The lenient entry point is `notebook::nbformat::parse_notebook(path, json) -> Result<(Notebook, Diagnostics-or-warnings)>`
> (see its doc comment); deserializing `Notebook` directly is strict. `frontmatter.ipynb` puts the
> front matter in the first **markdown** cell, which is not cell 0.

## 2. Cells → slides (`src/notebook/convert.rs`, T31)

```
slides: Vec<Slide> = []          // horizontal
cur: &mut Slide                  // the slide receiving content (top-level or a child)
for (index, cell) in cells.enumerate():
    if cell is removed by tags (§3)              → continue
    match cell.slide_type():
      Skip      → continue
      Slide     → push new top-level Slide; cur = it
      Subslide  → if slides empty: same as Slide
                  else push new Slide into slides.last().children; cur = it
      Notes     → ensure_first_slide(); cur.notes += render_notes(cell)
      Fragment | Continue →
                  ensure_first_slide()
                  blocks = convert_cell(cell, fragment = (slide_type == Fragment))
                  cur.blocks.extend(blocks)
```

- `ensure_first_slide()`: if `slides` is empty, push an empty slide and make it `cur`.
  (nbconvert behaviour: content before the first `slide` marker lands on slide 1.)
- `convert_cell`:
  - **markdown** cell: render the source with the Markdown renderer of T12 in
    *notebook mode*: no slide splitting, front matter only honoured for the first markdown
    cell (§5), `:::notes` directives still extract to `cur.notes`, heading attributes are
    lifted to `cur` **only if** the cell starts with a heading and `cur` is still empty.
    Result is `Block::Html`; when `fragment` the HTML is wrapped in `<div class="fragment">`.
    `attachments` (`![x](attachment:img.png)`) are registered as inline assets and the URL
    rewritten to `asset:<id>`.
  - **code** cell: `Block::Cell(NotebookCell { … })` with `fragment` set, outputs from §4,
    `hide_input`/`hide_output` from §3. When the cell is `hide_input && hide_output` it is
    omitted entirely.
  - **raw** cell: if `metadata.raw_mimetype` is `text/html` (or absent and `metadata.format == "text/html"`),
    emit `Block::Html` with the source verbatim; otherwise skip (warning).
- `Slide.title`: plain text of the first heading in the slide's first HTML block (same rule
  as Markdown).
- `Slide.span`: `SourceSpan { start_line: 1, end_line: <lines in first cell source>, cell: Some(index of the cell that opened the slide) }`.
- `Features.notebook = true` always for notebook sources; `code = true` if any code cell was
  emitted; `math` if any `data-math-style` or `text/latex` output; `notes` if any notes.

## 3. Tags and metadata that change rendering

| Source | Effect |
|---|---|
| tag `remove-cell`, `skip`, `hide-cell` | cell omitted entirely |
| tag `remove-input`, `hide-input`; `metadata.jupyter.source_hidden = true` | `hide_input = true` |
| tag `remove-output`, `hide-output`; `metadata.jupyter.outputs_hidden = true` | `hide_output = true` |
| tag `fragment` | same as `slide_type: fragment` (convenience) |
| tag `notes` | same as `slide_type: notes` |
| `metadata.presenter` (object) | per-cell overrides: `{ "layout": "...", "classes": [...], "attrs": {...} }` applied to the slide this cell **opens** (ignored otherwise, with a warning) |

Tag names are configurable (`[notebook] hide_input_tags`, `hide_output_tags`,
`remove_cell_tags` in `config.md`); the lists above are the defaults.

## 4. Output rendering (`src/notebook/outputs.rs`, T32)

For `DisplayData` / `ExecuteResult`, choose **one** MIME type: the first entry of the
configured priority list present in the bundle. Default priority:

```
text/html, image/svg+xml, image/png, image/jpeg, image/gif, image/webp,
text/markdown, text/latex, application/json, text/plain
```

| MIME | `CellOutput::Display.html` |
|---|---|
| `text/html` | the payload verbatim (string or joined list). Scripts are left in place; `check` warns when `<script` is present |
| `image/svg+xml` | payload verbatim, wrapped `<figure class="output-image">…</figure>`; if `metadata[mime].width/height` are present they are set as attributes on the root `<svg>` (best effort, regex) |
| `image/png`, `image/jpeg`, `image/gif`, `image/webp` | payload is base64 → `register_bytes(decoded, mime, "output.<ext>")` → `<img class="output-image" src="asset:<id>" alt="">` plus `width`/`height` from `metadata[mime]` when present |
| `text/markdown` | rendered with the Markdown renderer (notebook mode, no splitting) |
| `text/latex` | strip one pair of surrounding `$$`/`$`/`\[ \]` if present → `<div data-math-style="display">escaped TeX</div>`; sets `Features.math` |
| `application/json` | `<pre class="output-json">pretty-printed, escaped</pre>` |
| `text/plain` | `<pre class="output-text">escaped</pre>` |
| nothing matched | `CellOutput::Unsupported { mimes }` |

> **Clarifications (T32, 2026-10-08):** `text/plain` is `<pre class="output-text">` +
> `ansi_to_html(text)` (escaped, ANSI colours as spans, like nbconvert). Streams apply nbconvert's
> carriage-return rule after merging (progress bars show their final state). A chosen MIME whose
> payload cannot be rendered (bad base64, non-text payload, no renderer) warns `output-invalid` and
> the next MIME in priority order is tried. Image sizes fall back to top-level `metadata.width/height`.
> `text/latex` strips `\(…\)` too. Non-SGR ANSI sequences are removed.

Other outputs:
- `Stream { name, text }` → `CellOutput::Stream { name, html }` with `html = ansi_to_html::convert(&html_escape::encode_text(text))`
  — careful: convert ANSI **before** escaping would break the spans; the correct order is
  `ansi_to_html::convert_escaped` if the crate offers it, otherwise escape first and run the
  converter configured to not escape again. T32 verifies the crate's API (0.2.x has
  `convert(text) -> Result<String>` which escapes HTML itself; use that and do not pre-escape).
- `Error { ename, evalue, traceback }` → `html = convert(traceback.join("\n"))` same rule.
- Consecutive `Stream` outputs with the same `name` are merged into one (Jupyter splits them arbitrarily).
- `execution_count` on `Display` only for `ExecuteResult`.
- Widgets (`application/vnd.jupyter.widget-view+json`) have `text/plain` fallbacks in
  practice; the priority list handles them. `check` warns "interactive widget rendered as text".
- Plotly (`application/vnd.plotly.v1+json`) is not rendered in this plan (falls back to
  `text/html` when present, which needs plotly.js at runtime, or to `text/plain`). Stretch.

## 5. Deck metadata from a notebook (T31)

Merged in this order (later wins):
1. `metadata.title` (nbformat 4.5 convention) → `meta.title`.
2. `metadata.presenter` object: same keys as Markdown front matter (`title`, `author`,
   `theme`, `reveal`, …; unknown keys → `vars`).
3. If the **first markdown cell** begins with a `---` YAML front matter block, it is parsed
   exactly like Markdown front matter and removed from the cell content. This is the
   recommended way to set deck metadata from inside Jupyter.

> **Clarifications (T31, 2026-10-08):** a first markdown cell that holds only front matter opens
> no slide. A first-cell block that holds only YAML comments is empty front matter
> (`front-matter-empty` warning); a non-mapping block is an error, exactly as in Markdown. A `---`
> block at the start of a *later* markdown cell is rendered as Markdown, except that a block that
> would set deck metadata is removed with a `front-matter-ignored` warning. Hidden code cells
> (hide_input && hide_output) emit no block but still apply their slide_type structure.

## 6. Example mapping

Notebook cells (type · slide_type · tags):

```
0 markdown · slide      "# Analysis {layout=title}"
1 markdown · -          "Some intro"
2 code     · slide      df = load()
3 code     · fragment   df.describe()      tags: [hide-input]
4 markdown · subslide   "## Details"
5 code     · skip       debug()
6 markdown · notes      "Remember to mention caching"
7 raw      · -          <video src=...>    raw_mimetype text/html
```

Result:

```
slides[0]: layout=title, blocks=[Html(<h1>Analysis</h1>), Html(<p>Some intro</p>)]
slides[1]: blocks=[Cell(2), Cell(3, fragment=true, hide_input=true)]
           children=[ { title="Details", blocks=[Html(<h2>Details</h2>), Html(<video…>)], notes="<p>Remember to mention caching</p>" } ]
```

(Cells 6 and 7 attach to the current slide, which is the subslide opened by cell 4.)

## 7. Fixtures (`tests/fixtures/ipynb/`, T30)
- `minimal.ipynb` — one markdown + one code cell, no outputs.
- `slideshow.ipynb` — every `slide_type` value, exercising §2 and §6.
- `outputs.ipynb` — one output of each kind in §4, including a tiny real PNG (base64 of a
  1×1 pixel is fine), ANSI-colored stream, an error traceback, `text/latex`, merged streams.
- `tags.ipynb` — every tag in §3.
- `frontmatter.ipynb` — first-cell front matter + `metadata.presenter` + `metadata.title`
  precedence.
- Hand-write these as JSON; keep them small. The `examples/notebook/analysis.ipynb`
  (T33) is the only "real" executed notebook and must be regenerated by the documented
  command when its source changes.
