# T17 — Default theme v1 (design, layouts, code, notes, print)

| | |
|---|---|
| Milestone | M1 |
| Size | L |
| Depends on | T02, T16 |
| Parallel-safe with | T20–T24, T31, T32, T40, T41 |
| Files you own | `assets/themes/default/**` (templates, styles, README.md), `examples/basic/slides.md` (may extend for visual coverage), `docs/screenshots/*.png` |

## Goal
Make the built-in theme something people want to present with, and make it the reference
for how a theme is structured: tokens as CSS custom properties layered over reveal's
`--r-*` variables, clean layouts, good code blocks, sane defaults for images, tables and
columns, and print/PDF rules.

## Read first
- `docs/spec/theme-format.md` §5–§7, `assets/vendor/reveal/css/theme/template/settings.scss` (the `--r-*` variables reveal exposes)
- reveal.js docs on layouts (`r-stack`, `r-fit-text`, `r-stretch`) — the theme must not fight them

## Steps
1. Tokens in `styles/theme.css` `:root`: `--p-accent`, `--p-muted`, `--p-code-bg`,
   `--p-code-font-size`, `--p-column-gap`, `--p-slide-padding`, `--p-cell-border`, plus
   overrides of `--r-main-font`, `--r-heading-font`, `--r-main-font-size`, `--r-link-color`
   using system font stacks (no webfont downloads: offline output).
2. Layouts: `default` (left-aligned content, heading top), `title` (centered, subtitle/author/date,
   optional `meta.vars.logo` via `asset()` when set), `section` (big centered heading, accent
   background), `blank` (no padding, for full-bleed images/iframes). Each is one file in
   `templates/layouts/`.
3. Content rules: `.columns` grid (`grid-template-columns: repeat(var(--p-columns, 2), 1fr)`,
   `:::columns` with `widths=` already sets inline style), images max-height to fit 720 px
   with `r-stretch` compatibility, tables with zebra rows and smaller font, blockquotes, `> [!NOTE]`
   alerts (comrak `alerts` output classes `markdown-alert markdown-alert-note` — style them),
   footnotes small, `.small/.large/.center` directive classes, `kbd`.
4. Code: `pre code` font size via token, line-number colors readable on monokai, highlighted
   line steps (`.hljs-ln-line` visibility) not hidden; `data-trim` wrapping off, horizontal
   scroll for long lines; inline `code` subtle background.
5. Notes: nothing visible in the deck; make sure the speaker view (`S`) shows notes — reveal
   handles it; just test it.
6. Print: `@media print` and reveal's `.reveal.print-pdf` — hide fragments stacking issues,
   ensure backgrounds print (`-webkit-print-color-adjust: exact`).
7. `README.md` in the theme: every token with default and purpose; the layout list; how to
   extend (`presenter theme new ./mine`), with a 10-line example child `theme.css`.
8. Screenshots: `chrome --headless=new --screenshot=docs/screenshots/<name>.png --window-size=1280,720 "file://…/dist/slides.html#/<n>"`
   for the title, a code slide, a columns slide and a section slide. Commit them.

## Acceptance criteria
- `presenter build examples/basic/slides.md` renders every example slide without horizontal
  overflow at 1280×720 (check `document.documentElement.scrollWidth` per slide in headless
  Chrome, or by eye; record which).
- Switching `reveal_theme` between `white` and `black` in a child theme keeps the default theme readable (tokens derive from `--r-main-color` etc. where possible).
- Tokens documented in the README match the CSS (`grep -- '--p-' styles/theme.css` vs README list; add a test in `tests/theme_tokens.rs` that every `--p-*` defined in CSS appears in README).
- No webfont `@import` or `url(http…)` in the theme CSS.
- Snapshot tests from T16 updated with intention (review the diff in the PR).

## Out of scope
Notebook cell design (T33), SCSS conversion (T42). Additional themes beyond `default`.

## Hints (verified)
- reveal 6 stock themes expose `--r-background-color`, `--r-main-font`, `--r-main-font-size`, `--r-main-color`, `--r-heading-*`, `--r-link-color*`, `--r-selection-*`, `--r-block-margin`, `--r-code-font` as CSS custom properties on `:root`; overriding them in a later stylesheet works without SCSS.
- Our theme stylesheet is always loaded last (theme-format.md §5), so plain overrides win.
