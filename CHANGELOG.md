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
  content-hashed assets instead.
- `presenter dump` prints a deck's intermediate representation as JSON, or with
  `--context` the template context.
- The Markdown dialect (`docs/spec/markdown-dialect.md`) supports:
  - YAML front matter;
  - `---` and `--` slide separators, or heading-split mode;
  - `{#id .class key=value}` attributes;
  - `:::` directives;
  - code fences with highlighted and stepped lines;
  - `$…$` and `$$…$$` math rendered with KaTeX;
  - speaker notes and incremental lists;
  - reveal.js Markdown-plugin comments.
- Notebooks (`docs/spec/notebook-mapping.md`) map onto slides through the Jupyter
  slideshow metadata (`slide_type`) and cell tags. Notebooks are not executed; the
  outputs already stored in them are rendered:
  - HTML, SVG and bitmap images;
  - Markdown, LaTeX and JSON;
  - plain text, and streams and errors with ANSI colors.
- Themes: the built-in `default` theme, and theme directories that extend it with
  minijinja templates and CSS (`docs/spec/theme-format.md`).
- Configuration through `presenter.toml`, front matter and command-line flags, with a fixed
  precedence (`docs/spec/config.md`).
- Release builds. `presenter --version` names the exact build as
  `presenter 0.1.0 (<git describe>)`. Pushing a `v*` tag publishes binaries for macOS
  (arm64, x86_64) and Linux (x86_64, arm64), with SHA-256 checksums.
