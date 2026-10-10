# Command line

```text
presenter [OPTIONS] <COMMAND>
```

The help blocks on this page are the binary's own `--help` output (a test keeps them in
sync). `presenter <command> --help` shows the same in your terminal.

## Conventions

**Global options.** `-v` logs more (`-vv` debug), `-q` prints only errors, and
`--config PATH` reads that `presenter.toml` instead of looking for one. They work before or
after the command: `presenter -q check slides.md` and `presenter check slides.md -q` are the
same. `RUST_LOG` (for example `RUST_LOG=presenter=debug`) adds log filters on top.

**Output.** Standard output carries only a command's result: the `Wrote …` line of `build`,
`export pdf` and `export pptx`, the URL line of `serve`, the JSON of `dump` (TOML with
`--default-config`), the findings of `check --format json`, and the reports of `init` and
the `theme` commands. Warnings, errors and logs go to standard error, as
`path:line: level: message`, in colour when it is a terminal (`NO_COLOR=1` turns colour
off). Paths in messages are relative to the current directory.

**Exit codes.**

| Code | Meaning |
|---|---|
| 0 | Success (warnings do not count, except with `check --deny-warnings`) |
| 1 | The deck or theme has an error: a parse, template or style error, a missing image in `build`, a `check` finding of level error (or any finding, with `--deny-warnings`). Also any other failure while the command runs: a port that is in use, a file that cannot be read or written, Chrome failing during `export pdf` or `export pptx` |
| 2 | Wrong usage: a bad argument, an unknown file extension, an invalid `presenter.toml`, an output that would overwrite an input, a `theme` argument that names nothing usable |
| 3 | `export pdf` or `export pptx` found no Chrome or Chromium |

**Inputs are safe.** `build`, `export pdf` and `export pptx` never write over the deck, `presenter.toml`,
an image the deck uses or a file of a theme directory in use, also not through a symbolic
link or another spelling of the name. Such an output path is refused before anything is
written (exit 2):

```text
error: refusing to write slides.md: it is the input file slides.md; choose another output path with -o
```

## presenter

```text
$ presenter --help
Turn Markdown files and Jupyter notebooks into reveal.js presentations

Usage: presenter [OPTIONS] <COMMAND>

Commands:
  build   Render a deck to HTML
  serve   Render, watch and live-reload in the browser
  init    Scaffold a new presentation directory
  theme   Inspect, eject or create themes
  dump    Print the IR or the template context as JSON
  check   Lint a deck without building
  export  Export to other formats (pdf, pptx)
  help    Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
  -h, --help           Print help
  -V, --version        Print version
```

## build

```text
$ presenter build --help
Render a deck to HTML

Usage: presenter build [OPTIONS] <INPUT>

Arguments:
  <INPUT>
          The deck: a .md or .ipynb file

Options:
  -o, --out <PATH>
          Output file (standalone) or directory (site) [default: <build.out_dir>/<stem>.html, or
          <build.out_dir>/<stem>/ with --site; an out_dir set in presenter.toml is relative to that
          file, the default dist/ to the input's directory]. Never the input itself

  -v, --verbose...
          Log more: -v info, -vv debug (RUST_LOG directives apply on top)

  -q, --quiet
          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
          --deny-warnings` still prints the warnings it fails on

      --theme <NAME|PATH>
          Theme name or directory (overrides front matter and presenter.toml)

      --config <PATH>
          Use this presenter.toml instead of searching for one next to the input

      --standalone
          Write one self-contained HTML file (the default unless [build] standalone = false)

      --site
          Write a directory: index.html plus assets/

      --watch
          Rebuild whenever the deck, its assets or the theme change

      --format <FORMAT>
          Input format (default: from the file extension)

          Possible values:
          - md:    Markdown (`.md`)
          - ipynb: Jupyter notebook (`.ipynb`)

  -h, --help
          Print help (see a summary with '-h')
```

- Prints `Wrote dist/slides.html (6 slides, 1.8 MB)` on success.
- The output goes to `<out_dir>/<deck name>.html` (`--site`: `<out_dir>/<deck name>/index.html`
  and `assets/`), with `out_dir` from `[build]` ([Configuration](configuration.md#build)).
  `-o` names the file (standalone) or the directory (`--site`); a standalone `-o` must not
  name a directory.
- A missing image is an error here (in `serve` it is a warning). Each one is reported with
  its line.
- `--watch` builds, then rebuilds on every change of the deck, its images, `presenter.toml`
  or the theme, until Ctrl-C. A failed rebuild prints the error and keeps watching.

## serve

```text
$ presenter serve --help
Render, watch and live-reload in the browser

Usage: presenter serve [OPTIONS] <INPUT>

Arguments:
  <INPUT>  The deck: a .md or .ipynb file

Options:
      --theme <NAME|PATH>  Theme name or directory (overrides front matter and presenter.toml)
  -v, --verbose...         Log more: -v info, -vv debug (RUST_LOG directives apply on top)
      --port <N>           Port to listen on; 0 picks a free one [default: [serve] port, 3000]
  -q, --quiet              Print only errors (no warnings, no summary line; RUST_LOG is ignored).
                           `check --deny-warnings` still prints the warnings it fails on
      --config <PATH>      Use this presenter.toml instead of searching for one next to the input
      --host <H>           Address to bind [default: [serve] host, 127.0.0.1]
      --open               Open the deck in the default browser
      --site-preview       Serve the deck the way a site build lays it out
  -h, --help               Print help
```

- Prints `Serving http://127.0.0.1:3000/` as its first line. Ctrl-C stops it (exit 0).
- Stylesheet changes swap in place; other changes replace the slides and keep the current
  slide; errors show in an overlay in the page ([the live-reload loop](themes.md#the-live-reload-loop)).
- When the port is taken, it says so and exits 1; `--port 0` picks a free one.
- It answers only requests for `localhost`, `127.0.0.1`, `[::1]` or the `--host` given,
  other host names get `403 Forbidden` ([Security](themes.md#security)).
- `--site-preview` serves the deck the way `build --site` lays it out, to check a site build
  before you publish it.

## init

```text
$ presenter init --help
Scaffold a new presentation directory

Usage: presenter init [OPTIONS] [DIR]

Arguments:
  [DIR]  Directory to create (must be empty or not exist) [default: .]

Options:
      --name <NAME>    Presentation name [default: the directory name]
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
      --notebook       Start from a notebook (slides.ipynb) instead of slides.md
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
      --full-theme     Eject the full default theme instead of a small child theme
  -h, --help           Print help
```

Creates `presenter.toml` (every setting, commented out), `slides.md` or with `--notebook`
`slides.ipynb`, a child theme in `theme/` (with `--full-theme` a copy of the whole built-in
theme), a `README.md` and a `.gitignore` for `dist/`. The directory must be empty or not
exist (exit 2 otherwise).

## theme

```text
$ presenter theme --help
Inspect, eject or create themes

Usage: presenter theme [OPTIONS] <COMMAND>

Commands:
  list   List the built-in themes and the theme resolved for the current directory
  eject  Copy a built-in theme's files to DIR (DIR must not exist)
  new    Create a minimal child theme in DIR
  check  Validate a theme: theme.toml, its chain and the required templates
  help   Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
  -h, --help           Print help
```

```text
$ presenter theme list --help
List the built-in themes and the theme resolved for the current directory

Usage: presenter theme list [OPTIONS]

Options:
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
  -h, --help           Print help
```

```text
$ presenter theme eject --help
Copy a built-in theme's files to DIR (DIR must not exist)

Usage: presenter theme eject [OPTIONS] <NAME> <DIR>

Arguments:
  <NAME>  Built-in theme to copy
  <DIR>   Destination directory (must not exist)

Options:
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
  -h, --help           Print help
```

```text
$ presenter theme new --help
Create a minimal child theme in DIR

Usage: presenter theme new [OPTIONS] <DIR>

Arguments:
  <DIR>  Directory of the new theme

Options:
      --extends <NAME>  Parent theme [default: default]
  -v, --verbose...      Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet           Print only errors (no warnings, no summary line; RUST_LOG is ignored).
                        `check --deny-warnings` still prints the warnings it fails on
      --config <PATH>   Use this presenter.toml instead of searching for one next to the input
  -h, --help            Print help
```

```text
$ presenter theme check --help
Validate a theme: theme.toml, its chain and the required templates

Usage: presenter theme check [OPTIONS] <DIR>

Arguments:
  <DIR>  Theme directory to validate

Options:
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
  -h, --help           Print help
```

- `list` names the built-in themes, and the theme a deck in the current directory would use,
  with its chain.
- `eject default DIR` copies the built-in theme, README included, into a new theme that
  extends nothing ([Themes](themes.md#eject-a-full-copy)).
- `new DIR` writes a child theme: `theme.toml`, a `styles/theme.css` that lists the parent's
  tokens (commented out) and an empty `templates/` ([Themes](themes.md#extend-a-theme)).
- `check DIR` validates a theme directory. Exit codes for all four: 0 ok, 1 a problem in the
  theme (`check` findings, an invalid `theme.toml`, a missing parent), 2 an argument that
  names nothing usable (not a built-in theme, a `DIR` that exists for `eject` or is not empty
  for `new`, a `check` directory without `theme.toml`).

## dump

```text
$ presenter dump --help
Print the IR or the template context as JSON

Usage: presenter dump [OPTIONS] <INPUT>
       presenter dump --default-config

Arguments:
  [INPUT]  The deck: a .md or .ipynb file

Options:
      --context            Print the template context (theme resolved) instead of the deck IR
  -v, --verbose...         Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet              Print only errors (no warnings, no summary line; RUST_LOG is ignored).
                           `check --deny-warnings` still prints the warnings it fails on
      --theme <NAME|PATH>  Theme name or directory (overrides front matter and presenter.toml)
      --config <PATH>      Use this presenter.toml instead of searching for one next to the input
      --pretty             Pretty-print the JSON (the default)
      --compact            Print the JSON on one line
      --default-config     Print every presenter.toml setting with its default, commented out,
                           instead (no INPUT)
  -h, --help               Print help
```

- `presenter dump slides.md` prints the deck as presenter parsed it: slides, their
  attributes and HTML, notes, the assets it found. Keys are sorted.
- `--context` prints the data the theme's templates receive ([Themes](themes.md#what-templates-receive)).
- `--default-config` lists every `presenter.toml` setting with its default, each
  commented out: a reference to copy single lines from, not a file to save as it is
  ([Configuration](configuration.md#every-setting)). It ignores `--config`.

## check

```text
$ presenter check --help
Lint a deck without building

Usage: presenter check [OPTIONS] <INPUT>...

Arguments:
  <INPUT>...
          Decks to lint (.md or .ipynb)

Options:
      --format <FORMAT>
          Output format

          Possible values:
          - text: `path:line: level: message` lines
          - json: A JSON array of diagnostics

          [default: text]

  -v, --verbose...
          Log more: -v info, -vv debug (RUST_LOG directives apply on top)

      --deny-warnings
          Exit 1 when there are warnings

  -q, --quiet
          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
          --deny-warnings` still prints the warnings it fails on

      --config <PATH>
          Use this presenter.toml instead of searching for one next to the input

      --theme <NAME|PATH>
          Theme name or directory (overrides front matter and presenter.toml)

  -h, --help
          Print help (see a summary with '-h')
```

- Findings are `path:line: level: message` lines; a notebook finding names the cell
  (`analysis.ipynb: cell 3, line 1: warning: …`). Nothing is printed for a clean deck.
- Exit 0 with only warnings, 1 with an error, or with any warning under `--deny-warnings`.
  `-q` hides warnings, except those that `--deny-warnings` fails on.
- `--format json` prints an array of `{level, code, message, path, line, cell}`.
- [Writing slides](authoring.md#check-a-deck) and [Notebooks](notebooks.md#check-a-notebook)
  list what it finds.

## export pdf

```text
$ presenter export --help
Export to other formats (pdf, pptx)

Usage: presenter export [OPTIONS] <COMMAND>

Commands:
  pdf   Export a deck to PDF with headless Chrome
  pptx  Export a deck to PowerPoint (.pptx) with headless Chrome: an editable slide per slide or
        fragment step
  help  Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...     Log more: -v info, -vv debug (RUST_LOG directives apply on top)
  -q, --quiet          Print only errors (no warnings, no summary line; RUST_LOG is ignored). `check
                       --deny-warnings` still prints the warnings it fails on
      --config <PATH>  Use this presenter.toml instead of searching for one next to the input
  -h, --help           Print help
```

```text
$ presenter export pdf --help
Export a deck to PDF with headless Chrome

Usage: presenter export pdf [OPTIONS] <INPUT>

Arguments:
  <INPUT>  The deck: a .md or .ipynb file

Options:
  -o, --out <FILE>         PDF file to write [default: next to the HTML output, with .pdf]
  -v, --verbose...         Log more: -v info, -vv debug (RUST_LOG directives apply on top)
      --chrome <PATH>      Chrome or Chromium executable [default: $CHROME_PATH, then well-known
                           locations]
  -q, --quiet              Print only errors (no warnings, no summary line; RUST_LOG is ignored).
                           `check --deny-warnings` still prints the warnings it fails on
      --config <PATH>      Use this presenter.toml instead of searching for one next to the input
      --theme <NAME|PATH>  Theme name or directory (overrides front matter and presenter.toml)
  -h, --help               Print help

Limitations (reveal.js print mode, ?print-pdf):
  - Fragments: with reveal.js' default ([reveal] pdfSeparateFragments = true) every
    fragment step is printed on its own page; with pdfSeparateFragments = false the
    fragments collapse and each slide is printed once, fully revealed.
  - Animations, transitions and auto-animate are not exported: every page is a static
    snapshot. Videos and iframes show whatever they display when the page is printed.
  - A slide taller than the page continues on extra pages, up to [reveal]
    pdfMaxPagesPerSlide. The default theme prints the lines of code blocks and outputs
    slightly closer than on screen, to leave room for the line Chrome moves at each page
    break; reveal.js reserves the pages from the screen height, so such a slide that ends
    just past a page boundary on screen can end with a page that is empty but for its
    background (about one in ten to twenty of them). Nothing is lost.
  - Needs Chrome or Chromium (Edge and Brave work too); without one the command
    exits with code 3. Chrome gets 120 s for the whole export.
  - The PDF is not byte-for-byte reproducible, unlike `presenter build`: Chrome stamps
    the export time (CreationDate, ModDate) and its own version (Creator, Producer)
    into it, and another Chrome version lays the pages out differently.

Tips: [reveal] options shape the PDF: width/height set the page size, showNotes = true
prints the speaker notes, and pdfSeparateFragments = false gives one page per slide.
```

- Prints `Wrote dist/slides.pdf (9 pages, 167.4 kB)`. Without `-o`, the PDF goes next to
  where `build` would write the HTML.
- Chrome is looked for with `--chrome`, then `CHROME_PATH`, then the usual install
  locations on macOS and Linux. Without one, the command explains how to install it and
  exits 3.
- A `-o` that does not end in `.pdf` gets a warning.

## export pptx

```text
$ presenter export pptx --help
Export a deck to PowerPoint (.pptx) with headless Chrome: an editable slide per slide or fragment
step

Usage: presenter export pptx [OPTIONS] <INPUT>

Arguments:
  <INPUT>  The deck: a .md or .ipynb file

Options:
  -o, --out <FILE>         PowerPoint file to write [default: next to the HTML output, with .pptx]
  -v, --verbose...         Log more: -v info, -vv debug (RUST_LOG directives apply on top)
      --chrome <PATH>      Chrome or Chromium executable [default: $CHROME_PATH, then well-known
                           locations]
  -q, --quiet              Print only errors (no warnings, no summary line; RUST_LOG is ignored).
                           `check --deny-warnings` still prints the warnings it fails on
      --config <PATH>      Use this presenter.toml instead of searching for one next to the input
      --theme <NAME|PATH>  Theme name or directory (overrides front matter and presenter.toml)
      --scale <N>          Resolution of the pictures taken of what PowerPoint cannot hold natively
                           (SVG, display math, iframes…): device pixels per slide pixel, 1 to 4
                           [default: 2]
  -h, --help               Print help

What the file holds (from reveal.js' print mode, ?print-pdf):
  - Every page of the PDF export becomes one editable slide: headings, paragraphs,
    lists, code and quotes are text boxes with the fonts, sizes, colours and bullets
    the theme gave them (code keeps its highlighting colours, one paragraph per line),
    tables are tables, images are the deck's image files, and the slide background
    is a colour or the background picture. Everything sits where the browser laid it
    out. The speaker notes are text in the notes pane, one paragraph per line.
  - What PowerPoint cannot hold as native content becomes a picture with a transparent
    background: SVG, canvases, videos and iframes (as displayed), display math, and
    gradient backgrounds. Inline math becomes text (superscripts and subscripts kept,
    fractions as a/b). Themes' own decorations (rules under headings, logos) are
    kept when they are boxes or pictures; reveal.js' slide number is left out.
  - Fragments: with reveal.js' default ([reveal] pdfSeparateFragments = true) every
    fragment step is a slide of its own, so the slides build the deck's slide up step
    by step; with pdfSeparateFragments = false each slide is exported once, fully
    revealed.
  - Animations, transitions and auto-animate are not exported: every slide is static.
  - A slide taller than the page continues on extra slides, up to [reveal]
    pdfMaxPagesPerSlide, with the same pages as the PDF.
  - Fonts: PowerPoint substitutes a font the viewer's machine does not have, so text
    may wrap differently there; a text box grows to fit when it is edited.
  - Needs Chrome or Chromium (Edge and Brave work too); without one the command
    exits with code 3. Chrome gets 120 s to load the deck, plus 3 s per page.
  - The file is reproducible for one Chrome version: the same deck gives the same
    file.

Tips: [reveal] options shape the file: width/height set the slide size (16:9 gives
PowerPoint's Widescreen size, 4:3 its Standard size) and pdfSeparateFragments = false
gives one slide per slide; --scale sets the resolution of the pictures.
```

- Prints `Wrote dist/slides.pptx (9 slides, 1.1 MB)`. Without `-o`, the file goes next to
  where `build` would write the HTML.
- The slides are the pages of `export pdf`, as editable slides: one slide per deck slide,
  per fragment step and per page of a slide taller than a page, so the same `[reveal]`
  options (`pdfSeparateFragments`, `pdfMaxPagesPerSlide`, `width`, `height`) shape the
  file. Text is text boxes (the first heading in the title placeholder), tables are tables,
  images are the deck's image files, backgrounds are colours or pictures; display math,
  SVG, canvases, videos, iframes and gradients are pictures with transparent backgrounds.
  Each slide is named after its heading, with its step and part (`Results, step 2 of 3`);
  the speaker notes are text in the notes pane. The deck's title and author become the
  file's properties.
- `--scale N` sets the resolution of the pictures: `1` is their size on the slide, `2`
  (the default) twice that in each direction, up to `4`.
- Chrome is looked for as for `export pdf`; without one the command exits 3. It gets 120 s
  to load the deck, plus one second per picture.
- A `-o` that does not end in `.pptx` gets a warning.
- The same deck and Chrome version give the same file, byte for byte.
