# T43 — Heading-split mode, incremental lists, reveal compat comments

| | |
|---|---|
| Milestone | M4 |
| Size | M |
| Depends on | T11, T12 |
| Parallel-safe with | T40–T42, T44–T47 |
| Files you own | `src/markdown/split.rs` (headings mode; T11 is done), `src/markdown/compat.rs`, `src/markdown/postprocess.rs`, `Cargo.toml` (`lol_html`, additive), `tests/fixtures/md/headings-mode.md`, `tests/fixtures/md/reveal-compat.md`, `examples/headings-mode/**` |

## Goal
Lower the migration cost from pandoc-style decks and from nbconvert/reveal Markdown:
heading-driven slide boundaries, incremental lists, and the `<!-- .slide: -->`,
`<!-- .element: -->` and `Note:` conventions.

## Read first
- `docs/spec/markdown-dialect.md` §2.2 (row 4), §2.3, §3.3 (element attributes), §7 (Note:), `docs/spec/config.md` (`slides.split`, `slide_level`, `incremental_lists`)

## Steps
1. `SplitMode::Headings { slide_level }` in `split`: walk top-level nodes; `Heading(level)`
   with `level < slide_level` → new horizontal slide (the heading stays as the slide's first node);
   `level == slide_level` → new vertical slide; separators still honored. Leading content before
   the first heading → initial slide only if it contains a non-blank node.
2. `compat.rs`:
   - `<!-- .slide: k="v" .class #id -->` as the first node (`HtmlBlock` literal, after optional
     whitespace) → `parse_attr_block` on the inner text (support `k="v"` and `k=v`) → merged
     into the slide's attrs (precedence per dialect §3.2); node removed.
   - `Note:`/`Notes:` paragraph → cut the slide body there; render the rest as notes (prepend to
     existing notes from directives). Only in `RenderMode::Deck`.
   - `<!-- .element: … -->` after an element: implement as the reveal Markdown plugin does —
     over the **rendered HTML**: regex for `<!--\s*\.element:\s*(.*?)-->` and apply attrs to the
     nearest preceding opening tag at the same nesting depth (walk back to the last `<tag` whose
     closing has not been seen; use lol_html if simpler: a comment handler records the attrs, the
     next `end tag` of the enclosing element applies them — pick one, test it on lists/paragraphs/images).
3. `postprocess.rs` with `lol_html`: `incremental_lists` → for each top-level `ul`/`ol` directly
   under the slide root (selector `:scope > ul > li` is not supported by lol_html; instead rewrite
   `ul > li, ol > li` **only** when the slide HTML's root-level lists are known: wrap the slide
   HTML in `<div id="__root">` before rewriting and use selector `#__root > ul > li, #__root > ol > li`,
   then strip the wrapper) adding class `fragment` unless the list has class `no-incremental`;
   also honor per-slide class `.incremental`/`.no-incremental` overriding the config.
4. Fixtures and `examples/headings-mode/` (a pandoc-style deck with `presenter.toml` setting `split = "headings"`).

## Acceptance criteria
- `headings-mode.md`: `#` → horizontal, `##` → vertical, `###` → content; with `slide_level = 3` the shape changes accordingly; mixed `---` still works; IR snapshot.
- `reveal-compat.md`: `<!-- .slide: data-background="#f00" class="x" -->` → `attrs["data-background"] == "#f00"` (verbatim, not normalized… **decide**: run the key through `normalize_key` so `background=` shorthand also works and document) and class `x`; `Note:` splits notes; `- item <!-- .element: class="fragment" -->` → the `<li>` has `class="fragment"`; `![](x.png) <!-- .element: width="300" -->` → `<img … width="300">`.
- `incremental_lists = true` adds `fragment` to top-level `li`s only, not to nested lists; `.no-incremental` slide class disables; `.incremental` slide class enables when config is false.
- All existing snapshots unchanged (compat is opt-in by syntax; headings mode by config).

## Out of scope
pandoc `:::` div attribute syntax beyond what directives already give.

## Hints (verified)
- `lol_html` 3: `rewrite_str(html, RewriteStrSettings { element_content_handlers: vec![element!("sel", |el| { el.set_attribute(...)?; Ok(()) })], ..Default::default() })`; `el.get_attribute("class")`.
- reveal's own Markdown plugin regex for element attributes: `<!--\s*?\.element\s*?(.*?)\s*?-->` applied to the preceding tag; mirror its semantics.
