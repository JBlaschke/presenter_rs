# T02 — Default theme skeleton rendering the IR fixture

| | |
|---|---|
| Milestone | M0 |
| Size | M |
| Depends on | T01 |
| Parallel-safe with | T10, T11, T13, T14, T30 |
| Files you own | `assets/themes/default/**`, `tests/theme_skeleton.rs` |

## Goal
A complete, minimal built-in theme (manifest, every required template, a plain CSS file)
that renders `tests/fixtures/ir/sample-deck.json` into a working reveal.js page, verified
in a browser by hand, before the real renderer (T15) exists. This fixes the template
structure that T15's helpers and T17's design build on.

## Read first
- `docs/spec/theme-format.md` (all; §4 context, §6 class conventions, §7 template responsibilities)
- `docs/spec/ir.md` §6, `docs/spec/vendored-assets.md` §2

## Steps
1. `assets/themes/default/theme.toml`: `name = "default"`, no `extends`,
   `styles.entries = ["styles/theme.css"]`, `reveal_theme = "white"`, `highlight_style = "monokai"`,
   `[layouts]` documenting `default`, `title`, `section`, `blank`.
2. Templates (`templates/`): `deck.html`, `slide.html`, `block.html`, `cell.html`,
   `layouts/{default,title,section,blank}.html`, `partials/head.html`, `partials/scripts.html`.
   Follow theme-format.md §7 exactly; use the helper names from §4 (`style(...)`,
   `script(...)`, `theme_style()`, `reveal_init()`, `live_reload()`, `attrs` filter) even
   though T15 implements them later.
   - `layouts/default.html` emits `<section{{ slide.id and (' id="' ~ slide.id ~ '"') or '' }} class="layout-{{ slide.layout }} {{ slide.classes|join(' ') }}"{{ slide.attrs|attrs }}>`, the block loop, and `<aside class="notes">`.
   - `layouts/title.html` extends `layouts/default.html` and overrides a `content` block to
     add `meta.subtitle`/`meta.author`/`meta.date` under the heading when present.
   - `cell.html` renders per theme-format.md §7 with `kind`-based output loop; `hide-input`/`hide-output` classes.
3. `styles/theme.css`: only structural rules required for correctness (`.columns` grid,
   `.cell` framing, `[data-math-style="display"]` block display, hidden parts) and a short
   list of CSS custom properties with defaults. Visual design is T17.
4. `tests/theme_skeleton.rs` harness (no production code): `minijinja::Environment` with
   `path_loader("assets/themes/default/templates")`; register stub helpers returning
   `<link rel="stylesheet" href="../../assets/{path}">`/`<script src="../../assets/{path}">`
   (relative links so the preview opens from `target/preview/`), `theme_style()` → inline
   `<style>` with the CSS file contents, `reveal_init()` → `Reveal.initialize({hash:true, plugins:[RevealHighlight, RevealNotes]})`,
   `live_reload()` → empty, `attrs` filter. Context: the fixture `Deck` plus hand-written
   `reveal`, `theme`, `build`, `config` objects per §4, and `layout_template` added to each
   slide. Write the result to `target/preview/skeleton.html`.
5. Open `target/preview/skeleton.html` in a browser: slides navigate, vertical stack works,
   notes open with `S`, code is highlighted, the title layout shows subtitle/author.
   Record what you checked in the PR.

## Acceptance criteria
- The harness test passes and asserts: number of top-level `<section` elements that are
  direct children of `.slides` equals `deck.slides.len()`; a `<section>` wrapper exists for the
  slide with children; `aside class="notes"` present exactly for slides with notes; `class="layout-title"` present;
  the cell template emitted `class="cell` once per `Block::Cell`.
- Every template referenced by another exists; `theme.toml` parses with `toml`.
- No helper other than those listed in theme-format.md §4 is used.
- Manual browser check recorded in the PR.

## Out of scope
Visual design (T17), notebook styling (T33), SCSS (T42), the real helper implementations (T15).

## Hints (verified)
- minijinja: `{% with slide = child %}…{% endwith %}` and `{% include slide.layout_template %}`
  (dynamic include names) are supported; `{% extends %}` with `{% block %}` works with the
  `multi_template` default feature.
- Auto-escaping: `Environment::set_auto_escape_callback(|name| name.ends_with(".html"))`
  must be on in the harness so that `{{ block.html|safe }}` is the only unescaped path.
