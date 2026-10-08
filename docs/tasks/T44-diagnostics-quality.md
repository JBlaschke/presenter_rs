# T44 — Diagnostics quality

| | |
|---|---|
| Milestone | M4 |
| Size | S |
| Depends on | T16, T22 |
| Parallel-safe with | T40–T43, T45–T47 |
| Files you own | `src/cli/report.rs`, `src/error.rs` (additive: `Error::span()`, `Error::path()`), `src/serve/events.rs` (additive: `excerpt` field), `assets/presenter/live-reload.js` (additive: show excerpt) |

## Goal
Errors tell the author where and what, with a source excerpt, both in the terminal and in
the browser overlay; template errors include the template name and line.

## Steps
1. `report.rs`: `pub fn print_error(err: &Error, source_cache: &dyn Fn(&Path) -> Option<String>)` →
   ```
   error: unknown directive 'include'
     --> slides.md:41
      |
   41 | :::include other.md
      | ^^^
   ```
   (colors when TTY; `--no-color`/`NO_COLOR` honored). For notebooks, show `cell 3, line 2`.
2. Template errors: use `minijinja::Error::display_debug_info()` content in debug/serve mode; always include template name + line.
3. `ErrorPayload.excerpt: Option<String>` (the offending line) and the overlay renders it in a `<pre>`.
4. Panics: install a `std::panic::set_hook` in `main` that prints a one-line "this is a bug, please report with the command line" message and the location; keep the default backtrace behaviour behind `RUST_BACKTRACE`.

## Acceptance criteria
- Snapshot test of `print_error` output for a `Parse` error with line, a `Template` error, a `MissingAsset`, and a `Theme` error (no color).
- Serve: introduce a template syntax error in a child theme → overlay shows template name, line and excerpt (manual, recorded).
- `NO_COLOR=1` disables colors (test by capturing output with `assert_cmd` and asserting no `\x1b[`).
