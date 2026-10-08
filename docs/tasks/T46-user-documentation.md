# T46 — User documentation

| | |
|---|---|
| Milestone | M4 |
| Size | M |
| Depends on | M1–M3 complete (T17, T23, T33, T34 merged) |
| Parallel-safe with | T47 |
| Files you own | `docs/guide/**`, `README.md` |

## Goal
A user who has never seen the repo can install, write a deck, use a notebook, make a theme
and export a PDF from the docs alone — and the docs match the implementation, not the plan.

## Steps
1. `docs/guide/getting-started.md` (install via `cargo install --path .` and release binaries, `init`, `serve`, `build`).
2. `docs/guide/authoring.md`: the full dialect from `docs/spec/markdown-dialect.md` rewritten for users with
   rendered examples (screenshots from `examples/basic`), including the setext pitfall and `check`.
3. `docs/guide/notebooks.md`: slide types in JupyterLab's property inspector, tags, front matter in the first cell, what outputs render, execution advice, widgets/plotly limitations.
4. `docs/guide/themes.md`: theme directory, tokens, layouts, extending, ejecting, SCSS with reveal's template, template context (`dump --context`), live-reload workflow.
5. `docs/guide/configuration.md`: every key with default (generate the table from `Config::default()` serialized to TOML to avoid drift — add `presenter dump --default-config` if needed, additive in `cli/dump.rs`).
6. `docs/guide/cli.md`: generated from `--help` output (paste and keep in sync; add a test that `--help` output matches the doc's code blocks, or document the manual step).
7. `README.md`: pitch, 30-second demo, feature list, links.

## Acceptance criteria
- Every command in the guide runs as written against the examples (do it; note results).
- No reference to task ids or "planned" features; stretch items listed under "Not yet".
- Links checked (`lychee` locally or a quick script); images present.
