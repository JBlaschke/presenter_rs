# Changelog

All notable changes to `presenter` are recorded in this file. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html).

Add an entry under `Unreleased` in the change that makes it. When a version is released,
its entries move to a `## [<version>] - <YYYY-MM-DD>` section, and the release workflow
publishes that section as the GitHub Release notes (see
[`docs/dev/release.md`](docs/dev/release.md)).

## [Unreleased]

### Changed
- Directives nest with fences of one length, as in pandoc: a closing fence closes, of the
  open directives whose fence is not longer than its own, the innermost one with the longest
  fence. `:::small` inside a `:::column` inside `::::columns` now works: before, the `:::`
  that closed the `small` closed its column too, and the column's own `:::` opened an empty
  block that took in everything up to the next `:::` line, such as the next column. Decks
  with a longer fence at every level of nesting read as before, unless a `:::` in a list
  item or definition closed a directive around the list (below). A deck that let one `:::`
  close two directives now leaves the outer one open, with a warning, so write the missing
  `:::`. The same rule holds inside a list item, block quote, footnote or definition, and
  each keeps its own directives. A `:::` in one closes only a directive opened in it, never
  one around it, so it no longer closes a directive around a list along with the list. A
  directive opened in one ends with it, with a warning when its `:::` is missing there.
- A `:::` line with nothing left to close is ignored, with an `invalid-directive` warning
  that says why: the directive around it has a longer fence, the line is indented too far or
  followed by white space other than a space or tab (such as a no-break space or `&#32;`), it
  is in a list item, block quote, footnote or definition where no directive is left open
  (it never closes one around it), or there is a `:::` too many. Before, it opened an empty
  block that ran to the next `:::` line, and `---` lines in it started no slides.
- A directive runs to its own closing fence, also past the end of its slide. One without a
  `:::` of its own is an `invalid-directive` warning, which says what ends it: a longer fence
  that closes a directive around it, the end of its list item, block quote, footnote or
  definition, or the end of the deck (or notebook cell).

### Added
- The default theme styles `:::callout` blocks: a tinted box like an alert's that holds any
  content, in the accent colour or the one a second word names (`:::callout tip`; also
  `note`, `important`, `warning`, `caution` and `muted`), with an optional
  `title="…"` set over the text as an alert's title is. Callouts straight inside
  `::::columns` are columns, and when every column is a callout they are as tall as each
  other. The `--p-callout` token sets the default colour; a theme adds colours by setting
  `--p-callout-color` on a class of its own.

### Fixed
- `export pptx` text looks like the browser's in PowerPoint: lines break where the browser
  broke them, keep their widths (letter spacing makes up for PowerPoint drawing a system
  font as Arial, which is wider), and sit on the browser's baselines (PowerPoint's
  whole-point line spacing and first-line placement are allowed for). The padding of
  inline code and key caps counts in a line's width, and superscripts and subscripts
  (footnote marks, and the scripts of inline math) are raised as far and drawn as large as
  in the browser. A task list's checkboxes are its items' bullets, and the text of an item
  that wraps lines up after its box. Before, text in boxed labels such as a pill wrapped
  onto a second line, lists overflowed their slide, the lines of a long code block ran
  lower and lower, footnote marks sat on the baseline at half size, and rounded boxes had
  half the corner radius, their text off-centre, and outlines one border-half too big.
- `export pptx` makes the translucent colour of text that a theme draws with `::before` or
  `::after` (such as a callout's title) opaque over what is behind it, as it already did for
  all other text. Keynote, Preview and QuickLook ignore alpha on text and showed such text
  as dark as the body text.
- `check` counts the lines its `separator-not-recognized` messages name ("from line 8",
  "opened on line 32", …) within the notebook cell, as it already counted the line the
  warning is on. In a cell that starts with front matter they were counted from after it.
- `check` reads a deck that starts with empty front matter (`---` on lines 1 and 2) as the
  build does. It took everything up to the first `---` after line 2, often a slide
  separator, for front matter: it reported none of the `---`/`--` lines up to there, and
  could report one after it that starts a slide as one inside a directive.
- An HTML block or code block at the end of the last item of a list, right before the `:::`
  of a directive around the list, keeps its content, such as `- <img src="a.png"
  width="200">` as a column's last line, or an indented or fenced code sample that ends a
  list. comrak leaves such a block unfinished and empty. It rendered as an empty item or
  an empty code block, without a warning, and `check` did not see the image in it.
- A `:::name` line right after a task item's checkbox (`- [ ] :::small`) is reported as
  what it is: comrak reads it as the item's text, since a directive cannot start that text,
  and the warning says to start the directive on the next line. It used to blame a `:`
  after the fence that was not there. A `- [ ] :::include` line is an error that says an
  include cannot be in a list.
- A line that shows a directive fence as text, its colons escaped or written as character
  references (`\:::small`, `&#58;::small`), is no longer reported as a directive comrak did
  not open, with advice to write a colon as `&#58;` that it already followed, and
  `\:::include part.md` is no longer an error. A bare `:::` after such a line closes nothing
  and is now reported, as anywhere else.

## [0.1.1] - 2026-11-02

### Fixed
- `export pdf` and `export pptx` fit each page to what its slide shows before Chrome prints.
  A picture that runs off the slide, as on screen, no longer gives the slide a second page
  onto which Chrome moved the whole picture, leaving the first page without it: the slide
  prints on one page with the picture cut off at the page's edge. A slide whose content ends
  in the page's bottom margin no longer gets an empty second page.
- The project website is rebuilt after a release, so its download table shows the
  release: GitHub starts no workflow for a release that a workflow's own token published,
  so the release workflow now asks for the site build itself.

## [0.1.0] - 2026-10-12

### Added
- Release archives for Windows (x64 and arm64): `.zip` files with `presenter.exe`. The C
  runtime is linked statically, so no redistributable is needed. Paths in messages are
  `/`-separated there too, as in decks and the guide. `export pdf` and `export pptx` are
  not available on Windows yet.
- A project website, built by CI from `main` and on every release
  ([`docs/dev/site.md`](docs/dev/site.md)): the downloads of the latest release, the
  guide's pages and every example deck, rendered.
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
- `presenter export pptx` writes a PowerPoint file with the same headless Chrome: every
  page of the PDF export becomes one editable slide, so a slide with fragments builds up
  step by step over several slides, and a slide taller than a page continues on further
  slides. Headings, paragraphs, lists, code and quotes are text boxes with the theme's
  fonts, sizes, colours and bullets (code keeps its highlighting, a paragraph per line),
  tables are tables, images are the deck's own image files, backgrounds are colours or
  pictures, and themes' decorations (rules, logos) are kept; everything sits where the
  browser laid it out. What PowerPoint cannot hold as native content (SVG, iframes,
  display math, gradients) becomes a picture with a transparent background. The speaker
  notes go into the notes pane, the slides are named after their headings, and the slide
  size follows the deck's (a 16:9 deck gives PowerPoint's Widescreen size). `--scale` sets
  the resolution of the pictures (2 by default). The file depends on nothing but the deck
  and the Chrome version, so the same deck gives the same file.
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
