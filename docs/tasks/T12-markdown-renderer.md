# T12 — Markdown block renderer

| | |
|---|---|
| Milestone | M1 |
| Size | L |
| Depends on | T01, T10, T11 |
| Parallel-safe with | T13, T14, T15, T30, T32 |
| Files you own | `src/markdown/mod.rs`, `src/markdown/render.rs`, `src/markdown/code.rs`, `tests/markdown_snapshots.rs`, `tests/fixtures/md/*.md` (may extend), `tests/snapshots/markdown_*.snap` |

## Goal
The Markdown frontend: `markdown::parse(source, text, config) -> Parsed` producing the full
`Deck` with rendered HTML blocks, extracted notes, code-fence attributes, math spans and
registered image assets. Also the reusable `render_fragment` used by notebooks and the
`markdown` template filter.

## Read first
- `docs/spec/markdown-dialect.md` (all), `docs/spec/ir.md` §1–§4
- `docs/spec/notebook-mapping.md` §2 (what notebook mode needs from you)

## API to implement
```rust
// src/markdown/mod.rs
pub fn parse(source: &SourceInfo, text: &str, config: &Config) -> Result<Parsed>;
pub fn comrak_options(config: &MarkdownConfig) -> comrak::Options<'static>;   // dialect §1, exact

// src/markdown/render.rs
pub enum RenderMode { Deck, Notebook }
pub struct RenderCtx<'r> { pub assets: &'r mut AssetRegistry, pub base_dir: &'r Path, pub mode: RenderMode,
                           pub diagnostics: &'r mut Diagnostics, pub missing_asset_is_error: bool }
pub struct Rendered { pub html: String, pub notes: Option<String>, pub features: Features, pub title: Option<String> }
/// Render a group of top-level nodes (one slide) to HTML. Detaches `:::notes`, rewrites
/// directives/code/images, formats with comrak.
pub fn render_nodes<'a>(arena: &'a Arena<AstNode<'a>>, nodes: &[&'a AstNode<'a>], opts: &Options, ctx: &mut RenderCtx) -> Result<Rendered>;
/// Parse + render a standalone Markdown string (notebook cells, text/markdown outputs, filter).
pub fn render_fragment(text: &str, opts: &Options, ctx: &mut RenderCtx) -> Result<Rendered>;

// src/markdown/code.rs
pub struct CodeAttrs { pub lang: Option<String>, pub pre_id: Option<String>, pub pre_classes: Vec<String>, pub code_attrs: BTreeMap<String, String>, pub escape: bool }
pub fn code_attrs(info: &str, attrs: Option<&ParsedAttrs>) -> CodeAttrs;      // dialect §5 table
pub fn render_code_block(source: &str, attrs: &CodeAttrs) -> String;          // <pre …><code …>…</code></pre>
```

## Steps
1. `parse`: build options; `comrak::parse_document`; take `FrontMatter` → T10 → `DeckMeta`
   and `config.merge_front_matter`; T11 `split`; for each `RawSlide` call `render_nodes`;
   fill `Slide` (attrs via `SlideAttrs::apply_to`, `title`, `notes`, `span`, `children`);
   merge `Features`; `Deck.assets = registry.infos(base_dir)`.
2. `render_nodes`: build a synthetic `Document` node in the arena, `detach()` each node and
   `append` it (so footnotes and lists render correctly as a unit). Then AST passes in this order:
   a. **notes**: find `BlockDirective` nodes named `notes` anywhere; detach them; render each
      one's children through a second synthetic document; join into `notes`.
   b. **directives**: for `fragment` with `index=`, `columns` with `widths=`, and any directive
      with `key=value` args: replace the node's value with `HtmlBlock("<div class=… data-…>")`,
      move its children after it, and insert `HtmlBlock("</div>")`. Plain directives are left
      to comrak (it already emits `<div class="name args">`).
   c. **code**: for each `CodeBlock`: compute `code_attrs(info, node.attrs)` and replace the
      node with `HtmlBlock(render_code_block(...))`. (This avoids depending on how comrak
      passes attributes to the `SyntaxHighlighterAdapter`.) Set `features.code`.
   d. **images**: for each `Image` with a relative URL: resolve against `base_dir`; if the file
      exists → `register_file` and set `url = "asset:<id>"`; else diagnostic `missing-asset`
      (error when `missing_asset_is_error`). Also scan `HtmlBlock`/`HtmlInline` literals for
      `src="…"`/`poster="…"` relative values with a regex and rewrite the same way.
   e. **math**: if any `Math` node exists → `features.math`.
   f. **title**: `first_heading_text` (T11) before any heading is modified.
   Then `comrak::format_html(doc, opts, &mut out)`.
3. `render_fragment`: parse text to its own arena/document and call the same passes
   (notebook mode: never split; front matter ignored here — T31 strips it first).
4. Escaping: `html_escape::encode_text` for code; `encode_double_quoted_attribute` for
   attribute values you emit.

## Acceptance criteria
- `insta` JSON snapshot of `Deck` for every fixture in `tests/fixtures/md/` (`insta::assert_json_snapshot!`
  with the `source.path`/`base_dir` redacted to `[path]`).
- `code.md`: every row of dialect §5 appears in the snapshot with the exact attribute output;
  `escape=false` leaves `<mark>` intact; trailing newline removed.
- `directives.md`: notes removed from body and present in `notes`; `:::fragment fade-up index=2` →
  `<div class="fragment fade-up" data-fragment-index="2">`; `:::columns widths="1fr 2fr"` →
  `style="grid-template-columns: 1fr 2fr"`; nested `:::column` inside `:::columns` works.
- `images.md`: existing image → `src="asset:` + 12 hex; missing image → diagnostic and
  `Error::MissingAsset` when `missing_asset_is_error`; `https://` and `/abs.png` untouched;
  raw `<img src="rel.png">` rewritten.
- `math.md`: `features.math == true`, spans have `data-math-style`.
- `render_fragment("# Hi\n\n$x$")` works without front matter and returns `title == Some("Hi")`.
- `cargo clippy -D warnings` clean; no `unwrap`.

## Out of scope
Heading-split mode, `Note:` compat, `<!-- .slide: -->`, incremental lists (T43). Theme/HTML document (T15).

## Hints (verified)
- `comrak::nodes::NodeValue::{BlockDirective(NodeBlockDirective { info, fence_length, fence_offset }), CodeBlock(NodeCodeBlock { info, literal, fenced, .. }), Image(NodeLink { url, title }), HtmlBlock(NodeHtmlBlock { literal, block_type }), HtmlInline(String), Math(NodeMath { dollar_math, display_math, literal })}`.
- Node manipulation: `node.detach()`, `parent.append(child)`, `node.insert_after(other)`, `node.data_mut().value = …`, `arena.alloc(Node::new(RefCell::new(Ast::new(NodeValue::Document, sourcepos))))`.
- comrak directive rendering: `:::foo bar` → `<div class="foo bar">` — children only, no attribute support; hence pass (b).
- `render.r#unsafe = true` is required or your `HtmlBlock` replacements are escaped/clobbered.
- Image attributes `{width=50%}`: check whether `format_html` writes `attrs` for `Image`; if not, convert the image to `HtmlInline("<img …>")` in pass (d).

## Amendments (coordinator, 2026-10-08)
- `parse` returns `Parsed { deck, assets, diagnostics }` (ir.md §4 contract change) and **never
  fails on a missing image**: it renders with `missing_asset_is_error = false`, so missing images
  become `missing-asset` warnings (with path + span) and keep their URL. `render_nodes` still
  honours `ctx.missing_asset_is_error = true` by returning `Error::MissingAsset` (test both).
- `parse` applies `merge_front_matter` to a clone of `config` before splitting/rendering.
- Diagnostics from T11's `RawDeck` are forwarded into `Parsed.diagnostics`.
- Call `split::split_with_source(root, text, mode)` (T11's exact, source-based separator
  detection) rather than the AST-only `split`. `split` returns `Error::Parse` with an empty path
  for unbalanced quotes in a separator; fill in `source.path` before returning it.
- comrak does not emit node attrs for headings/links/images/inline code, and renders ```` ```math ````
  as `<pre><code class="language-math" …>`; both need the AST rewrites (dialect §3.3, §5 notes).
- Standalone background videos: see T15's amendment (warning only; nothing to do here).
