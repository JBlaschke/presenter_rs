# T00 — Repository scaffold, CI, vendoring, embedded assets

| | |
|---|---|
| Milestone | M0 |
| Size | M |
| Depends on | — |
| Parallel-safe with | nothing (everything depends on this) |
| Files you own | `Cargo.toml`, `Cargo.lock`, `.gitignore`, `README.md`, `rustfmt.toml`, `clippy.toml`, `.github/workflows/ci.yml`, `scripts/vendor.sh`, `assets/vendor/**`, `assets/presenter/*.js` (placeholders), `src/main.rs`, `src/lib.rs`, `src/vendor.rs`, every `src/**/mod.rs` stub listed below, `tests/vendor_smoke.rs` |

## Goal
A compiling, CI-green crate skeleton with all module files present (so later tasks never
edit `lib.rs`), reveal.js 6.0.2 and KaTeX 0.19.0 vendored and embedded, and a `--version`
that prints.

## Read first
- `PLAN.md` §3.2 (layout), §3.3 (dependencies), §6 (conventions)
- `docs/spec/vendored-assets.md` (exact file list, embedding)

## Steps
1. `cargo init --name presenter` (edition 2024, `rust-version = "1.85"`), lib + bin:
   `src/lib.rs` declares every module from PLAN §3.2; create each `mod.rs`/file with only a
   `//!` doc comment so the crate compiles. `src/main.rs`: clap `Cli` with `--version` and a
   placeholder `build`/`serve` subcommand that returns "not implemented" (exit 2).
2. Dependencies (versions verified 2026-10-08; use these):
   ```toml
   [dependencies]
   anyhow = "1"
   thiserror = "2"
   clap = { version = "4.6", features = ["derive", "wrap_help"] }
   serde = { version = "1", features = ["derive"] }
   serde_json = { version = "1", features = ["preserve_order"] }   # NOTE: IR uses BTreeMap/Map; preserve_order keeps front-matter key order in `vars`
   toml = "1.1"
   serde_norway = "0.9"
   comrak = { version = "0.56", default-features = false, features = ["attributes"] }
   minijinja = { version = "2.24", features = ["loader", "json", "loop_controls"] }
   rust-embed = { version = "8.13", features = ["include-exclude"] }
   notify = "8.2"
   notify-debouncer-full = "0.7"
   tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "signal", "time", "fs"] }
   axum = "0.8"
   tracing = "0.1"
   tracing-subscriber = { version = "0.3", features = ["env-filter"] }
   base64 = "0.23"
   blake3 = "1"
   mime_guess = "2"
   html-escape = "0.2"
   ansi-to-html = "0.2"
   regex = "1"
   walkdir = "2"
   open = "5"
   [dev-dependencies]
   insta = { version = "1.49", features = ["json", "filters", "redactions"] }
   assert_cmd = "2"
   predicates = "3"
   tempfile = "3"
   ureq = "3"
   ```
   `grass` (T42), `lol_html` (T43), `headless_chrome` (T24) are added by those tasks.
3. `scripts/vendor.sh` (bash, `set -euo pipefail`): download both tarballs to a temp dir,
   verify SHA-256 against values hard-coded in the script, extract, copy exactly the files in
   `vendored-assets.md` §2 into `assets/vendor/{reveal,katex}/`, delete anything else, write
   `assets/vendor/VERSIONS.toml`. Running it twice produces no git diff. Document usage at
   the top of the script. Commit the vendored files.
4. `src/vendor.rs`: the `rust-embed` struct and helpers from `vendored-assets.md` §4.
   Create empty `assets/presenter/math.js` and `assets/presenter/live-reload.js` (comment
   only) and an empty `assets/themes/default/.gitkeep` so include globs match.
5. `README.md`: one paragraph, status "pre-alpha, see PLAN.md". `LICENSE` already exists
   (MIT, Johannes Blaschke, decided 2026-10-08); set `license = "MIT"` in `Cargo.toml` and keep
   the vendored reveal.js and KaTeX `LICENSE` files alongside their assets (both MIT).
6. `.github/workflows/ci.yml`: on push/PR, ubuntu + macos: `cargo fmt --check`,
   `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and a job that
   runs `scripts/vendor.sh` and fails on `git diff --exit-code` (vendoring drift).
7. `rustfmt.toml` (`edition = "2024"`), `clippy.toml` (empty or `too-many-arguments-threshold = 8`).
8. `.gitignore`: `/target`, `/dist`, `.DS_Store`, `*.snap.new`.

## Acceptance criteria
- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` pass locally and in CI on both OSes.
- `cargo run -- --version` prints `presenter 0.1.0`.
- `tests/vendor_smoke.rs`: `vendor::get("vendor/reveal/dist/reveal.js")` is `Some` with length > 100 000;
  `vendor::get("vendor/katex/katex.min.js")` is `Some`; `vendor::list("vendor/reveal/dist/theme/")` has 14 entries;
  `vendor::get("vendor/reveal/dist/plugin/markdown.js")` is `None` (not vendored).
- `scripts/vendor.sh` run twice → `git status --porcelain` empty.
- Total size of `assets/vendor/` is below 8 MB.

## Out of scope
Any real functionality. Do not write IR types (T01) or theme files (T02).

## Hints (verified)
- reveal 6 moved plugins to `dist/plugin/<name>.js`; `dist/plugin/highlight.js` is 921 KB and bundles highlight.js.
- `dist/theme/*.css` already inline their fonts as `data:font/woff;base64,…` — copy as-is.
- rust-embed with `#[folder = "assets/"]` resolves relative to `CARGO_MANIFEST_DIR`. Debug builds read from disk at runtime; keep that (do **not** enable `debug-embed`) so theme editing live-reloads in `cargo run`.
- `tar` in the npm tarball has a top-level `package/` directory.

## Amendments (coordinator, 2026-10-08)
- Create a stub file for **every** module the plan will ever need, so later tasks never edit
  module declarations: everything in PLAN §3.2 *plus* `src/markdown/compat.rs`,
  `src/markdown/postprocess.rs` (T43), `src/theme/styles.rs` (T42), `src/cli/report.rs` (T44),
  `src/check.rs`, `src/pdf.rs`, and `src/cli/{mod,build,serve,init,theme,dump,check,export,report}.rs`.
  Each `mod.rs` (and `lib.rs`) declares its submodules with `pub mod …;` and a `//!` doc comment.
- `src/serve/watch.rs` additionally defines the shared contract type used by T20 and T21:
  ```rust
  /// What kind of rebuild a change batch requires (live-reload.md §2).
  #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "lowercase")]
  pub enum ChangeKind { /// Only stylesheets/theme assets changed.
      Css, /// Anything else.
      Content }
  ```
- Add `examples/**/dist/` to `.gitignore`.
- Do not edit `docs/tasks/README.md`; the coordinator maintains the status table.
