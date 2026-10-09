# Changelog

All notable changes to `presenter` are recorded in this file. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html).

Add an entry under `Unreleased` in the change that makes it. When a version is released,
its entries move to a `## [<version>] - <YYYY-MM-DD>` section, and the release workflow
publishes that section as the GitHub Release notes (see
[`docs/dev/release.md`](docs/dev/release.md)).

## [Unreleased]

### Added
- `presenter build` renders a Markdown file (`.md`) or a Jupyter notebook (`.ipynb`,
  nbformat 4) into one self-contained HTML file that works offline. reveal.js 6.0.2 and
  KaTeX 0.19.0 are embedded in the binary. `--site` writes a directory with
  content-hashed assets instead. `--watch` rebuilds whenever the deck, the files it uses or
  the theme change.
- `presenter serve` builds the deck in memory, serves it on `127.0.0.1:3000` (`--port`,
  where 0 picks a free port, `--host`, `--open`) and reloads the browser live over
  Server-Sent Events: a stylesheet or theme CSS change is swapped in without a reload, a
  content change replaces the slides and keeps the current slide, and build errors show in
  an overlay in the page. `--site-preview` serves the deck laid out as a site build.
- `presenter check` lints one or more decks without building them: separators that did not
  split, empty slides, duplicate slide ids, unknown layouts and directives, missing images,
  notebooks saved without outputs, and every warning a build would print. `--format json`
  and `--deny-warnings` make it fit for CI.
- `presenter init` scaffolds a presentation directory: `presenter.toml`, `slides.md` (or
  `slides.ipynb` with `--notebook`), a small child theme (or with `--full-theme` a copy of
  the default theme) and a `.gitignore`.
- `presenter theme list`, `theme eject`, `theme new` and `theme check`: list the built-in
  themes, copy one to edit freely, start a child theme, and validate a theme directory.
- `presenter export pdf` prints a deck to PDF with headless Chrome or Chromium (found on
  the system, or `--chrome`), one page per slide or fragment step. Slides taller than a page
  continue on further pages, and code blocks and notebook outputs that scroll on screen
  print in full. To leave room for the line Chrome moves at each page break, their lines
  print slightly closer than on screen; so a slide whose content ends just past a page
  boundary can end with a page that is empty but for the slide's background (about one in
  ten to twenty of the slides that run onto more pages).
- `presenter dump` prints a deck's intermediate representation as JSON, or with
  `--context` the template context. `presenter dump --default-config` lists every
  `presenter.toml` setting with its built-in default, commented out.
- A user guide in `docs/guide/`: getting started, the Markdown dialect, notebooks, themes,
  every setting and every command. Tests keep its `--help` blocks, its settings table and
  its links in sync with the binary and the repository.
- Every command takes `-v`/`-vv` (more logging), `-q` (errors only) and `--config PATH`.
  Errors name the file and line, with an excerpt of the source, on the terminal and in the
  `serve` overlay.
- The Markdown dialect (`docs/spec/markdown-dialect.md`) supports:
  - YAML front matter;
  - `---` and `--` slide separators, or heading-split mode (`[slides] split = "headings"`,
    as in pandoc);
  - `{#id .class key=value}` attributes;
  - `:::` directives (notes, fragments, columns, and any class);
  - code fences with highlighted and stepped lines;
  - `$…$` and `$$…$$` math rendered with KaTeX;
  - speaker notes and incremental lists;
  - GitHub tables, task lists, footnotes and alerts;
  - reveal.js Markdown-plugin comments (`<!-- .slide: … -->`, `<!-- .element: … -->`).
- `:::include part2.md` (or `analysis.ipynb`), alone on a slide, composes a Markdown deck
  from other Markdown files and notebooks: their slides take the directive's place, `serve`
  and `build --watch` watch them, and errors and `presenter check` name the included file
  and line (example: `examples/mixed`).
- Notebooks (`docs/spec/notebook-mapping.md`) map onto slides through the Jupyter
  slideshow metadata (`slide_type`) and cell tags (`hide-input`, `hide-output`, …).
  Notebooks are not executed; the outputs already stored in them are rendered:
  - HTML, SVG and bitmap images;
  - Markdown, LaTeX and JSON;
  - plain text, and streams and errors with ANSI colors.
- Themes: the built-in `default` theme (four layouts, documented CSS custom properties,
  styled code, tables, alerts, columns and notebook cells, print rules), and theme
  directories that extend it with minijinja templates and CSS or SCSS
  (`docs/spec/theme-format.md`). SCSS is compiled by the embedded `grass` compiler, can
  build on reveal.js's own theme template (`@use "reveal/theme/template/settings" with
  (…)`), and its CSS imports and fonts are inlined into standalone decks.
- Configuration through `presenter.toml`, front matter and command-line flags, with a fixed
  precedence (`docs/spec/config.md`).
- Opt-in headless-browser smoke tests (`cargo test --features browser-tests`): the example
  deck runs in Chrome without console errors, and `serve` swaps a changed theme
  stylesheet in a real browser without reloading the page.
- Release builds. `presenter --version` names the exact build as
  `presenter 0.1.0 (<git describe>)`. Pushing a `v*` tag publishes binaries for macOS
  (arm64, x86_64) and Linux (x86_64, arm64), with SHA-256 checksums and the license
  notices of the third-party code they contain: the Rust crates, reveal.js, KaTeX, the
  libraries bundled into reveal's files (highlight.js, highlightjs-line-numbers.js, marked
  and fitty), and the fonts of reveal's themes.
