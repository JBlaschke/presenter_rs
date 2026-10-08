# T34 — `presenter check` and lints

| | |
|---|---|
| Milestone | M3 |
| Size | M |
| Depends on | T16, T31 |
| Parallel-safe with | T33, T40–T45 |
| Files you own | `src/check.rs`, `src/cli/check.rs`, `tests/cli_check.rs` |

## Goal
Fast feedback without building: collect diagnostics from the frontends and add deck-level
lints, print them as `path:line: level: message` or JSON, with exit codes per the CLI spec.

## Read first
- `docs/spec/cli.md` (check); `docs/spec/markdown-dialect.md` §10; `docs/spec/notebook-mapping.md` §4 notes; `docs/spec/theme-format.md` §2 (`[layouts]`)

## Steps
1. `check::run(opts: &BuildOptions) -> Result<Diagnostics>`: `pipeline::parse_only`, resolve theme,
   then lints over the `Deck`:
   - `empty-slide`, `orphan-vertical` (already produced by T11 → pass through)
   - `separator-not-recognized`: scan the raw source lines equal to `---`/`--` (trimmed) that are not inside fenced code
     and compare with slide `span.start_line`s; report the rest (dialect §2.2 setext pitfall).
   - `unknown-layout`: `slide.layout` not in `theme.layouts()`.
   - `unknown-slide-attr`: produced by T11 as a diagnostic; pass through.
   - `duplicate-slide-id`.
   - `missing-asset` (from frontends; `check` never errors on it).
   - notebooks: `output-script`, `output-unsupported`, `notebook-no-outputs` (code cells all without outputs → hint to execute),
     `unknown-slide-type`.
2. Text formatter with colors when stderr is a TTY; `--format json` → `[{level, code, message, path, line, cell}]`.
3. Exit codes: errors → 1; warnings → 0 unless `--deny-warnings` → 1.

## Acceptance criteria
- `tests/cli_check.rs`: a fixture with a setext `--` pitfall reports `separator-not-recognized` at the right line;
  `layout=nope` reports `unknown-layout`; two `{#same}` ids report `duplicate-slide-id`;
  `examples/basic/slides.md` and `examples/notebook/analysis.ipynb` report **no** warnings (keep them clean);
  `--deny-warnings` flips the exit code; `--format json` parses.
- `check` on a missing file → exit 1 with the path.

## Out of scope
Fixing issues automatically; theme file validation (T41 `theme check`).
