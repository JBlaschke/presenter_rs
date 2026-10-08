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
- `--watch`: rebuild on change without serving (no browser). Shares T20's watcher.
- Exit codes: 0 ok · 1 build error (parse/template/missing asset) · 2 usage/config error.
- Stdout: nothing except `Wrote <path> (<n> slides, <size>)` on success. Diagnostics on stderr.

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

## dump
```
presenter dump <INPUT> [--context] [--theme …] [--pretty]
```
- Default: `Deck` IR as JSON. `--context`: the full template context (theme resolved).
- Always pretty-printed with sorted keys unless `--compact`.

## check
```
presenter check <INPUT>... [--format text|json] [--deny-warnings]
```
- Runs the frontend and the lints (markdown-dialect.md §10, notebook-mapping.md §4 notes,
  theme-format.md layouts). Text format: `path:line: level: message`. Exit 0 with no
  warnings, 0 with warnings unless `--deny-warnings` (then 1), 1 on errors.

## export
```
presenter export pdf <INPUT> [-o FILE] [--chrome PATH] [--theme …]
```
- Builds standalone HTML to a temp file, opens `file://…?print-pdf` in headless Chrome
  (`--chrome`, `$CHROME_PATH`, or well-known locations on macOS/Linux), writes the PDF.
  Exit 3 when no Chrome is found, with install hints.

## Global behaviour
- `-v` info, `-vv` debug (`tracing-subscriber` with `EnvFilter`; `RUST_LOG` also honoured).
- `--config PATH` bypasses `presenter.toml` discovery.
- Colors on stderr when it is a TTY (`anstream`/`owo-colors` or clap's own styling helpers).
- Every command accepts `--theme` where a deck is involved.
