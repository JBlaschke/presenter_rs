# The `default` theme

The built-in theme of presenter, and the root of every theme chain
([`docs/spec/theme-format.md`](../../../docs/spec/theme-format.md)). It is reveal.js's stock
`white` theme for colours, plus `styles/theme.css`: system fonts, a heading at the top of
each slide with left-aligned content, styled code blocks, tables, quotes, alerts,
footnotes and columns, four layouts, and print rules for PDF export.

```
default/
├── theme.toml                 name, reveal_theme = "white", highlight_style = "monokai", [layouts]
├── styles/theme.css           the tokens and rules below
├── styles/notebook.css        the notebook cells' design (see Notebook cells)
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
| `--p-code-max-height` | `520px` | Taller code blocks scroll (long lines always scroll sideways; they never wrap). Printed, they are shown in full ([Print and PDF](#print-and-pdf)). |
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
| `--p-cell-border` | `1px solid rgba(128, 128, 128, 0.4)` | Frame of a notebook cell's output area, under its input. More notebook tokens: [Notebook cells](#notebook-cells). |

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
| `li > input[type=checkbox]` | task lists: `- [ ] open`, `- [x] done` | no bullet: a box sized with the text takes its place, in `--p-accent`, with a tick when done; the item's text lines up with the other items' |
| `img`, `video`, `iframe` | images, raw HTML | fit the slide and keep the alignment of the text around them (`:::center` centres a figure); `.r-stretch` see below |
| `[data-math-style]` | `$…$`, `$$…$$` | KaTeX; wide display math scrolls instead of overflowing |
| `section.has-dark-background`, `section.has-light-background` | `background=` colours | white or dark text, and every derived token with it |
| `aside.notes` | `:::notes` | hidden on the slide (reveal); shown in the speaker view (<kbd>S</kbd>) |
| `.cell`, `.cell-*`, `.output*` | notebook code cells | see [Notebook cells](#notebook-cells) |

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

## Notebook cells

A Jupyter notebook's code cells are rendered by `templates/cell.html` and styled by
`styles/notebook.css` (Markdown cells are ordinary slide content). A cell reads as a card:
the input is a highlighted code block, and the outputs sit in a frame under it.
[`examples/notebook/`](../../../examples/notebook/) shows every part.

```html
<div class="cell" data-cell-index="7">                <!-- + fragment, hide-input, hide-output -->
  <div class="cell-input">
    <div class="cell-prompt">In [3]:</div>           <!-- when [notebook] show_prompts -->
    <pre><code class="language-python" data-trim>…</code></pre>
  </div>
  <div class="cell-outputs">                          <!-- only when the cell has outputs -->
    <pre class="output output-stream stdout">…</pre>  <!-- or stderr -->
    <div class="output output-display" data-mime="text/html">
      <div class="cell-prompt">Out [3]:</div>        <!-- execute results only -->
      <table class="dataframe">…</table>              <!-- or img.output-image, pre.output-text, … -->
    </div>
    <pre class="output output-error">…</pre>          <!-- the traceback -->
    <div class="output output-unsupported">No renderable output (…)</div>
  </div>
</div>
```

- **Prompts** (`In [n]:`, and `Out [n]:` on execute results) are small labels in
  `--p-prompt-color`. `show_prompts = false` under `[notebook]` in `presenter.toml`, or
  `notebook: {show_prompts: false}` in the front matter, leaves them out.
- **Outputs** taller than `--p-output-max-height` scroll inside their frame, and lines longer
  than the slide scroll it sideways (they never wrap). In print they are shown in full (see
  [Print and PDF](#print-and-pdf)). Images keep their size, or the width and
  height the notebook recorded, and shrink to fit the frame instead of scrolling it.
- **Streams:** stdout is plain text; stderr is tinted with `--p-stderr-color`.
- **Errors:** the traceback sits in a box with a bar in `--p-error-color` and a tint of it.
- **DataFrames** (pandas' `table.dataframe`, and any HTML table in a result): compact text
  (`--p-cell-font-size`), numbers right-aligned in tabular figures, the index column bold and
  left-aligned. The header rows (two for a named index) stay in view at the top of the frame
  while it scrolls, on the deck's background colour (`--r-background-color`). On a slide with
  its own background (a `background=` colour, image, video or gradient) they scroll with the
  table instead, since the slide's colour is not known to the stylesheet.
- **Rendered Markdown and LaTeX** results keep the slide's text size.
- **`hide-input`** (tag, or `metadata.jupyter.source_hidden`): the outputs become slide
  content, without frame or prompts. Hidden inputs and outputs stay in the markup with
  `display: none`, so a theme can show them faintly instead.
- **`fragment`** (slide type or tag): the whole cell is one reveal fragment.
- **ANSI colours** in streams and tracebacks (text and background, SGR 30–37, 40–47, 90–97
  and 100–107, and reverse video, SGR 7) are spans that read one of 16 variables, the
  converter's names rather than `--p-` tokens: `--black`, `--red`, `--green`, `--yellow`,
  `--blue`, `--magenta`, `--cyan`, `--white`, and `--bright-black` … `--bright-white`. So are
  8-bit colours 0–15 (`38;5;n`, `48;5;n`); the other 8-bit colours and RGB colours
  (`38;2;r;g;b`, `48;2;r;g;b`) are written as hex values.
  - *Text on the slide*: `.cell-outputs` sets all 16: the six colours mixed with the text
    colour, so they stay readable as text on light and dark slides; each bright colour equal
    to the normal one; black, white and their bright variants as shades of the text colour.
    Set them on `.reveal .cell-outputs` to change them. These mixes are meant for text only.
  - *Text on an ANSI background*, and reverse video, sits on a fixed colour instead. There
    the 16 variables are a terminal palette, for the background and for the text: red,
    green, blue, magenta, cyan and bright black are mid-tones on which black, white and
    `--white` (all but white) text all read, and which read on black and white; yellow and
    the other bright colours are light, for black text. Text in an ANSI colour keeps that
    colour: from this palette, or its 8-bit or RGB value. Other text is black or white,
    whichever reads on the background: SGR 47 is black on white, 41 white on red, 7 black
    on white, 31;7 white on red. An 8-bit or RGB background is a hex value CSS cannot read,
    so there the converter writes that black or white itself (`48;5;236` and `38;5;241;7`
    are white on grey, `38;5;100;7` black on olive). Text in an ANSI colour on another one
    (`31;43`, `31;48;5;250`, `38;5;100` on any background) is not adjusted and may not read,
    as in a terminal. The palette is set by the rules after `.output-error` in
    `styles/notebook.css`.

| token | default | purpose |
|---|---|---|
| `--p-cell-font-size` | `0.5em` | Text size of a cell's code, its text outputs and its tables, relative to the slide text. |
| `--p-prompt-color` | `color-mix(in srgb, currentColor 50%, transparent)` | Colour of the `In [n]:` and `Out [n]:` prompts. |
| `--p-stderr-color` | `var(--p-alert-warning)` | Text and background tint of stderr output. |
| `--p-error-color` | `var(--p-alert-caution)` | Bar and background tint of an error's traceback. |
| `--p-output-max-height` | `288px` | Taller outputs scroll inside their frame (printed, they are shown in full); images shrink to fit it. 40 % of the default 720px slide height. Use slide pixels, not `vh`: reveal scales the slide to the window, so a `vh` length grows with the window and can overflow the slide. |

`--p-cell-gap` and `--p-cell-border` are in the [token table](#tokens).

**Your own cell markup.** Put a `templates/cell.html` in a child theme. It receives `cell`
(`index`, `language`, `source`, `execution_count`, `outputs`, `hide_input`, `hide_output`,
`fragment`, `tags`, `metadata`; see `presenter dump --context`) and the whole template context
(`config.notebook.show_prompts`). Each output has a `kind`: `stream` (`name`, `html`), `error`
(`ename`, `evalue`, `html`), `display` (`mime`, `html`, `execution_count`) or `unsupported`
(`mimes`). Output `html` is already rendered: insert it with `|safe`, and leave `cell.source`
escaped. Keep the classes above and the default stylesheet styles your markup without any CSS
of your own. To wrap the default markup instead of replacing it:

```jinja
<div class="my-cell">{% include "@default/cell.html" %}</div>
```

## Print and PDF

`?print-pdf` (and `presenter export pdf`) prints one slide per page, or one page per
fragment step with reveal's default `pdfSeparateFragments: true`. The theme keeps backgrounds,
code blocks and fills (`print-color-adjust: exact`), keeps the slide padding that reveal's
print view would reset, and keeps slides one slide high so top-aligned slides stay top-aligned
and title and section slides stay centred on the page.

Paper cannot scroll, so the boxes that scroll on screen print in full: a code block taller
than `--p-code-max-height` and a notebook cell's outputs taller than `--p-output-max-height`
grow to their whole height, and the slide continues on extra pages (as many as it needs, up
to reveal's `pdfMaxPagesPerSlide`). In print, and only there, their lines are set 4–5 %
closer: code at `0.95lh`, 95 % of the line height its `pre` has on screen (1.33 instead of
1.4), text outputs at 1.3 instead of 1.35 (a theme that changes the outputs' line height
should lower its print value as well), and DataFrame rows get a little less padding. Chrome moves a line or row that straddles a page break to the
next page; the space saved leaves room for that, so the last lines still fit on the pages
reveal reserved. (It can also leave the last of those pages empty but for the slide's
background.) A DataFrame's header rows print once, at the top of the table.

What does not print in full: lines longer than the box and tables wider than it are cut off
at its right edge (on screen they scroll sideways); a `.r-stretch` code block keeps the size
it has on screen; and an image that straddles a page break moves to the next page whole, so
a slide that spans pages with several images in it can lose its last lines; split such a
slide.

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
