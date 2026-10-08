# Spec: Vendored assets

**Status:** contract. Created by T00 (`scripts/vendor.sh`), consumed by T15/T21/T42.
All vendored files are committed to the repo under `assets/vendor/` and embedded into the
binary with `rust-embed` (`#[folder = "assets/"]`). Paths used by templates are relative to
`assets/`, e.g. `vendor/reveal/dist/reveal.js`.

## 1. Versions (pinned; verified 2026-10-08)

| package | version | license | source |
|---|---|---|---|
| reveal.js | 6.0.2 | MIT | `https://registry.npmjs.org/reveal.js/-/reveal.js-6.0.2.tgz` |
| KaTeX | 0.19.0 | MIT | `https://registry.npmjs.org/katex/-/katex-0.19.0.tgz` |

`assets/vendor/VERSIONS.toml` records name, version, tarball URL, SHA-256 of the tarball and
the date; `scripts/vendor.sh` regenerates everything from scratch and must be idempotent.

## 2. Files copied (and nothing else)

From reveal.js (`package/` → `assets/vendor/reveal/`):
```
LICENSE
dist/reveal.js            (119 KB)   dist/reveal.css (54 KB)   dist/reset.css (1 KB)
dist/plugin/highlight.js  (921 KB, bundles highlight.js + common languages)
dist/plugin/highlight/monokai.css  dist/plugin/highlight/zenburn.css
dist/plugin/notes.js      (68 KB)    dist/plugin/zoom.js   dist/plugin/search.js
dist/theme/*.css          (14 files, fonts inlined, 2.4 MB total)
css/theme/template/{settings,theme,mixins}.scss      (for SCSS themes, T42)
css/theme/*.scss                                      (stock theme sources, T42)
css/theme/fonts/**                                    (league-gothic, source-sans-pro; woff2 + the .css that declares them; drop .ttf/.woff if present)
```
> **Contract change (coordinator, 2026-10-08, from T00):** reveal.js 6.0.2 ships the
> `css/theme/fonts/**` fonts as `.woff` only (no `.woff2`), and their `.css` files reference
> `./*.woff`. `scripts/vendor.sh` therefore keeps `LICENSE`, `*.css`, `*.woff2` **and** `*.woff`
> there, and drops `.ttf/.eot/.otf/.svg`. Inlined reveal theme fonts appear as `data:font/woff`.
> KaTeX fonts are unaffected (woff2). `vendor::get` normalizes paths (see `src/vendor.rs`), and
> OS/editor junk (`.DS_Store`, `*.swp`, `*~`, …) is excluded from embedding.

Not copied: `dist/plugin/markdown.js` (we render Markdown ourselves), `dist/plugin/math.js`
(loads from CDN; we ship our own KaTeX init), `*.mjs`, `*.d.ts`, `index.html`, `demo.html`,
`js/`, `css/print/*` (reveal.css already contains print rules), source maps.

From KaTeX (`package/dist/` → `assets/vendor/katex/`):
```
LICENSE  katex.min.js (273 KB)  katex.min.css (25 KB)  fonts/*.woff2 (20 files, 296 KB)
```
`katex.min.css` references `fonts/KaTeX_*.woff2` first and `.woff`/`.ttf` as fallbacks.
Browsers use the first supported source, so woff2-only is sufficient; the fallback URLs
remain in the CSS and are simply never requested. The emitter's CSS `url()` rewriting must
tolerate unresolvable fallback URLs **inside vendor CSS only** (leave them as-is, no error).

Our own files (`assets/presenter/`, hand-written, not vendored):
```
math.js          renders [data-math-style] elements with katex.render; exposes window.__presenterRenderMath(root)
live-reload.js   see live-reload.md §3
```

## 3. Size budget
A standalone deck with code and math, no images: reveal (175 KB) + highlight (921 KB) +
one stock theme (≈170–580 KB) + KaTeX (595 KB) + notes/zoom/search (75 KB) ≈ 2–2.3 MB.
Without math ≈ 1.4–1.7 MB. Images add their size × 1.37 (base64). Acceptable for the
single-file use case; site mode exists for anything larger.

## 4. Embedding (`src/vendor.rs`, T00)

```rust
#[derive(rust_embed::Embed)]
#[folder = "assets/"]
#[include = "vendor/**/*"]
#[include = "presenter/**/*"]
#[include = "themes/**/*"]
#[exclude = "**/*.map"]
pub struct Assets;

pub fn get(path: &str) -> Option<std::borrow::Cow<'static, [u8]>>;   // Assets::get(path).map(|f| f.data)
pub fn get_str(path: &str) -> crate::error::Result<String>;           // UTF-8 or Error::Other
pub fn list(prefix: &str) -> Vec<String>;                             // for `theme list`, tests
```
rust-embed reads from disk in debug builds (so editing `assets/themes/default/*` while
running `cargo run -- serve` live-reloads) and embeds in release builds. Add
`debug-embed` feature toggling if CI needs embedded behaviour in debug tests.

## 5. Upgrade procedure
1. Edit versions at the top of `scripts/vendor.sh`; run it; review the diff (expect only
   version-related changes).
2. Run `cargo test` and the browser smoke test (`cargo test --features browser-tests`).
3. Read the upstream release notes for breaking changes (reveal 6.0 moved plugin paths —
   templates reference `dist/plugin/<name>.js`).
4. Update `VERSIONS.toml`, `docs/spec/vendored-assets.md` §1 and the user docs.
