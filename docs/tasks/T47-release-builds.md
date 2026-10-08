# T47 — Release builds

| | |
|---|---|
| Milestone | M4 |
| Size | S |
| Depends on | T16 |
| Parallel-safe with | T40–T46 |
| Files you own | `.github/workflows/release.yml`, `Cargo.toml` (`[package]` metadata, `[profile.release]`), `build.rs` (optional git sha), `CHANGELOG.md` |

## Goal
Tagging `v*` produces downloadable binaries for macOS (arm64, x86_64) and Linux (x86_64,
arm64), and `presenter --version` identifies the exact build.

## Steps
1. `[profile.release]`: `lto = "thin"`, `codegen-units = 1`, `strip = true`; `opt-level = 3`.
   Measure binary size (expect ~15–25 MB with embedded assets) and startup time.
2. `build.rs`: embed `git describe --always --dirty` into `PRESENTER_GIT_SHA` (fallback `unknown` when git is absent);
   `--version` prints `presenter 0.1.0 (abc1234)`.
3. `release.yml`: on tag push, matrix build with `cross`/native runners (`macos-14` for arm64, `macos-13` for x86_64,
   `ubuntu-22.04` for x86_64, `ubuntu-22.04-arm` for arm64), run `cargo test` first, package `tar.gz` with `LICENSE`,
   `README.md`, the binary; upload to a GitHub Release with SHA-256 sums. Consider `cargo-dist` if it reduces YAML; document the choice.
4. `CHANGELOG.md` with `Unreleased` section; release process notes in `docs/dev/release.md`.

## Acceptance criteria
- A tag on a fork/test branch produces four artifacts (verify once; link the run in the PR).
- `cargo build --release` works offline after `cargo fetch` (no build-time downloads).
- `presenter --version` output format tested in `tests/cli_build.rs` (regex).

## Amendments (coordinator, 2026-10-08)
- T16 already routes `--version` through `cli::version()`, which reads `PRESENTER_GIT_SHA` at
  compile time; extend T00's `build.rs` (keep its rerun-if-changed logic). There is no git remote,
  so the release workflow cannot run here: validate it statically (actionlint if available, or
  careful review) and mark the "four artifacts" criterion as not verifiable, with the reason.
