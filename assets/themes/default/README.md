# The `default` theme

The built-in theme of presenter, and the root of every theme chain
([`docs/spec/theme-format.md`](../../../docs/spec/theme-format.md)). It is reveal.js's stock
`white` theme for colours, plus `styles/theme.css`: system fonts, a heading at the top of
each slide with left-aligned content, styled code blocks, tables, quotes, alerts,
footnotes and columns, four layouts, and print rules for PDF export.

```
default/
├── theme.toml                 name, reveal_theme = "white", highlight_style = "monokai", [layouts]
├── styles/theme.css           every rule and token below
└── templates/
    ├── deck.html              document shell (blocks head, styles, body_start, slides, scripts, body_end)
    ├── slide.html             one slide or vertical stack; picks the layout
    ├── block.html, cell.html  one content block, one notebook code cell
    ├── layouts/*.html         default, title, section, blank
    └── partials/head.html, partials/scripts.html, partials/logo.html (the title slide's logo)
```

The output never downloads anything: fonts are system font stacks, and the stylesheet has
no `@import` and no remote `url()`.

## Layouts

Pick a layout per slide with `layout=NAME` on the separator or the first heading
(`--- {layout=section}`, `# Thanks {layout=section}`). Every slide's `<section>` gets the class
`layout-<name>`; an unknown name renders like `default` but keeps its own class.

| layout | file | what it does |
|---|---|---|
| `default` | `templates/layouts/default.html` | Heading at the top, content left-aligned, a short accent rule under the slide's first heading. The fallback for unknown names. |
| `title` | `templates/layouts/title.html` | Centred both ways. An optional logo, the first block (the deck heading), then `subtitle`, `author` and `date` from the front matter (author and date fall back to `presenter.toml` `[deck]`), then the rest. |
| `section` | `templates/layouts/section.html` | A section divider: big centred heading, text in `--p-on-accent` on a full-bleed `--p-accent` background. A `background=` on the slide still wins. |
| `blank` | `templates/layouts/blank.html` | No padding and no heading rule. When the slide holds nothing but one image, video or iframe (a Markdown image `![](plot.png)` counts; speaker notes may follow), it fills the whole slide, fitted inside it; for an image that covers the slide edge to edge use `background=`. CSS cannot see text, so a lone paragraph of text with exactly one inline image would be filled the same way. |

`title` and `section` add the class `center` to the slide, which makes reveal centre it
vertically whatever the deck's `center` option says. Every other slide is exactly one slide
high, so it starts at the top even with reveal's default `center: true`. Add `{.center}` to
any slide to centre it both ways. In the overview (<kbd>Esc</kbd>) every thumbnail is one
slide high, with the content of centred slides centred inside it.

**Logo.** The title layout shows a logo when the front matter sets `logo:` or the theme's
`theme.toml` sets `[vars] logo`. The value is a path that `asset()` resolves: a file in a
theme's `assets/` directory (`logo: logo.svg` finds `mytheme/assets/logo.svg`), or
`@mytheme/<path>` relative to that theme's directory. `logo: false` in the front matter hides
a theme's logo, `logo: true` keeps it, and a value that is not a string is ignored. In a
standalone build the logo is inlined like every other asset. `templates/partials/logo.html`
renders it; override that file to place the logo differently.

A logo file next to the deck is **not supported yet**: `logo: img/logo.png` beside
`slides.md` fails the build with `asset("img/logo.png"): unknown path`, reported from
`partials/logo.html`. Put the file in a child theme's `assets/` directory instead
(`mytheme/assets/logo.png` and `logo: logo.png`, see [Extending it](#extending-it)), or show
it in the slide itself with `![](img/logo.png)`, which resolves next to the deck.

A layout is one file. To add one, create `templates/layouts/<name>.html` in your theme; most
layouts extend the default one and override its two blocks:

```jinja
{% extends "layouts/default.html" %}
{% block layout_classes %} center{% endblock %}   {# extra classes on the <section> #}
{% block content %}…{% endblock %}                 {# what goes inside it #}
```

## Tokens

Tokens are CSS custom properties on `:root`. Override them in your theme's stylesheet before
overriding rules. Colours are derived from the text colour (`currentColor`, i.e. reveal's
`--r-main-color`, or the colour reveal gives a slide with a dark or light `background=`), so
they adapt to `reveal_theme = "black"` and to coloured slides without changes. Lengths in
`px` are slide pixels (the deck is 1280×720 unless `[reveal] width/height` say otherwise); `em`
is relative to the surrounding text.

| token | default | purpose |
|---|---|---|
| `--p-accent` | `color-mix(in oklab, #2563eb 75%, currentColor)` | Accent colour: links (`--r-link-color`), list markers, the heading rule, reveal's controls and progress bar, the `section` background. Mixed with the text colour so it stays readable on light and dark backgrounds. |
| `--p-on-accent` | `var(--r-background-color, #fff)` | Text on an accent background (the `section` layout). |
| `--p-muted` | `color-mix(in srgb, currentColor 66%, transparent)` | Secondary text: subtitle, date, footnotes. |
| `--p-surface` | `color-mix(in srgb, currentColor 6%, transparent)` | Subtle fills: zebra table rows, `<kbd>`. |
| `--p-border` | `color-mix(in srgb, currentColor 22%, transparent)` | Hairlines: table rows, quotes, footnote rule, iframes. |
| `--p-radius` | `0.25em` | Corner radius of code blocks, inline code, alerts and keys. |
| `--p-slide-padding` | `40px 60px` | Space inside every slide (not `blank`); kept in PDF export. |
| `--p-code-bg` | `#272822` | Background of the code blocks the highlight style does not colour (`.plain` blocks, `highlight_style = "none"`); monokai's. Highlighted blocks take their background from `highlight_style`. |
| `--p-code-color` | `#f8f8f2` | Text colour of those code blocks. Line numbers are a fainter shade of each block's text colour. |
| `--p-code-font-size` | `0.6em` | Code block text size, relative to the slide text. |
| `--p-code-max-height` | `520px` | Taller code blocks scroll (long lines always scroll sideways; they never wrap). |
| `--p-code-dim-opacity` | `0.5` | Opacity of the lines a line step (`lines="1\|3-4"`) does not highlight; they are dimmed, never hidden (reveal's own value is 0.4). |
| `--p-inline-code-bg` | `color-mix(in srgb, currentColor 9%, transparent)` | Background of `code` in running text. |
| `--p-table-font-size` | `0.75em` | Table text size. |
| `--p-image-max-height` | `480px` | Images and videos are at most this tall so a figure fits under a heading. Exempt: `.r-stretch` images and videos that reveal sizes itself (direct children of the slide, see below). |
| `--p-column-gap` | `1.25em` | Gap between the columns of `:::columns`. |
| `--p-columns` | unset | Number of columns of `:::columns`. Unset: one equal column per `:::column`. Set it (e.g. `2`) to wrap more columns into rows. A `widths="1fr 2fr"` argument overrides both. |
| `--p-alert-note` | `color-mix(in oklab, #2563eb 75%, currentColor)` | `> [!NOTE]` alerts: border, title and tint. |
| `--p-alert-tip` | `color-mix(in oklab, #16a34a 75%, currentColor)` | `> [!TIP]` alerts. |
| `--p-alert-important` | `color-mix(in oklab, #7c3aed 75%, currentColor)` | `> [!IMPORTANT]` alerts. |
| `--p-alert-warning` | `color-mix(in oklab, #d97706 75%, currentColor)` | `> [!WARNING]` alerts. |
| `--p-alert-caution` | `color-mix(in oklab, #dc2626 75%, currentColor)` | `> [!CAUTION]` alerts. |
| `--p-alert-color` | `var(--p-alert-note)` | Set on each `.markdown-alert` from the five tokens above; the alert box reads it. Override the five instead. |
| `--p-cell-gap` | `0.5em` | Vertical space around a notebook `.cell`. |
| `--p-cell-border` | `1px solid rgba(128, 128, 128, 0.4)` | Frame of a notebook `.cell` and its output area. |
| `--p-output-max-height` | `40vh` | Taller notebook cell outputs scroll. |

### reveal variables the theme overrides

reveal's stock themes expose their settings as `--r-*` custom properties
(`css/theme/template/settings.scss`). The default theme keeps the colours of the chosen
`reveal_theme` (`--r-background-color`, `--r-main-color`, `--r-heading-color`, …) and
overrides these:

| variable | value |
|---|---|
| `--r-main-font` | system UI font stack (`-apple-system`, `Segoe UI`, `system-ui`, Roboto, …, `sans-serif`) |
| `--r-heading-font` | `var(--r-main-font)` |
| `--r-code-font` | system monospace stack (`ui-monospace`, `SF Mono`, `Cascadia Mono`, Menlo, Consolas, …, `monospace`) |
| `--r-main-font-size` | `36px` |
| `--r-block-margin` | `20px` (reveal's own default, set here as well so `reveal_theme = "none"` works) |
| `--r-heading-text-transform` | `none` (the stock themes use upper case) |
| `--r-heading-letter-spacing`, `--r-heading-font-weight`, `--r-heading-line-height`, `--r-heading-margin` | `-0.015em`, `700`, `1.15`, `0 0 0.5em 0` |
| `--r-heading1-size`, `--r-heading2-size`, `--r-heading3-size`, `--r-heading4-size` | `2.2em`, `1.45em`, `1.15em`, `1em` |
| `--r-link-color`, `--r-link-color-hover`, `--r-link-color-dark` | derived from `--p-accent` |

## What the stylesheet styles

| markup | source | style |
|---|---|---|
| `section.layout-<name>` | every slide | see Layouts |
| `.columns > .column` | `::::columns` / `:::column` | CSS grid, `--p-columns`, `--p-column-gap` |
| `pre > code` | code fences | colours from `highlight_style` (the code tokens for blocks it leaves alone), size and scrolling tokens; line numbers and dimmed (never hidden) lines of `lines="1\|3-4"` steps |
| `code` | `` `inline` `` | `--p-inline-code-bg` |
| `table` | GitHub tables | `--p-table-font-size`, zebra rows; a table wider than the slide scrolls sideways |
| `blockquote` | `> quote` | rule on the left, italic |
| `.markdown-alert.markdown-alert-{note,tip,important,warning,caution}` | `> [!NOTE]` … | tinted box in the alert colours |
| `.footnotes`, `sup.footnote-ref` | `[^1]` | small, muted, under a rule |
| `.small`, `.large`, `.center` | `:::small`, `{.center}` on a slide, … | 0.7×, 1.3× text; centred text (and the whole slide when on a slide) |
| `kbd`, `mark` | `<kbd>S</kbd>`, `<mark>` | key caps, highlighter |
| `img`, `video`, `iframe` | images, raw HTML | fit the slide and keep the alignment of the text around them (`:::center` centres a figure); `.r-stretch` see below |
| `[data-math-style]` | `$…$`, `$$…$$` | KaTeX; wide display math scrolls instead of overflowing |
| `section.has-dark-background`, `section.has-light-background` | `background=` colours | white or dark text, and every derived token with it |
| `aside.notes` | `:::notes` | hidden on the slide (reveal); shown in the speaker view (<kbd>S</kbd>) |
| `.cell`, `.cell-*`, `.output*` | notebooks | structural rules only (framing, hidden parts) |

reveal's own layout helpers work with the slide padding:

- `.r-stretch` (or `.stretch`) on an element that is a **direct child of the slide** fills the
  height left over, as in reveal, and at most the width inside the padding (images and
  videos keep their aspect ratio). That covers raw HTML on a line of its own, e.g.
  `<img class="r-stretch" src="plot.png">`, an `<iframe class="r-stretch">` or a fenced code
  block with `{.r-stretch}`. A Markdown image `![](plot.png){.r-stretch}` is wrapped in a
  paragraph, which reveal does not stretch: it stays an ordinary image, capped at
  `--p-image-max-height`.
- `.r-fit-text` text is fitted to its parent's width including the padding, so a slide that
  holds fit text directly drops its side padding and the text spans the slide, as in
  reveal.
- `r-stack`, `r-hstack` and `r-vstack` are left alone.

## Print and PDF

`?print-pdf` (and `presenter export pdf`) prints one slide per page, or one page per
fragment step with reveal's default `pdfSeparateFragments: true`. The theme keeps backgrounds,
code blocks and fills (`print-color-adjust: exact`), keeps the slide padding that reveal's
print view would reset, and keeps slides one slide high so top-aligned slides stay top-aligned
and title and section slides stay centred on the page.

## Extending it

Create a child theme and override what you need; everything else comes from `default`:

```sh
presenter theme new ./mytheme        # theme.toml (extends = "default") and styles/theme.css
presenter build talk.md --theme ./mytheme
```

A child's `styles.entries` are added after the parent's, so its rules and tokens win. A
complete `mytheme/styles/theme.css`:

```css
:root {
  --p-accent: color-mix(in oklab, #c2410c 75%, currentColor); /* orange, readable on any background */
  --r-main-font: "Iowan Old Style", Georgia, serif;           /* headings follow the main font */
  --p-slide-padding: 48px 72px;
  --p-code-font-size: 0.65em;
}
.reveal .slides section.layout-section h1 {
  font-style: italic;
}
```

- **Dark slides:** set `reveal_theme = "black"` under `[styles]` in `mytheme/theme.toml`; the
  tokens follow the text colour, so nothing else has to change.
- **Another code style:** `highlight_style = "zenburn"` (or a path to a highlight.js CSS file)
  under `[styles]`; highlighted blocks take that style's background and text colours. Set
  `--p-code-bg` and `--p-code-color` as well to colour the blocks it leaves alone (`.plain`)
  to match.
- **Templates:** put a file with the same name in `mytheme/templates/` to replace one, or
  extend the original: `{% extends "@default/layouts/title.html" %}`.
- **A complete copy** to edit freely: `presenter theme eject default ./mytheme`.

## SCSS

A theme's `[styles] entries` may be `.scss` or `.sass` files as well as `.css`. presenter
compiles them with [grass](https://crates.io/crates/grass) (Dart Sass compatible, built into
the binary; no Node.js) and adds the result to the one theme stylesheet in entry order,
parent first. `examples/custom-theme-scss/` is a complete example.

The usual way to write one is to derive it from reveal.js's own theme template, exactly
like reveal's stock themes are:

```scss
// mytheme/styles/theme.scss   (theme.toml: entries = ["styles/theme.scss"], reveal_theme = "none")
@use "reveal/theme/template/settings" with (
  $background-color: #0b1021,
  $main-color: #e6e6e6,
  $heading-font: "Georgia, serif",
  $link-color: #ffb454
);
@use "reveal/theme/template/theme";
@use "reveal/theme/template/mixins";

.reveal strong { color: #ffb454; }
```

Set `reveal_theme = "none"` when the SCSS builds the whole reveal theme; otherwise the stock
theme's CSS is included as well (before the theme stylesheet). The settings become `--r-*`
custom properties on `:root`, after the default theme's `styles/theme.css` (parent entries
come first), so they win over the values in the table above; set
`$heading-text-transform: none` and the like to keep the default theme's look.

To use a stock theme unchanged, write `@use "reveal/theme/white";` (any of the 14 names).
Its settings are already configured, so it cannot be combined with
`@use "reveal/theme/template/settings" with (…)`: copy the stock theme's settings into your
own `with (…)` instead.

**What Sass can load.** `@use`, `@forward` and `@import` see the theme chain, not the file
system. A URL may be written with or without its extension (`"palette"`,
`"palette.scss"`); every row also applies to the `.scss`/`.sass`/`.css` form:

| URL | finds |
|---|---|
| `"palette"`, `"parts/box"` | next to the file that loads it (`_palette.scss`, `palette.scss`, `palette.sass`, `palette.css`, `palette/index.scss`, …), then in the `styles/` directory of each theme of the chain, child first |
| `"reveal/theme/template/settings"`, `…/theme`, `…/mixins` | reveal.js's theme template, shipped with presenter (`reveal/…` is reveal's `css/` directory) |
| `"reveal/theme/<name>"` | a stock reveal theme's source |
| `"@<theme>/styles/x"` | a file of that theme of the chain |
| `"../…"` | the same tree: `..` can step into another theme of the chain (`../../@parent/styles/x` from `styles/`) or the embedded vendor files (`../../vendor/…`), never onto the disk |
| absolute paths, anything else | nothing: a theme cannot read other files |

**CSS imports are inlined.** Sass leaves CSS imports (`@import url("x.css")`,
`@import "x.css"`) in its output, and so do `.css` entries; presenter replaces every
top-level import of a relative URL with the imported file, so a standalone deck needs
nothing else. In a `.css` file the import is looked up next to that file. In Sass, CSS
imports are looked up next to the **entry** file, also when they are written in a partial
(Sass passes them through without their location), as for `url()`s below. Last, for
`reveal/…` and `fonts/…`, they are looked up in reveal's theme sources:
`@import url("reveal/theme/fonts/source-sans-pro/source-sans-pro.css")` brings reveal's
Source Sans Pro, and the stock themes' own `@import url('./fonts/…')` works too. Imports
nest at most 5 levels and must not form a cycle; an import with media queries becomes an
`@media` block; `layer()` and `supports()` imports cannot be inlined (an error). Imports of
`https://…` URLs stay as written but move to the top of the theme stylesheet (CSS ignores
an `@import` after other rules), keeping the media queries of the import that inlined
their file (`@import url(https://…) print;`): the fonts load when the deck is online, and
a standalone build warns `remote-import`. An `@import` inside a block (`@media … { … }`)
is invalid CSS that browsers ignore; it is left as written.

**`url()`s.** Sass does not rewrite URLs, so a `url()` anywhere in the compiled SCSS is
relative to the **entry** file (`styles/theme.scss`), then the chain's `assets/` directories
as for CSS entries, and last, for `reveal/…` and `fonts/…`, reveal's theme sources. A
`url()` in an imported CSS file is relative to that file (reveal's font files are found next
to their CSS); in an imported vendor file (`../../vendor/katex/katex.min.css`) one that
names no file stays as written, like KaTeX's `.woff`/`.ttf` fallbacks. In a standalone build
every font and image becomes a `data:` URI (reveal's fonts are `data:font/woff`).

**Errors** name the file and the position, with the line:

```text
error: style '@mytheme/styles/theme.scss': line 5, column 14: Expected expression.
 5 |   .b { color: ; }
   |              ^^
```

A partial that cannot be read or is not UTF-8 is an error naming the partial.

`@warn` messages go to the log only, as warnings (they are not diagnostics: `presenter
check --deny-warnings` and the serve overlay do not see them), and `@debug` with `-vv`,
which also shows how long each compilation took (`compiled Sass stylesheet=… elapsed_ms=…`).
`random()` and `unique-id()` are not available: the output is deterministic.

**Live reload.** `presenter serve` watches the theme directory: saving a `.scss` (or
`.sass`, `.css`) file recompiles the theme stylesheet and swaps it in the browser without
reloading the page; a Sass error shows in the browser overlay until it is fixed.
