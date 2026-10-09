# T48 — `:::include`: compose a deck from Markdown and notebook files

| | |
|---|---|
| Milestone | M5 (post-plan feature, requested 2026-10-09) |
| Size | M |
| Depends on | everything merged on `main` (all of M0–M4 and the audit fixes) |
| Parallel-safe with | nothing (single task) |
| Files you own | `src/markdown/include.rs` (new; declare it in `src/markdown/mod.rs`), `src/markdown/mod.rs`, `src/markdown/render.rs` (additive), `src/ir.rs` (the two fields below only), `src/pipeline.rs` (additive, if needed), `src/serve/watch.rs` (watch set), `src/check.rs` (lint included files), `src/notebook/convert.rs` (the `include-in-notebook` warning only), `tests/include.rs`, `tests/fixtures/include/**`, `examples/mixed/**`, `docs/guide/authoring.md` and `docs/guide/notebooks.md` (an "Including files" section / cross-reference), `CHANGELOG.md` (one Unreleased line), `docs/tasks/README.md` is NOT yours |

## Goal
Let a Markdown deck pull in whole notebooks or other Markdown decks at horizontal slide
boundaries, so a talk can mix authored slides with an executed notebook without copying
content around.

## Read first
- `docs/spec/markdown-dialect.md` §4.1 (**this is your spec**) and §2.2 (separators), §2.3 (headings mode)
- `docs/spec/ir.md` §1 (`Deck.includes`, `SourceSpan.file` — contract change 2026-10-09), §2, §4
- `docs/spec/live-reload.md` §5 (watch set), `docs/spec/cli.md` (check)
- The code: `src/markdown/mod.rs` (`parse`, how slides are built from `split_with_source`),
  `src/notebook/mod.rs` (`parse`), `src/pipeline.rs` (`parse_only`, `prepare`, `build_deck`,
  `detect_kind`), `src/serve/watch.rs` (`watch_set_for*`), `src/check.rs` (raw-source lints),
  `src/assets/registry.rs` (`merge`)

## Steps
1. IR: add `Deck.includes: Vec<PathBuf>` and `SourceSpan.file: Option<PathBuf>` exactly as in
   ir.md §1 (serde `default` + `skip_serializing_if`), so every existing snapshot and
   `tests/fixtures/ir/sample-deck.json` round-trip stay byte-identical. Update constructors and
   `Deck::invariant_violations` if needed (an included slide's span may name another file).
2. `src/markdown/include.rs`: recognise an include slide on the split result (a top-level
   `BlockDirective` whose name is `include`, alone among the slide's visible nodes); resolve the
   path against the including file's directory; parse the target with `markdown::parse` or
   `notebook::parse` (by extension) using the including deck's effective config; discard its
   `DeckMeta` (warning `include-meta-ignored` for differing title/subtitle/author/date/description/lang/theme/reveal);
   tag every slide (and child) with `span.file`; merge assets (`AssetRegistry::merge`),
   features and diagnostics; record `includes` transitively. Depth ≤ 8, cycle detection by
   canonical path, errors as specified (`Error::Parse` at the directive line, path = including file).
   Every misuse in §4.1 (vertical slide, nested, not alone, non-empty body, missing file,
   unknown extension) has a precise message.
3. Wire it into `markdown::parse` (both rules and headings mode). `:::include` inside a notebook
   markdown cell: warning `include-in-notebook`, renders nothing.
4. Watch set: `watch_set_for`, `watch_set_before_build` and `watch_set_for_failed` include every
   file in `Deck.includes` (and, for a failed build, whatever includes could be resolved).
5. `check`: run the raw-source lints (separator pitfalls, etc.) on every included Markdown file and
   the notebook lints on every included notebook, reported with the included file's path and
   line; drop the old `unknown-directive` warning for `include`.
6. Serve: an error inside an included file shows in the overlay with that file's path, line and
   excerpt (the error machinery already keys excerpts by path — verify).
7. `examples/mixed/slides.md`: a short Markdown deck that includes `../notebook/analysis.ipynb`
   and a sibling `part2.md`; it must build and `check` with zero warnings. Docs: an "Including
   files" section in `docs/guide/authoring.md` (syntax, horizontal-only rule, what is ignored,
   watch behaviour) and a pointer from `docs/guide/notebooks.md`; keep `tests/guide.rs` green
   (relative links/anchors are checked). One CHANGELOG line.

## Acceptance criteria
- `tests/include.rs` (with fixtures under `tests/fixtures/include/`): a Markdown master that
  includes a notebook and a Markdown part yields the expected slide order and vertical children;
  included slides carry `span.file`; `Deck.includes` is correct and transitive; assets from both
  files resolve (standalone build has no `asset:` placeholder left and both images inline);
  features merge (math/code/notebook); included front matter title/theme are ignored with
  `include-meta-ignored`.
- Every misuse in markdown-dialect §4.1 is tested: include next to content, in a vertical slide,
  nested in a directive/list, non-empty body, missing file, unknown extension, cycle (A→B→A),
  depth > 8, include in a notebook cell, attributes on the include slide (warning).
- Existing snapshots and the IR round-trip test are unchanged.
- `presenter build examples/mixed/slides.md` works standalone and `--site`; open it with the CDP
  helper: reveal initializes, the notebook's cells/outputs/plot render, KaTeX and highlighting
  work, no console errors; screenshot a slide from each source.
- `presenter serve examples/mixed/slides.md` (on a temp copy): editing the included notebook or
  `part2.md` triggers a rebuild and the browser updates; a syntax error in `part2.md` shows in
  the overlay with `part2.md` and its line (manual check with the CDP helper, recorded).
- `presenter check examples/mixed/slides.md` reports nothing; a deliberate setext `--` pitfall in
  an included `.md` is reported with that file's path and line.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`, and
  `cargo test --features browser-tests --test browser` pass.

## Out of scope
Including part of a file, includes inside slides (mixed Markdown/notebook slides), remote URLs,
includes from notebooks.
