# T41 — `presenter theme list / eject / new / check`

| | |
|---|---|
| Milestone | M4 |
| Size | S |
| Depends on | T13 |
| Parallel-safe with | T40, T42–T47 |
| Files you own | `src/cli/theme.rs`, `tests/cli_theme.rs` |

## Goal
The theme-iteration workflow: see what themes exist, copy a built-in theme to disk to edit
it ("eject"), create a minimal child theme, and validate a theme directory.

## Read first
- `docs/spec/cli.md` (theme); `docs/spec/theme-format.md` §1–§3

## Steps
1. `list`: built-in names (`theme::builtin_names()`), each with its `description` from
   `theme.toml`; if a `presenter.toml` is discoverable from CWD, show its resolved theme and chain.
2. `eject NAME DIR`: copy every embedded file under `themes/NAME/` to `DIR` (must not exist),
   rewriting `name = "NAME"` to the directory's name and removing `extends`-less-ness as-is
   (ejecting `default` yields a root theme). Print how to use it (`--theme DIR` or `deck.theme`).
3. `new DIR [--extends NAME]`: write `theme.toml` (`name`, `extends`), `styles/theme.css` with
   a commented token block, `templates/.gitkeep`. Print next steps.
4. `check DIR`: `theme::resolve(DIR)`; report chain, missing required templates (root only),
   style entries that do not exist, `[layouts]` entries with no template file, unknown keys.
   Exit 1 on problems.

## Acceptance criteria
- `tests/cli_theme.rs`: `list` output contains `default`; `eject default X` creates `X/theme.toml`
  and `X/templates/deck.html`, and `build --theme X` works; `eject default <existing dir>` → exit 2;
  `new Y` then `check Y` exits 0; `check` on a dir without `theme.toml` → exit 1 with a clear message.

## Out of scope
Theme galleries, remote themes.

## Amendments (coordinator, 2026-10-08)
- Replace the `stub_exits_2` test in `src/cli/theme.rs` along with the stub.
