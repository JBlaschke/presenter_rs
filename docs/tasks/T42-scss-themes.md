# T42 — SCSS themes via grass, reveal template imports, CSS asset inlining

| | |
|---|---|
| Milestone | M4 |
| Size | M |
| Depends on | T14, T17 |
| Parallel-safe with | T40, T41, T43–T47 |
| Files you own | `src/theme/styles.rs`, `Cargo.toml` (`grass` dep, additive), `examples/custom-theme-scss/**`, `tests/styles.rs`, `src/render/html.rs` (replace the "SCSS arrives in T42" error with a call into `styles.rs`, additive) |

## Goal
Theme authors can write SCSS, and in particular can derive a theme from reveal's own
theme template exactly like the stock themes do: `@use "reveal/theme/template/settings" with (...)`.
Local `@import url(...)` and font `url()`s in CSS are inlined for standalone output.

## Read first
- `docs/spec/theme-format.md` §5; `docs/spec/vendored-assets.md` §2 (SCSS sources vendored)
- `assets/vendor/reveal/css/theme/white.scss` and `template/*.scss` (what must compile)

## Steps
1. `grass = { version = "0.13", default-features = false }` (check which default features you need; `random` is only for `random()`).
2. `styles.rs`: `pub fn compile_entry(file: &ThemeFile, theme: &Theme) -> Result<String>`:
   `.css` → passthrough; `.scss/.sass` → `grass::from_string(source, &Options::default().fs(&VirtualFs { theme, entry_dir }))`.
   `VirtualFs` implements `grass::Fs` (`is_dir`, `is_file`, `read`) mapping: paths starting with
   `reveal/` → embedded `vendor/reveal/css/<rest>` (so `reveal/theme/template/settings` resolves to
   `css/theme/template/settings.scss`; implement partial/underscore and `.scss` extension probing
   like Sass does); other relative paths → the entry's own directory on disk/embedded, then the chain's
   `styles/` directories. Map grass errors to `Error::Style { file, message }` (include line/col from grass's error display).
3. Post-processing shared by CSS and SCSS results: inline local `@import url("x.css")` /
   `@import "x.css"` by recursively compiling and splicing (depth ≤ 5, cycle guard); then
   `register_css_urls(... lenient = false)` for theme CSS. This makes reveal's font CSS
   (`@import url('./fonts/source-sans-pro/source-sans-pro.css')` from the stock theme sources)
   resolve to the vendored woff2 files when a user theme does `@use "reveal/theme/white"`
   — because `VirtualFs` serves `reveal/theme/fonts/...` as well, make `register_css_urls`'
   resolver try the virtual fs for `reveal/`-prefixed and fonts-relative refs.
4. `examples/custom-theme-scss/`: `theme.toml` with `extends = "default"`, `reveal_theme = "none"`,
   `styles.entries = ["styles/theme.scss"]`; `theme.scss` that does
   `@use "reveal/theme/template/settings" with ($background-color: #0b1021, $main-color: #e6e6e6, $heading-font: "Georgia, serif", $link-color: #ffb454);`
   `@use "reveal/theme/template/theme";` plus a few rules; `slides.md` to try it.
5. Document SCSS usage in the theme README (T17 owns it; additive section).

## Acceptance criteria
- `tests/styles.rs`: compiling `assets/vendor/reveal/css/theme/white.scss` through `VirtualFs` succeeds and
  contains `--r-background-color:#fff` (or with a space, normalize) and no `@import url(./fonts` (inlined);
  the example theme compiles; a syntax error reports `Error::Style` with the file name and a line number.
- `presenter build examples/custom-theme-scss/slides.md` standalone output contains `data:font/woff2` and no
  `url(fonts/`.
- Build time overhead of SCSS compilation for the example < 300 ms (log it at debug level; assert loosely in a test with a 2 s cap).

## Out of scope
Converting the default theme to SCSS (deliberately kept plain CSS); Sass `@import` of remote URLs.

## Hints (verified)
- `grass` 0.13.4 CLI compiled reveal 6's `white.scss` unchanged (7 KB output). It supports `@use … with (…)`, `sass:color`, `color.scale`, and passes `@import url(...)` through verbatim.
- `grass::Options::fs(&dyn grass::Fs)`; `grass::Fs` has `fn is_dir(&self, path: &Path) -> bool; fn is_file(&self, path: &Path) -> bool; fn read(&self, path: &Path) -> io::Result<Vec<u8>>`.
