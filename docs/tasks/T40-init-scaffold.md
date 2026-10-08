# T40 — `presenter init` scaffold

| | |
|---|---|
| Milestone | M4 |
| Size | S |
| Depends on | T16 (T41 helpful for `--full-theme`) |
| Parallel-safe with | T41–T47 |
| Files you own | `src/cli/init.rs`, `assets/scaffold/**`, `src/vendor.rs` (additive `#[include = "scaffold/**/*"]`), `tests/cli_init.rs` |

## Goal
A new user gets a working, nicely commented project in one command, including a child
theme they can start editing immediately.

## Read first
- `docs/spec/cli.md` (init); `docs/spec/theme-format.md` §1–§2; `docs/spec/config.md` §2

## Steps
1. `assets/scaffold/`: `presenter.toml` (every key commented, defaults shown), `slides.md`
   (a 6-slide starter that demonstrates separators, a fragment, columns, code, notes and the
   title layout), `slides.ipynb` (3 cells with slideshow metadata, for `--notebook`),
   `theme/theme.toml` (`extends = "default"`), `theme/styles/theme.css` (sets 3 tokens with
   comments), `theme/templates/layouts/title.html` (extends `@default/layouts/title.html`
   and adds a footer line with `meta.vars.event`), `.gitignore` (`dist/`), `README.md`.
   Placeholders `{{name}}` replaced at copy time (plain string replace; do not run minijinja on scaffold files).
2. `cli/init.rs`: validate target (empty or absent), copy files, `--notebook` swaps the deck file,
   `--full-theme` uses T41's eject (or copies `themes/default/**` directly if T41 is not merged),
   print next steps (`cd DIR && presenter serve slides.md`).

## Acceptance criteria
- `tests/cli_init.rs`: `init` into a tempdir then `build slides.md` inside it exits 0 and the
  output references the child theme's token values; `init` into a non-empty dir → exit 2;
  `--notebook` produces a buildable `slides.ipynb` (requires T31; mark the assertion conditional if not merged).
- The scaffold `slides.md` passes `presenter check` with no warnings.

## Out of scope
Interactive prompts.

## Amendments (coordinator, 2026-10-08)
- T34 (`check`) and T41 (`theme eject`) run in parallel with you: copy `themes/default/**` directly
  for `--full-theme`, and verify "the scaffold passes `check` with no warnings" by building it with
  zero diagnostics (and with `check` if it exists in your worktree). Replace the `stub_exits_2` test.
