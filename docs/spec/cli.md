# Spec: Command-line interface

**Status:** contract. `clap` 4 derive, binary name `presenter`. Global flags come first.

```
presenter [-v|-vv] [-q] [--config PATH] <COMMAND>

Commands:
  build    Render a deck to HTML
  serve    Render, watch and live-reload in the browser
  init     Scaffold a new presentation directory
  theme    Inspect, eject or create themes
  dump     Print the IR or the template context as JSON
  check    Lint a deck without building
  export   Export to other formats (pdf)
```

## build
```
presenter build <INPUT> [-o/--out PATH] [--theme NAME|PATH] [--standalone|--site] [--watch]
```
- `INPUT`: `.md` or `.ipynb` (detected by extension; `--format md|ipynb` overrides).
- Default output: `<build.out_dir>/<input stem>.html` (standalone) or `<out_dir>/<stem>/index.html` + `assets/` (site).
  `-o` sets the file (standalone) or the directory (site).
  (An `out_dir` set in `presenter.toml` is relative to that file; the default `dist` is relative to
  the input's directory, config.md §2, audit F09.)
- `--watch`: rebuild on change without serving (no browser). Shares T20's watcher.
- Exit codes: 0 ok · 1 build error (parse/template/missing asset) · 2 usage/config error.
- Stdout: nothing except `Wrote <path> (<n> slides, <size>)` on success. Diagnostics on stderr.
- The output never overwrites one of the build's inputs (audit F01). Every file to be written
  (`-o`, the default path, a site's `<out>/index.html` and `assets/`) is compared by file identity
  with the input, the `presenter.toml` in use, the deck's local assets and the files of a theme
  directory in use (its `theme.toml` and its other top-level files except `.html`, `.htm` and
  `.pdf` files, which may be earlier outputs; everything in its `templates/`, `styles/`, `assets/`
  and `scripts/`; the files its `theme.toml` names; and every other file of it that the build
  read (contract change, W8FIX 2026-10-09), such as an image its stylesheet reaches with
  `url("../img/x.png")`, a file a template names with `asset()` or a Sass partial outside
  `styles/`), so a symbolic or hard link and another
  spelling of the name on a case-insensitive file system count too, and so does a path that
  reaches the file once its missing directories are created (`new/../link/../x.md`). A clash
  exits 2 with `error: refusing to write <out>: it is the input file <input>; choose another
  output path with -o` (`the config file`, `the deck's asset`, `the theme file`; for the default
  path: `…; it is the default output path ([build] out_dir); choose another one with -o`) and
  writes nothing. `--watch` checks the input and `presenter.toml` before it starts watching, and
  ends the same way when its first build finds a clash; a later rebuild that would overwrite an
  input (an image the deck starts to use) prints that error and watching goes on. A standalone
  `-o` that names a directory (a trailing `/`, a last component `.` or `..`, or an existing
  directory) exits 2 as well (audit F41).
- A build that fails on missing assets reports each of them as a located error with its source
  line, in source order (audit F37).

## serve
```
presenter serve <INPUT> [--theme …] [--port N] [--host H] [--open] [--site-preview]
```
- Serves at `http://host:port/`. Port 0 means pick a free port; the chosen URL is printed as
  the first line on stdout (tests rely on this).
- `--open` opens the browser (`open` crate).
- Rebuild errors do not stop the server; they are shown in the browser overlay and on stderr.
- Ctrl-C exits 0.

## init
```
presenter init [DIR] [--name NAME] [--notebook] [--full-theme]
```
- Creates `DIR` (default `.`, must be empty or not exist): `presenter.toml`, `slides.md`
  (or `slides.ipynb` with `--notebook`), `theme/` extending `default` with one overridden
  layout and a `theme.css` that sets a few CSS variables, `.gitignore` with `dist/`.
- `--full-theme` ejects the full default theme instead of a child theme.
- Prints next steps. Exit 2 if `DIR` is non-empty.

## theme
```
presenter theme list                       # built-in themes + the resolved theme for CWD if any
presenter theme eject <NAME> <DIR>         # copy a built-in theme's files to DIR (DIR must not exist)
presenter theme new <DIR> [--extends NAME] # minimal child theme skeleton
presenter theme check <DIR>                # validate theme.toml, chain, required templates
```
- Exit codes, the same rule for every subcommand (audit F42): 0 ok · 1 `theme check` found
  problems, or a theme that exists is broken (invalid `theme.toml`, a missing ancestor) · 2 a
  NAME or DIR argument that names nothing usable: not a built-in theme (`eject`), an `--extends`
  that is neither a theme directory nor a built-in theme (`new`), a `check` DIR that does not
  exist, is not a directory or has no `theme.toml`, an existing `eject` DIR, a non-empty `new` DIR.
- `eject` (and `init --full-theme`) copy the built-in theme's `README.md` (its CSS and templates
  refer to it) with a note on top: the copy is the theme `<name>`, and the text documents the
  built-in theme. Its links into presenter's repository keep their text but lose the link, which
  would break in the copy (audit F48).

## dump
```
presenter dump <INPUT> [--context] [--theme …] [--pretty]
presenter dump --default-config
```
- Default: `Deck` IR as JSON. `--context`: the full template context (theme resolved).
- Always pretty-printed with sorted keys unless `--compact`. `--context` keeps its top-level keys
  in theme-format.md §4 order (`deck, meta, slides, features, config, reveal, theme, build`); the
  keys below them are sorted (audit F46).
- `--default-config` (no INPUT; INPUT, `--context`, `--theme`, `--pretty` and `--compact` with it are
  usage errors, exit 2; `--config` is ignored) prints every `presenter.toml` setting with its built-in
  default as TOML on stdout (`Config::default()` with `[reveal]` filled from the built-in reveal
  defaults). Every value line is commented out (`#key = value`); a key without a default is a
  `# key: unset` line; a header explains that the text changes nothing as it is. Saved as
  `presenter.toml` it therefore builds exactly like no file (T46; docs/guide/configuration.md is
  generated from it and kept in sync by tests/guide.rs).

## check
```
presenter check <INPUT>... [--format text|json] [--deny-warnings]
```
- Runs the frontend and the lints (markdown-dialect.md §10, notebook-mapping.md §4 notes,
  theme-format.md layouts), for the input and for every file it includes (markdown-dialect §4.1),
  each reported with its own path. Text format: `path:line: level: message`. Exit 0 with no
  warnings, 0 with warnings unless `--deny-warnings` (then 1), 1 on errors.
- `-q` hides warnings in the text format, except when `--deny-warnings` makes them fail the run:
  then they are printed, so the failure says why (audit F24). JSON always lists everything.

## export
```
presenter export pdf <INPUT> [-o FILE] [--chrome PATH] [--theme …]
```
- Builds standalone HTML to a temp file, opens `file://…?print-pdf` in headless Chrome
  (`--chrome`, `$CHROME_PATH`, or well-known locations on macOS/Linux), writes the PDF.
  Exit 3 when no Chrome is found, with install hints.
- The PDF never overwrites an input either (as for `build`, audit F01): exit 2, checked before
  Chrome is looked for (the input, `presenter.toml`) and before it starts (the deck's assets, the
  theme's files). A `-o` that names a directory exits 2; a `-o` that does not end in `.pdf` gets a
  warning once every check has passed, just before Chrome starts.
- The PDF is not byte-for-byte reproducible: Chrome stamps the export time and its version into
  it (the PLAN goal 7 exception, audit F21). `export pdf --help` lists this under Limitations.

> **Clarification (W8FIX, 2026-10-09; audit F15 as A5 resolved it):** a slide taller than a page
> continues on extra pages. The default theme sets the lines of code blocks and notebook outputs
> about 5 % closer in print, to leave room for the line Chrome moves at each page break, while
> reveal.js reserves a slide's pages from its height on screen; so a slide that ends within a
> line or two past a page boundary on screen ends a page earlier in print, and its last page is
> empty but for the slide's background (about one in ten to twenty of the slides that span
> pages; nothing is lost). `export pdf --help` lists this under Limitations, and the theme
> README's "Print and PDF" section explains it. Dropping that page would need a print-time
> step in `export pdf` (re-measuring each slide with the print rules before Chrome prints),
> which is not implemented.

## Global behaviour
- `-v` info, `-vv` debug (`tracing-subscriber` with `EnvFilter`; `RUST_LOG` also honoured).
- `--config PATH` bypasses `presenter.toml` discovery.
- Colors on stderr when it is a TTY (`anstream`/`owo-colors` or clap's own styling helpers).
- Every command accepts `--theme` where a deck is involved.
- Paths in messages (`Wrote …`, `path:line:` diagnostics, error reports, `check --format json`)
  are relative to the current directory, with `..` for a path outside it when that is no longer
  than the absolute path; a symbolic link in a path's prefix does not keep it absolute, and a `..`
  in a path is resolved as the file system resolves it (after a link it leaves the directory the
  link points to), so the message names the file really read or written (audit F39). Contract
  change (W8FIX, 2026-10-09): a path is resolved only up to its last `..`; the names after it are
  shown as typed, so a final symbolic link is not replaced by its target and the letter case is
  kept (`-o ../w/alias.html` is `alias.html` from `w/`), and a symbolic link to a directory
  among them is kept too (`../decks-link/talk.md`, as `check` shows it), unless resolving it
  gives a shorter relative path (the rule for a link in a path's prefix).
