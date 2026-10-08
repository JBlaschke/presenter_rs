# Custom theme in SCSS

A child theme of the built-in `default` theme, written in SCSS and derived from reveal.js's
own theme template exactly like reveal's stock themes are
([`docs/spec/theme-format.md`](../../docs/spec/theme-format.md) §5; the SCSS section of
[`assets/themes/default/README.md`](../../assets/themes/default/README.md#scss)).

| File | What it is |
|---|---|
| `slides.md` | The deck; `theme: theme` in its front matter picks the theme directory next to it |
| `theme/theme.toml` | `extends = "default"`, `reveal_theme = "none"`, `entries = ["styles/theme.scss"]` |
| `theme/styles/theme.scss` | The stylesheet: configures `reveal/theme/template/settings`, includes reveal's template rules and mixins, imports reveal's vendored Source Sans Pro, sets the default theme's `--p-*` tokens and adds a few rules |
| `theme/styles/_palette.scss` | A partial with the colours, loaded with `@use "palette" as p` |

## Build and serve

From the repository root:

```sh
# One self-contained HTML file: examples/custom-theme-scss/dist/slides.html
presenter build examples/custom-theme-scss/slides.md

# Live preview; saving a .scss file recompiles the theme and swaps the stylesheet in place
presenter serve examples/custom-theme-scss/slides.md

# The compiled stylesheet and how long grass took
presenter -vv build examples/custom-theme-scss/slides.md 2>&1 | grep "compiled Sass"
```

With a checkout and no installed binary, use `cargo run --` in place of `presenter`.
`dist/` is not committed.

## How it works

- `@use "reveal/theme/template/settings" with (…)` loads reveal.js's
  `css/theme/template/settings.scss`, which presenter ships (`reveal/…` names reveal's
  Sass sources). Every setting becomes a `--r-*` custom property on `:root`.
- `@use "reveal/theme/template/theme"` adds reveal's theme rules, which read those
  properties; `@use "reveal/theme/template/mixins"` provides `light-bg-text-color()`.
- `@import url("reveal/theme/fonts/source-sans-pro/source-sans-pro.css")` is a plain CSS
  import. presenter inlines it, and the four `.woff` fonts it declares become `data:` URIs
  in a standalone build, so the file needs no network.
- The default theme's `styles/theme.css` comes first (parent entries first), so this
  theme's `:root` values win.

The stock themes work the same way: `@use "reveal/theme/white";` alone gives reveal's
`white` theme, fonts included.
