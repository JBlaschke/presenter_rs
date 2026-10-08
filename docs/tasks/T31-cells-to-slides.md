# T31 — Cells → slides

| | |
|---|---|
| Milestone | M3 |
| Size | M |
| Depends on | T30, T12 (and T10 for front matter) |
| Parallel-safe with | T17, T20–T24, T32 (agree on `CellOutput` producer signature below) |
| Files you own | `src/notebook/mod.rs`, `src/notebook/convert.rs`, `tests/notebook_snapshots.rs`, `tests/snapshots/notebook_*.snap` |

## Goal
The notebook frontend: `notebook::parse(source, json, config) -> Parsed` implementing the
slide mapping, tags, raw cells, notes and deck metadata rules.

## Read first
- `docs/spec/notebook-mapping.md` §2, §3, §5, §6; `docs/spec/ir.md` §1 (`NotebookCell`, `Features`)
- T12's `render_fragment` and T11's `lift_heading_attrs`/`first_heading_text`

## API
```rust
pub fn parse(source: &SourceInfo, json: &str, config: &Config) -> Result<Parsed>;
// convert.rs
pub fn convert(nb: &Notebook, source: &SourceInfo, config: &mut Config, assets: &mut AssetRegistry, diags: &mut Diagnostics) -> Result<Deck>;
// Outputs are produced by T32's function; until it lands, use this signature and a stub:
// pub fn render_outputs(outputs: &[Output], cfg: &NotebookConfig, assets: &mut AssetRegistry, diags: &mut Diagnostics) -> (Vec<CellOutput>, Features)
```

## Steps
1. Deck metadata (§5): `metadata.title` → `metadata.presenter` → first markdown cell front
   matter (`frontmatter::split_front_matter`; remove it from the cell source). Apply
   `config.merge_front_matter`.
2. Walk cells with the algorithm in §2; implement tag handling (§3) using the configurable
   tag lists; `metadata.presenter` per-cell overrides applied to the slide the cell opens.
3. Markdown cells: `render_fragment` in `RenderMode::Notebook`; heading attribute lifting only
   when the current slide is still empty; `attachments` → `register_bytes` and rewrite
   `attachment:NAME` URLs before rendering (simple string replace on the source is acceptable
   because the names are opaque).
4. Code cells → `NotebookCell` with outputs from `render_outputs`; `hide_input && hide_output` → skip.
5. Raw cells per §2. `Slide.span` with `cell: Some(index)`.
6. `Features`: `notebook = true`, others merged from fragments and outputs.

## Acceptance criteria
- `insta` JSON snapshots of `Deck` for every `tests/fixtures/ipynb/*.ipynb` (paths redacted).
- `slideshow.ipynb` matches spec §6 exactly (slide count 2, children 1, notes on the subslide, cell 5 absent, fragment flag on cell 3, `hide_input` on cell 3).
- `frontmatter.ipynb`: front matter title beats `metadata.presenter.title` beats `metadata.title`; the `---` block is gone from the first slide's HTML.
- `tags.ipynb`: `remove-cell` cell absent; `hide-input` → `hide_input`; `notes` tag → notes; custom tag names via config work.
- A notebook with zero cells → one empty slide, no panic.

## Out of scope
Output HTML (T32), cell template (T33), lints (T34).

## Amendments (coordinator, 2026-10-08)
- `notebook::parse` exists as a coordinator stub in `src/notebook/mod.rs`; replace its body.
- Use T30's `nbformat::parse_notebook(path, json)` (lenient; returns warnings/diagnostics),
  T32's `outputs::render_outputs`, T12's `markdown::render::render_fragment_with_lift` (notebook-mode
  heading lifting only while the current slide is empty), T10's `frontmatter::split_front_matter`
  / `parse_front_matter` / `meta_from_json` (for `metadata.presenter`).
- ir.md §2 invariant 4 vs. notebook-mapping §4: `text/html` outputs and raw HTML cells may contain
  relative `src`/`poster` URLs (e.g. `<img src="fig.png">`, IPython `Video("clip.mp4")`). Run them
  through T12's raw-HTML asset rewriting (the same tokenizer T12 uses for HtmlBlock literals; make it
  `pub` additively if needed) with `source.base_dir`, so standalone builds stay self-contained and
  missing files produce `missing-asset` warnings.
- `CellType::Unknown` cells are skipped with a warning; `""`/null slide types count as `"-"`.
