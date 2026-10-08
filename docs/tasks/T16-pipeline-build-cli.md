# T16 — Pipeline, `build` and `dump` CLI, example deck, end-to-end snapshots

| | |
|---|---|
| Milestone | M1 |
| Size | M |
| Depends on | T10, T11, T12, T13, T14, T15 |
| Parallel-safe with | T30, T31, T32 (notebook frontend plugs in later) |
| Files you own | `src/pipeline.rs`, `src/cli/mod.rs`, `src/cli/build.rs`, `src/cli/dump.rs`, `src/main.rs` (replace T00 stub), `examples/basic/**`, `tests/cli_build.rs`, `tests/snapshots/cli_*.snap` |

## Goal
First end-to-end milestone: `presenter build examples/basic/slides.md` writes a single
self-contained HTML file that opens in a browser as a working reveal.js deck; `presenter dump`
prints the IR or the template context.

## Read first
- `PLAN.md` §3.1, `docs/spec/cli.md` (build, dump, global), `docs/spec/config.md` §1

## API to implement
```rust
// pipeline.rs
pub struct BuildOptions { pub input: PathBuf, pub config_path: Option<PathBuf>, pub cli: CliOverrides,
                          pub output: OutputMode, pub build: BuildInfo, pub missing_asset_is_error: bool }
pub struct Build { pub deck: Deck, pub assets: AssetRegistry, pub theme: Theme, pub config: Config,
                   pub config_path: Option<PathBuf>, pub document: RenderedDocument, pub diagnostics: Diagnostics }
pub fn build_deck(opts: &BuildOptions) -> Result<Build>;
pub fn parse_only(opts: &BuildOptions) -> Result<(Parsed, Config, Option<PathBuf>)>;   // for dump/check
pub fn detect_kind(path: &Path) -> Result<SourceKind>;                                  // by extension
pub fn write_output(build: &Build, out: &Path, mode: &OutputMode) -> Result<Vec<PathBuf>>;  // html + emitted files
pub fn default_output_path(input: &Path, config: &Config, standalone: bool) -> PathBuf;
```
Notebook input: call `notebook::parse` (T31). Until T31 lands, `notebook::parse` is a stub
returning `Error::Other("notebook support arrives in T31")` — add that stub in
`src/notebook/mod.rs` (additive, one function) if T31 has not merged.

## Steps
1. `build_deck`: read file (UTF-8, error with path); `Config::discover` from the input's dir
   unless `--config`; theme search roots `[config dir, input dir, CWD]`; parse; resolve theme
   (`cli.theme` → `meta.theme` → `config.deck.theme`); merge reveal defaults; render; collect diagnostics.
2. `cli/mod.rs`: clap derive per cli.md with global `-v/-vv/-q/--config`; `tracing_subscriber`
   init with `EnvFilter` (`RUST_LOG` fallback). Map `Error` → exit codes (1 build error, 2 usage/config).
   Print diagnostics as `path:line: warning: message` on stderr.
3. `cli/build.rs`: run `build_deck`, `write_output`, print `Wrote dist/slides.html (12 slides, 1.8 MB)`.
   `--watch` is wired by T20 later (accept the flag, error "watch arrives in T20" for now).
4. `cli/dump.rs`: `--context` renders via `build_context` and prints JSON; default prints `Deck`.
5. `examples/basic/`: `slides.md` showcasing every dialect feature (title layout, vertical
   stack, fragments with effects and index, columns, code with line stepping, math inline +
   display, image `figures/plot.png` (commit a small real PNG), notes, background attrs,
   a raw `<video>`/`<iframe>` slide, a table, a footnote), `presenter.toml` with a comment per
   key, `figures/plot.png`, `README.md` with the build/serve commands.
6. `main.rs`: `fn main() -> ExitCode` calling `cli::run()`.

## Acceptance criteria
- `cargo run -- build examples/basic/slides.md` exits 0 and writes `examples/basic/dist/slides.html`
  (gitignored) which: has `<div class="slides">`, exactly the expected number of top-level
  sections, contains `Reveal.initialize(`, contains no `asset:` placeholder, no `/__presenter/`,
  and no `href="http`/`src="http` except from deck content. Verified by `tests/cli_build.rs` with `assert_cmd` into a `tempfile` dir using `-o`.
- The file opens in Chrome and navigates (manual check recorded in PR; T24 automates).
- `presenter build missing.md` → exit 1, stderr contains the path.
- `presenter build examples/basic/slides.md --theme nope` → exit 1 with theme error.
- `presenter dump examples/basic/slides.md` → valid JSON deserializing to `Deck`; `--context` →
  JSON with keys `deck, meta, slides, features, config, reveal, theme, build`.
- `insta` snapshot of the `<div class="slides">…</div>` region of the built example (asset
  URIs redacted with `insta::Settings::filters` on `data:[^"]+`).
- Site mode: `--site -o tmpdir` writes `index.html` + `assets/` with the theme CSS and images, and `index.html` references them relatively.

## Out of scope
`serve` (T20–T23), `check` (T34), `init`/`theme` (T40/T41).

## Hints (verified)
- `assert_cmd::Command::cargo_bin("presenter")`; use `predicates::str::contains`.
- Keep `examples/basic/dist/` out of git (`.gitignore` in T00 covers `/dist`; add `examples/**/dist/`).

## Amendments (coordinator, 2026-10-08)
- `src/cli/mod.rs` defines the **complete** clap surface from `docs/spec/cli.md` (all seven
  commands with all their flags, `theme` subcommands, `export pdf`). Each command lives in its
  own module with `pub fn run(args: &<Cmd>Args, global: &GlobalArgs) -> anyhow::Result<std::process::ExitCode>`
  (or an equivalent signature you choose and document at the top of `cli/mod.rs`). For commands
  owned by later tasks (`serve` T23, `init` T40, `theme` T41, `check` T34, `export` T45) write a
  stub `run` that prints `<command>: not implemented yet` to stderr and exits 2 — those tasks
  replace the stub body only, so they never need to touch `cli/mod.rs`.
- Switch T15's `markdown` template filter to `markdown::render::render_fragment` (additive edit
  in `src/render/`).
- `build_deck` escalates the first `missing-asset` diagnostic to `Error::MissingAsset` when
  `opts.missing_asset_is_error` (ir.md §4 contract change); `build` sets it, `serve` will not.
- Theme resolution: `cli.theme` → `meta.theme` → `config.deck.theme`; then
  `config.merge_theme_reveal(&theme.reveal_defaults())`, then `config.merge_front_matter(&deck.meta)`,
  then `config.apply_cli(...)`.
