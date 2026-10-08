# T15 — HTML renderer: context, helpers, reveal config, KaTeX init

| | |
|---|---|
| Milestone | M1 |
| Size | M |
| Depends on | T13, T14 (and T02's templates to test against) |
| Parallel-safe with | T12, T30, T32 |
| Files you own | `src/render/mod.rs`, `src/render/context.rs`, `src/render/html.rs`, `src/render/reveal.rs`, `assets/presenter/math.js`, `tests/render.rs`, `tests/snapshots/render_*.snap` |

## Goal
Turn `Deck + Theme + Config + mode` into the final HTML document (and, for serve/site, the
separate theme stylesheet and emitted files), implementing every template helper from the
theme spec.

## Read first
- `docs/spec/theme-format.md` §4 (context + helpers; this is your spec), §5
- `docs/spec/config.md` §4 (`reveal_options_json`, `enabled_plugins`), `docs/spec/vendored-assets.md` §2–§4
- `docs/spec/live-reload.md` §1 (serve URLs)

## API to implement
```rust
pub struct BuildInfo { pub mode: BuildMode /* Build | Serve */, pub standalone: bool, pub live_reload: bool, pub id: u64, pub version: &'static str }
pub struct RenderInput<'a> { pub deck: &'a Deck, pub assets: &'a AssetRegistry, pub theme: &'a Theme, pub config: &'a Config, pub output: OutputMode, pub build: BuildInfo }
pub struct RenderedDocument { pub html: String, pub theme_css: String, pub files: Vec<EmittedFile>, pub slide_count: usize }
pub fn render(input: RenderInput<'_>) -> Result<RenderedDocument>;

// context.rs
pub fn build_context(input: &RenderInput<'_>, theme_css_url: &str) -> Result<minijinja::Value>;   // theme-format.md §4 shape, `layout_template` decoration
// reveal.rs
pub struct PluginInfo { pub id: PluginId, pub js_ident: &'static str, pub script: &'static str /* "vendor/reveal/dist/plugin/highlight.js" */ }
pub fn plugins(config: &Config, features: &Features) -> Vec<PluginInfo>;   // enabled ∩ (notes only if features.notes || explicitly listed), highlight always when features.code
pub fn reveal_config_json(config: &Config, meta: &DeckMeta, theme: &Theme) -> String;   // theme < config(+front matter) merge, "plugins" stripped, sorted
```

## Steps
1. Theme CSS: concatenate `theme.style_entries()` (`.css` only; `.scss` → `Error::Style("SCSS support arrives in T42")`),
   run `register_css_urls` with a resolver that looks relative to the entry's own directory
   then `theme.read_asset`, then placeholder-resolve with the emitter (standalone: inline
   data URIs; site: files; serve: `/__presenter/asset/…` for theme assets by mapping ids back —
   simplest: in serve mode register theme assets as bytes and serve `/__presenter/deck-asset/<id>`).
2. Helpers (closures capturing `Arc<Mutex<HelperState>>` — minijinja stores functions as
   `Send + Sync` boxes, so `Rc<RefCell<..>>` does not compile — holding the emitter and a list of
   vendor files used): `style`, `script`, `asset`, `theme_style`, `reveal_init`, `live_reload`
   per the §4 table; filter `attrs`; filter `markdown` (calls T12 `render_fragment` with a
   throwaway registry — images in template-rendered markdown are not supported; document).
   Vendor lookups: `vendor::get(path)`; `vendor/katex/katex.min.css` must go through
   `register_css_urls(lenient = true)` so fonts inline in standalone mode.
3. Context per §4. `layout_template`: `format!("layouts/{}.html", slide.layout)` if
   `theme.has_template`, else `layouts/default.html` + diagnostic `unknown-layout`.
4. `reveal_config_json`: merge `theme.reveal_defaults()` ← `config.reveal` ← `meta.reveal`
   (right wins), remove `plugins`, serialize with sorted keys. Add `width/height` defaults from config.
5. `assets/presenter/math.js`:
   ```js
   (function(){ function render(root){ if(!window.katex) return;
     root.querySelectorAll('[data-math-style]').forEach(el=>{ if(el.dataset.rendered) return;
       const display = el.dataset.mathStyle==='display';
       try { katex.render(el.textContent, el, Object.assign({displayMode: display, throwOnError:false}, window.__presenterKatexOptions||{})); el.dataset.rendered='1'; }
       catch(e){ console.warn('[presenter] katex', e); } }); }
     window.__presenterRenderMath = render; document.addEventListener('DOMContentLoaded', ()=>render(document)); })();
   ```
   `reveal_init()` emits `window.__presenterKatexOptions = <config.math.katex_options json>;` before `Reveal.initialize`.
   Include KaTeX when `features.math` or `config.math.enabled == "always"`, never when `"never"`.
6. Final pass: `Emitter::resolve_html` on the rendered document; `files` from the emitter;
   `slide_count` = number of top-level slides + children (for the CLI summary).

## Acceptance criteria
- `tests/render.rs`: render `tests/fixtures/ir/sample-deck.json` with the default theme in
  Standalone, Site and Serve modes → `insta` snapshots of the `<div class="slides">…</div>`
  region (strip everything else) are identical across modes except asset URLs; standalone
  output contains `<style id="presenter-theme">`, serve output contains
  `<link id="presenter-theme" … href="/__presenter/theme.css?v=0">`, site output lists files.
- Standalone output has no `http://`/`https://` references except those present in the deck content.
- A deck with `features.math == false` and `math.enabled = "auto"` → no `katex` substring in output; with `true` → KaTeX CSS inlined and fonts are `data:font/woff2` URIs.
- `reveal_config_json` precedence test (theme transition `fade` < config `slide` < front matter `zoom`).
- Unknown layout falls back to default and emits the diagnostic.
- Helpers used with an unknown path → `Error::Template` naming the path and the template.

## Out of scope
Writing files to disk (T16), SCSS (T42), live-reload client (T22).

## Hints (verified)
- minijinja: `env.add_function("style", move |path: String| -> Result<Value, Error> { … Ok(Value::from_safe_string(html)) })`;
  `env.add_filter("attrs", |v: Value| …)`; `Value::from_serialize(&deck)`; use
  `minijinja::value::Value::from_safe_string` for HTML results so autoescape does not touch them.
- `Features`-gated plugin scripts: `highlight` 921 KB is the largest; include only when `features.code` or explicitly listed in `reveal.plugins` and the deck has code — keep it simple: include when `features.code`.
- reveal 6 wants `Reveal.initialize({...})` after `dist/reveal.js` and plugin scripts; plugin globals are `RevealHighlight`, `RevealNotes`, `RevealZoom`, `RevealSearch`.

## Amendments (coordinator, 2026-10-08)
- The `markdown` filter cannot call T12's `render_fragment` yet (T12 runs in the same wave).
  Implement it with `comrak::markdown_to_html` and options equivalent to markdown-dialect §1
  (`render.r#unsafe = true`). T16 switches it to `markdown::render::render_fragment`.
- `reveal_config_json` merge order: `Config::builtin_reveal_defaults()` ← `theme.reveal_defaults()`
  ← `config.reveal` ← `meta.reveal` (right wins), `plugins` stripped, keys sorted, no trailing `.0`
  on integers (config.md contract change).
- `plugins()` uses `config.enabled_plugins()` (effective reveal map).
