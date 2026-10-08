# T14 — Asset registry and emitter

| | |
|---|---|
| Milestone | M1 |
| Size | M |
| Depends on | T01 |
| Parallel-safe with | T02, T10, T11, T12, T13, T30 |
| Files you own | `src/assets/mod.rs`, `src/assets/registry.rs`, `src/assets/emit.rs`, `tests/assets.rs` |

## Goal
The single place that knows how local files become URLs: register assets (by path or bytes),
and rewrite `asset:<id>` placeholders in HTML and `url()` references in CSS into `data:`
URIs (standalone), hashed files (site) or server routes (serve).

## Read first
- `docs/spec/ir.md` §3 (registry API and placeholder grammar), `docs/spec/live-reload.md` §1 (serve URLs), `docs/spec/theme-format.md` §4–§5, `docs/spec/vendored-assets.md` §2 (lenient vendor CSS)

## API to implement
```rust
// registry.rs — exactly ir.md §3 plus:
impl AssetRegistry { pub fn merge(&mut self, other: AssetRegistry); pub fn len(&self) -> usize; pub fn read(&self, id: &str) -> Result<Cow<'_, [u8]>>; /* reads File lazily, errors with MissingAsset */ }

// emit.rs
pub enum OutputMode { Standalone, Site { assets_dir: PathBuf, url_prefix: String /* "assets/" */ }, Serve }
pub struct EmittedFile { pub path: PathBuf, pub bytes: Vec<u8> }
pub struct Emitter<'a> { registry: &'a AssetRegistry, mode: &'a OutputMode, pub files: Vec<EmittedFile> }
impl<'a> Emitter<'a> {
    pub fn new(registry: &'a AssetRegistry, mode: &'a OutputMode) -> Self;
    /// Replace every `asset:<id>` in attribute values and CSS url() of an HTML document.
    pub fn resolve_html(&mut self, html: &str) -> Result<String>;
    /// Replace `asset:<id>` inside a CSS text.
    pub fn resolve_css(&mut self, css: &str) -> Result<String>;
    /// URL for one id in the current mode (data URI / "assets/plot-3f9a1c2b7e0d.png" / "/__presenter/deck-asset/3f9a1c2b7e0d").
    pub fn url_for(&mut self, id: &str) -> Result<String>;
}
/// Rewrite relative url(...) references in a CSS file to placeholders, registering them.
/// `resolve_rel` maps a relative ref to an absolute path or bytes (theme chain lookup is supplied by the caller).
/// `lenient = true` leaves unresolvable refs untouched (vendor CSS with woff/ttf fallbacks).
pub fn register_css_urls(css: &str, registry: &mut AssetRegistry, resolve_rel: &mut dyn FnMut(&str) -> Option<AssetData>, lenient: bool) -> Result<String>;
pub fn data_uri(mime: &str, bytes: &[u8]) -> String;
pub fn hashed_file_name(hint: &str, id: &str) -> String;   // "plot.png" + id → "plot-3f9a1c2b7e0d.png"
```

## Steps
1. Ids: `blake3::hash(bytes).to_hex()[..12]`; for files hash the absolute path string
   (`path.to_string_lossy()`), so re-registering is idempotent and ids are stable across runs.
2. Placeholder scanning: regex `asset:([0-9a-f]{12})` applied to the whole document is
   acceptable **because** the grammar is unambiguous and HTML text content never legitimately
   contains it; document this. Collect unique ids first, resolve each once, then replace.
3. CSS `url()` parsing: regex `url\(\s*(['"]?)([^'")]+)\1\s*\)`; skip `data:`, `http(s):`, `//`,
   `#` fragments, and absolute `/`. Strip `?query#frag` when resolving but keep them in site/serve URLs.
4. Site mode: `url_for` pushes an `EmittedFile` once per id (dedupe by id) with
   `assets_dir.join(hashed_file_name(hint, id))` and returns `url_prefix + file name`.
5. Standalone: `data_uri(mime, bytes)` (base64 standard alphabet, no line breaks); SVG stays base64 too (simplest, safe).
6. Serve: `/__presenter/deck-asset/<id>` — no bytes read at render time.

## Acceptance criteria
- Same file registered twice → same id, `len() == 1`; bytes registered with different content → different ids.
- `resolve_html` on `<img src="asset:…"> <section data-background-image="asset:…"> <div style="background:url(asset:…)">`
  rewrites all three in all three modes; unknown id → `Error::Other` naming the id.
- `register_css_urls("@font-face{src:url(fonts/a.woff2) format('woff2'), url(fonts/a.woff)}", …, lenient=true)`
  with only `a.woff2` resolvable → first ref becomes a placeholder, second untouched; with `lenient=false` → error.
- `data_uri("image/png", b"\x89PNG")` starts with `data:image/png;base64,iVBORw`.
- Site mode writes exactly one file per distinct id; file names are deterministic.
- `MissingAsset` raised by `read()` when a registered file was deleted between parse and emit.

## Out of scope
Deciding *which* URLs are relative in Markdown (T12) or theme CSS lookup order (T15).

## Hints (verified)
- `base64` 0.23: `use base64::Engine; base64::engine::general_purpose::STANDARD.encode(bytes)`.
- `mime_guess::from_path(p).first_or_octet_stream().essence_str()`.
- Keep `blake3` default features (SIMD) — it is already fast; no need for `rayon`.
