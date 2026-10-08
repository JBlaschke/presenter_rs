# T32 — Notebook output rendering

| | |
|---|---|
| Milestone | M3 |
| Size | M |
| Depends on | T30, T14 (and T12's `render_fragment` for `text/markdown`) |
| Parallel-safe with | T31 (shared signature below), T15, T17 |
| Files you own | `src/notebook/outputs.rs`, `tests/outputs.rs` |

## Goal
Reduce each Jupyter output to one `CellOutput` with ready-to-insert HTML, following the MIME
priority rules, with images registered as assets and ANSI colors preserved.

## Read first
- `docs/spec/notebook-mapping.md` §4; `docs/spec/ir.md` (`CellOutput`); `docs/spec/config.md` (`[notebook] mime_priority`)

## API
```rust
pub fn render_outputs(outputs: &[Output], cfg: &NotebookConfig, assets: &mut AssetRegistry, diags: &mut Diagnostics, md: &mut dyn FnMut(&str) -> Result<String>) -> Result<(Vec<CellOutput>, Features)>;
pub fn choose_mime<'a>(bundle: &'a MimeBundle, priority: &[String]) -> Option<&'a str>;
pub fn render_display(mime: &str, payload: &Value, metadata: &Value, assets: &mut AssetRegistry) -> Result<(String, bool /* sets features.math */)>;
pub fn ansi_to_html(text: &str) -> String;     // escapes + converts; never fails (falls back to escaped text)
```

## Steps
1. Merge consecutive `Stream` outputs with the same `name` before rendering.
2. `choose_mime` per priority; `render_display` per the §4 table. Image sizes from
   `metadata[mime]["width"|"height"]` when numeric. SVG wrapped in `<figure class="output-image">`.
3. `text/latex`: strip one surrounding delimiter pair (`$$…$$`, `$…$`, `\[…\]`, `\(…\)`), trim,
   emit `<div data-math-style="display">…</div>` escaped.
4. `ansi_to_html`: use `ansi_to_html::convert(text)`; verify in a test whether the crate
   escapes HTML (0.2.x does); if it returns `Err`, fall back to `html_escape::encode_text`.
5. Diagnostics: `text/html` containing `<script` → warning `output-script`; unsupported-only → warning `output-unsupported` listing MIME types.

## Acceptance criteria (table-driven tests over `outputs.ipynb` and inline bundles)
- Priority: bundle with `text/plain` + `image/png` → `image/png`; custom priority `["text/plain"]` → `text/plain`.
- PNG → `<img class="output-image" src="asset:` + 12 hex, registered bytes decode to the original; `width`/`height` attributes when metadata present.
- Two stdout streams `"a\n"`, `"b\n"` → one `Stream` with html `a\nb\n`.
- `"\u001b[31mred\u001b[0m"` → contains `<span` with a color style and the text `red`; `<` in a stream is escaped.
- Error traceback → `Error { ename, evalue, html }` with ANSI converted.
- `text/latex` `"$$x^2$$"` → `<div data-math-style="display">x^2</div>` and `features.math`.
- Widget-only bundle → `Unsupported { mimes: [...] }` + diagnostic.
- `text/markdown` goes through the supplied `md` closure (test with a closure that uppercases).

## Out of scope
Cell template/CSS (T33), plotly (stretch, not planned).
