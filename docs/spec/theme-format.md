# Spec: Theme format, inheritance and template context

**Status:** contract. Implemented by T13 (loading), T15 (rendering helpers), T02/T17
(default theme), T41 (tooling), T42 (SCSS). Read by anyone writing a theme.

A theme is a directory. The built-in theme `default` lives in `assets/themes/default/`
and is embedded in the binary. User themes are directories on disk that usually
`extends = "default"` and override a few files.

## 1. Directory layout

```
mytheme/
├── theme.toml                 manifest (required)
├── templates/                 minijinja templates (any may be omitted in a child theme)
│   ├── deck.html              whole document
│   ├── slide.html             one slide or vertical stack; dispatches to a layout
│   ├── layouts/default.html   section markup for layout "default" (required in root theme)
│   ├── layouts/title.html     optional additional layouts, one file per layout name
│   ├── layouts/section.html
│   ├── block.html             renders one Block (html or cell)
│   ├── cell.html              a notebook code cell
│   └── partials/*.html        optional includes (head.html, scripts.html, …)
├── styles/
│   ├── theme.css              or theme.scss (T42); entry points listed in theme.toml
│   └── …                      files @imported/@used from the entry points
├── assets/                    fonts, logos, images; referenced from CSS by relative url() or from templates via asset("name")
└── scripts/                   optional JS appended before Reveal.initialize (listed in theme.toml)
```

## 2. `theme.toml`

```toml
name = "mytheme"                  # required; used in error messages and `theme list`
extends = "default"               # optional: built-in theme name, or path relative to this file
description = "Dark theme for data talks"

[styles]
entries = ["styles/theme.css"]    # compiled in order and concatenated into ONE stylesheet (§5)
reveal_theme = "white"            # stock reveal theme CSS to include first: any of
                                  # black, white, league, beige, night, serif, simple, solarized,
                                  # moon, dracula, sky, blood, black-contrast, white-contrast, or "none"
highlight_style = "monokai"       # "monokai" | "zenburn" | "none" | a path relative to this file

[scripts]
entries = []                      # e.g. ["scripts/extra.js"]; included after plugins, before Reveal.initialize

[reveal]                          # reveal defaults for this theme (lowest precedence after built-ins)
transition = "fade"
slideNumber = "c/t"

[layouts]                         # optional documentation of layouts this theme (or its parents) provide;
title = "Centered title slide"    # `check` warns about layout= values not present as files in the chain
section = "Section divider"

[vars]                            # free-form; exposed to templates as theme.vars.*
accent = "#2a76dd"
```

Inheritance: child values override parent values key by key; `styles.entries` and
`scripts.entries` are **concatenated parent-first** (a child adds CSS after its parent's,
so cascade order is natural). `reveal_theme`/`highlight_style` are plain overrides.
`[reveal]` tables deep-merge (child wins). `[vars]` deep-merge.

## 3. Resolution and the template chain (`src/theme/resolve.rs`, T13)

Resolving `meta.theme` / `--theme` / config `deck.theme` (first set wins — see config.md):
1. If the value is a path to a directory containing `theme.toml` (relative to the config
   file, else to the source file's directory, else CWD): user theme.
2. Else if it names a built-in theme (`default`; later possibly more): embedded theme.
3. Else `Error::Theme { message: "not found" }` listing where we looked.

Follow `extends` recursively to build `chain: Vec<ThemeDir>` ordered child → … → root.
Cycles and depth > 8 are errors. The root must provide `deck.html`, `slide.html`,
`block.html`, `cell.html`, `layouts/default.html`.

Template lookup (`Environment::set_loader`): for name `N`, return the first existing
`templates/N` walking the chain child → root. Names prefixed `@<theme-name>/` skip the
search and read from that specific theme (used by a child to extend its parent's file:
`{% extends "@default/layouts/default.html" %}`). Embedded themes read from `rust-embed`;
disk themes read from disk every time (no caching) so `serve` always sees edits.
Set `env.set_debug(true)` in serve mode and translate `minijinja::Error` to
`Error::Template { template, line, message }`.

Asset lookup (`asset("fonts/x.woff2")`, CSS `url(fonts/x.woff2)`): same walk over
`assets/` directories; `vendor/...` names come from the embedded vendor tree.

## 4. Template context

The JSON object passed to `deck.html` (and visible to `presenter dump --context`):

```jsonc
{
  "deck":   { /* the full IR, see ir.md */ },
  "meta":   { /* = deck.meta */ },
  "slides": [ /* = deck.slides, each decorated with "layout_template": "layouts/title.html" (resolved, falls back to layouts/default.html) */ ],
  "features": { "math": true, "code": true, "notes": false, "notebook": false },
  "config": { /* effective merged Config (config.md) */ },
  "reveal": {
    "config_json": "{\"hash\":true,\"transition\":\"fade\"}",   // merged options, serialized, WITHOUT plugins
    "plugins": ["RevealHighlight", "RevealNotes", "RevealZoom", "RevealSearch"],   // JS identifiers, in order
    "plugin_scripts": ["vendor/reveal/dist/plugin/highlight.js", "vendor/reveal/dist/plugin/notes.js", …]
  },
  "theme": { "name": "mytheme", "chain": ["mytheme", "default"], "reveal_theme": "white",
             "highlight_style": "monokai", "vars": { "accent": "#2a76dd" }, "scripts": ["scripts/extra.js"] },
  "build": { "mode": "build" | "serve", "standalone": true, "live_reload": false,
             "version": "0.1.0", "id": 17 /* serve rebuild counter, 0 in build */ }
}
```

Helper functions (registered on the environment by T15; all return HTML marked safe):

| helper | standalone build | site build | serve |
|---|---|---|---|
| `style(path)` | `<style>…contents…</style>` with `url()` assets inlined as `data:` | `<link rel="stylesheet" href="assets/<hashed>">` and file copied | `<link rel="stylesheet" href="/__presenter/asset/<path>">` |
| `script(path)` | `<script>…</script>` | `<script src="assets/<hashed>"></script>` | `<script src="/__presenter/asset/<path>"></script>` |
| `asset(path)` | `data:` URI string | `assets/<hashed>` string | `/__presenter/asset/<path>` string |
| `theme_style()` | `<style id="presenter-theme">…compiled theme CSS…</style>` | `<link id="presenter-theme" rel="stylesheet" href="assets/theme-<hash>.css">` | `<link id="presenter-theme" rel="stylesheet" href="/__presenter/theme.css?v=<build id>">` |
| `reveal_init()` | `<script>Reveal.initialize(Object.assign(<config_json>, { plugins: [<plugins>] }));</script>` | same | same |
| `live_reload()` | empty | empty | `<script src="/__presenter/live-reload.js"></script>` |

`path` resolution for `style`/`script`/`asset`: `vendor/…` → embedded vendor tree
(`docs/spec/vendored-assets.md`); `presenter/…` → `assets/presenter/` (our JS);
anything else → theme chain `assets/` directories (child first). Unknown path → `Error::Template`.

> **Contract change (coordinator, 2026-10-08, from T02/T13):** §2 lets themes name files
> relative to the theme directory (`scripts.entries = ["scripts/extra.js"]`, a path-valued
> `highlight_style`), which the three rules above cannot reach. A fourth form is added:
> `@<theme-name>/<path>` resolves `<path>` relative to that theme's **root directory** (not
> `assets/`), inside that theme only (no `..` escapes). The context builder (T15) emits
> `theme.scripts` entries and a path-valued `theme.highlight_style` in this form (chain order,
> parent first for scripts), so templates pass them to `script()`/`style()` unchanged. One
> resolver implements all four forms (`render::resolve_helper_path`, T15) and the dev server's
> `/__presenter/asset/{*path}` route (T21) uses the same function.

Filters: minijinja built-ins plus `markdown` (render a string as inline Markdown, notebook
mode), `json` (built-in), `attrs` (render a `{k: v}` map as ` k="v"` pairs, escaped).
Globals: `now` is deliberately **not** provided (determinism).

Auto-escaping is on for `.html` templates. IR HTML strings are inserted with `| safe`.

## 5. Styles

- All `styles.entries` of the chain, parent-first, are compiled (`.css` passthrough,
  `.scss` via grass — T42) and concatenated into a single stylesheet. It is always the
  **last** stylesheet in `<head>` and carries `id="presenter-theme"` so the live-reload
  client can swap exactly one `<link>`.
- Relative `url(...)` references in theme CSS resolve against the CSS file's own directory
  first, then the chain's `assets/` directories. They are rewritten to `asset:<id>`
  placeholders and resolved by the emitter like any other asset (inlined in standalone).
- `@import url(...)` in theme CSS is **not** followed in M1; T42 adds inlining of local
  `@import`s so reveal's font CSS works.
- CSS custom properties the default theme defines are documented in `assets/themes/default/README.md`
  (T17). Themes are encouraged to override variables before overriding rules.

## 6. Class and markup conventions the default theme guarantees (for CSS authors)

| selector | meaning |
|---|---|
| `section.layout-<name>` | every slide section gets `layout-<layout>` |
| `.columns > .column` | grid columns from `:::columns` |
| `.fragment` | reveal fragment (from `:::fragment` or notebook fragments) |
| `.cell`, `.cell-input`, `.cell-outputs`, `.cell-prompt` | notebook cell framing (`cell.html`) |
| `.output`, `.output-stream.stdout`, `.output-stream.stderr`, `.output-error`, `.output-display`, `.output-text`, `.output-image`, `.output-json` | notebook outputs |
| `.cell.hide-input .cell-input`, `.cell.hide-output .cell-outputs` | hidden parts (CSS `display:none`, so a theme can choose to show them faintly instead) |
| `aside.notes` | speaker notes (hidden by reveal) |
| `[data-math-style]` | KaTeX targets |

## 7. Template responsibilities (default theme; children may override any file)

- `deck.html`: document shell. Blocks: `head`, `styles`, `body_start`, `slides`,
  `scripts`, `body_end`. Includes `slide.html` for each top-level slide.
- `slide.html`: if `slide.children` is non-empty, emit `<section>` wrapper containing the
  slide itself rendered via its layout, then each child via its layout (`{% with slide = child %}`);
  otherwise just the layout. Never more than two levels.
- `layouts/<name>.html`: emits exactly one `<section …>` with id/class/attrs via the `attrs`
  filter, iterates `slide.blocks` through `block.html`, and emits `<aside class="notes">`
  when `slide.notes`. Receives `slide` in scope.
- `block.html`: `{% if block.type == "html" %}{{ block.html|safe }}{% else %}{% include "cell.html" %}{% endif %}`.
- `cell.html`: `<div class="cell …">` with prompt, input `<pre><code class="language-{{cell.language}}" data-trim>{{ cell.source }}</code></pre>`, outputs loop by `kind`.

## 8. Debugging workflow for theme authors
1. `presenter dump deck.md --context > ctx.json` to see exactly what templates receive.
2. `presenter serve deck.md --theme ./mytheme`; edit templates/CSS; the browser updates.
3. Template errors show in the browser overlay with file and line (serve mode) and on
   stderr (build mode).
4. `presenter theme eject default ./mytheme-full` to start from a complete copy instead
   of extending.
