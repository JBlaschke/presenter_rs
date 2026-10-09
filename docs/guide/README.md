# presenter user guide

presenter turns a Markdown file or a Jupyter notebook into a reveal.js presentation: one
HTML file that works offline. While you write, `presenter serve` shows the deck in the
browser and updates it on every save of the deck, its images or its theme.

| Page | What it covers |
|---|---|
| [Getting started](getting-started.md) | Install, create a deck with `presenter init`, preview it, build it, export a PDF |
| [Writing slides](authoring.md) | The Markdown dialect: separators, attributes, directives, code, math, images, notes, headings mode, `presenter check` |
| [Notebooks](notebooks.md) | Slide types and tags from Jupyter, front matter in the first cell, which outputs render, running the notebook first |
| [Themes](themes.md) | Theme directories, tokens, layouts, extending and ejecting, SCSS, templates and their context, the live-reload loop |
| [Configuration](configuration.md) | `presenter.toml`, front matter and flags: every setting with its default and how they combine |
| [Command line](cli.md) | Every command with its `--help`, exit codes and output conventions |

The repository's `examples/` directory has a deck for each part of the guide:

| Example | Shows |
|---|---|
| [`examples/basic`](../../examples/basic/README.md) | The Markdown features, with the built-in theme |
| [`examples/notebook`](../../examples/notebook/README.md) | An executed notebook: plots, a DataFrame, math, a traceback |
| [`examples/headings-mode`](../../examples/headings-mode/slides.md) | Slides that start at headings, as in pandoc, and reveal.js-style comments |
| [`examples/custom-theme-scss`](../../examples/custom-theme-scss/README.md) | A dark child theme written in SCSS on top of reveal.js's theme template |

## Not yet

These do not exist yet. Each entry says what to do instead.

- **Running notebooks.** presenter renders the outputs saved in a notebook and never runs
  it. Run it in Jupyter first, or with `jupyter nbconvert --to notebook --execute --inplace`
  ([Notebooks: run it first](notebooks.md#run-the-notebook-first)).
- **Plotly figures.** A Plotly output is not drawn from its JSON. presenter shows its
  `text/html` form, which needs plotly.js at presentation time, or its text. Save static
  images instead ([Notebooks: interactive outputs](notebooks.md#interactive-outputs-widgets-plotly-bokeh)).
- **Interactive widgets** (`ipywidgets`). They need a running kernel. presenter shows the
  widget's text form (`IntSlider(value=3)`) and `presenter check` warns about it.
- **`:::include`.** Including another Markdown file is reserved but does nothing: the
  directive renders as a plain `<div>`, and `presenter check` warns. Keep the deck in one
  file.
- **A logo file next to the deck.** The title layout's `logo:` must name a file in a theme's
  `assets/` directory ([Themes: the logo](themes.md#the-title-slides-logo)).
- **Minified output.** `[build] minify` is accepted and does nothing.
