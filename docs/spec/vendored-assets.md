# Spec: Vendored assets

**Status:** contract. Created by T00 (`scripts/vendor.sh`), consumed by T15/T21/T42.
All vendored files are committed to the repo under `assets/vendor/` and embedded into the
binary with `rust-embed` (`#[folder = "assets/"]`). Paths used by templates are relative to
`assets/`, e.g. `vendor/reveal/dist/reveal.js`.

## 1. Versions (pinned; verified 2026-10-09)

| package | version | license | source |
|---|---|---|---|
| reveal.js | 6.0.2 | MIT | `https://registry.npmjs.org/reveal.js/-/reveal.js-6.0.2.tgz` |
| KaTeX | 0.19.0 | MIT | `https://registry.npmjs.org/katex/-/katex-0.19.0.tgz` |

`assets/vendor/VERSIONS.toml` records name, version, tarball URL, SHA-256 of the tarball and
the date; `scripts/vendor.sh` regenerates everything from scratch and must be idempotent.

Other people's work inside those packages, each shipped with its license file (§2), which
the release archives copy into `THIRD-PARTY-LICENSES/assets/` (`docs/dev/release.md`):

| work | version | license | where it is | license file in `assets/vendor/` |
|---|---|---|---|---|
| highlight.js | 11.11.1 | BSD-3-Clause | bundled in `reveal/dist/plugin/highlight.js`; `dist/plugin/highlight/{monokai,zenburn}.css` are its styles | `highlight.js/LICENSE` |
| highlightjs-line-numbers.js | 2.8.0 | MIT | bundled in `reveal/dist/plugin/highlight.js` (reveal copies it into the plugin's source) | `highlightjs-line-numbers.js/LICENSE` |
| marked | 17.0.5 | MIT | bundled in `reveal/dist/plugin/notes.js` | `marked/LICENSE.md` |
| fitty | 2.4.2 | MIT | bundled in `reveal/dist/reveal.js` (sizes `r-fit-text`) | `fitty/LICENSE` |
| Source Sans Pro | — | OFL-1.1 | `reveal/css/theme/fonts/source-sans-pro/`, inlined in the `white`, `black`, `white-contrast` and `black-contrast` themes | `reveal/css/theme/fonts/source-sans-pro/LICENSE` |
| League Gothic | — | OFL-1.1 | `reveal/css/theme/fonts/league-gothic/`, inlined in the `beige`, `league`, `moon` and `solarized` themes | `reveal/css/theme/fonts/league-gothic/LICENSE` and `LICENSE-OFL.txt` |

reveal's package contains none of the four libraries' licenses, so `vendor.sh` copies them
from the libraries' own npm packages (pinned and checksummed like the two above, recorded in
`VERSIONS.toml` with `bundled_in`; nothing else of them is vendored). The versions are the
ones reveal 6.0.2 was built with. For highlight.js, marked and fitty, reveal's npm
dependencies, that is reveal's `package-lock.json` at the `6.0.2` tag
(`packages["node_modules/<name>"].version`; highlight.js's bundle also carries its version
string). highlightjs-line-numbers.js is not a dependency: reveal copies it into
`plugin/highlight/plugin.js`, whose header comment at that tag names version 2.8.0.

reveal ships League Gothic with a `LICENSE` that only names the OFL; the OFL asks every copy
of the font to carry its copyright notice and the license text, so `vendor.sh` writes
`LICENSE-OFL.txt` next to it: the copyright notice recorded in `league-gothic.woff` and the
OFL 1.1 text from Source Sans Pro's `LICENSE`.

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

> **Note (T15, 2026-10-08):** only 6 of the 14 `dist/theme/*.css` files inline their fonts
> (white, black, white-contrast, black-contrast, dracula, serif). The other 8 `@import` Google
> Fonts; standalone builds keep those imports (fonts load when online) and warn `remote-import`.

> **Contract change (A5, 2026-10-09, audit finding F19):** besides the files above,
> `scripts/vendor.sh` writes these license files (and nothing else of their packages):
> ```
> highlight.js/LICENSE                  package/LICENSE of highlight.js 11.11.1 (BSD-3-Clause)
> highlightjs-line-numbers.js/LICENSE   package/LICENSE of highlightjs-line-numbers.js 2.8.0 (MIT)
> marked/LICENSE.md                     package/LICENSE.md of marked 17.0.5 (MIT)
> fitty/LICENSE                         package/LICENSE of fitty 2.4.2 (MIT)
> reveal/css/theme/fonts/league-gothic/LICENSE-OFL.txt   generated, see §1
> ```
> `VERSIONS.toml` lists the four libraries as packages too, with `bundled_in` naming the
> reveal file that contains them. The versions are those of reveal's `package-lock.json` at
> its release tag (and, for highlightjs-line-numbers.js, the header of reveal's
> `plugin/highlight/plugin.js`); upgrading reveal means reading them again (§5).

> **Correction (A5, 2026-10-09):** the note above is wrong about which themes inline fonts.
> Eight do: `white`, `black`, `white-contrast` and `black-contrast` inline Source Sans Pro,
> `beige`, `league`, `moon` and `solarized` League Gothic (and also `@import` a Google Font
> for their body text). `dracula` and `serif` use system fonts only. `beige`, `blood`,
> `league`, `moon`, `night`, `simple`, `sky` and `solarized` `@import` Google Fonts.

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
   version-related changes). Pin the versions of the libraries the new reveal bundles: read
   highlight.js, marked and fitty from reveal's `package-lock.json` at the new release tag
   (`https://raw.githubusercontent.com/hakimel/reveal.js/<version>/package-lock.json`,
   `packages["node_modules/<name>"].version`) and highlightjs-line-numbers.js from the header
   comment of `plugin/highlight/plugin.js` at that tag. Check the new reveal's `js/` and
   `plugin/*/plugin.js` imports for a library not listed in §1. (`tests/vendor_smoke.rs`
   checks highlight.js's version string and a piece of each other library's code in the
   bundles.)
2. Run `cargo test` and the browser smoke test (`cargo test --features browser-tests`).
3. Read the upstream release notes for breaking changes (reveal 6.0 moved plugin paths —
   templates reference `dist/plugin/<name>.js`).
4. Update `docs/spec/vendored-assets.md` §1 and the user docs (`vendor.sh` writes
   `VERSIONS.toml`).
