# Configuration

presenter works without any configuration. Settings come from the command line, the deck,
`presenter.toml` and the theme, on top of built-in defaults; this page lists every setting,
its default, and how the sources combine.

## How settings combine

From highest to lowest precedence:

1. **Command-line flags**: `--theme`, `--site`/`--standalone`, `--port`, `--host`, `--open`,
   `-o`.
2. **The deck's front matter** (for a notebook: the first Markdown cell's front matter,
   then the notebook's `metadata.presenter`): `theme:`, and the `reveal:`, `slides:`,
   `markdown:`, `math:` and `notebook:` maps.
3. **`presenter.toml`**, the project file.
4. **The theme's `theme.toml` `[reveal]` table**: reveal.js defaults that come with a
   theme ([Themes](themes.md#a-theme-directory)).
5. **Built-in defaults**, listed below.

Tables merge key by key: a front matter `reveal: {transition: fade}` changes the transition
and keeps every other `[reveal]` value. A list replaces the list below it.

## `presenter.toml`

presenter looks for `presenter.toml` in the deck's directory, then in each parent
directory up to the root, and uses the first one it finds; so one file can serve every deck
below it. `--config PATH` uses that file instead. Every key is optional.

```toml
# talk/presenter.toml
[deck]
theme = "theme"          # a directory next to this file

[reveal]
slideNumber = "c/t"
transition = "fade"

[slides]
incremental_lists = true
```

`presenter init` writes a `presenter.toml` that lists every setting, commented out, with its
default. A key presenter does not know is an error that names the file and line and
suggests the key you probably meant, and the command exits with code 2 (`[reveal]` is the
exception: it passes any key on to reveal.js):

```text
error: config: presenter.toml: TOML parse error at line 2, column 1
  |
2 | incremental_list = true
  | ^^^^^^^^^^^^^^^^
unknown field `incremental_list`, expected one of `split`, `slide_level`, `incremental_lists` (did you mean 'incremental_lists'?)
```

## Every setting

`presenter dump --default-config` prints a `presenter.toml` with every setting at its
built-in default:

<!-- generated: presenter dump --default-config -->
```toml
# presenter.toml with every setting at its built-in default

[deck]
theme = "default"
lang = "en"

[build]
out_dir = "dist"
standalone = true
minify = false

[slides]
split = "rules"
slide_level = 2
incremental_lists = false

[markdown]
smart_punctuation = false
sourcepos = false

[reveal]
hash = true
slideNumber = false
transition = "slide"
controls = true
progress = true
center = true
width = 1280
height = 720
plugins = ["highlight", "notes", "zoom", "search"]

[math]
enabled = "auto"

[math.katex_options]

[notebook]
mime_priority = ["text/html", "image/svg+xml", "image/png", "image/jpeg", "image/gif", "image/webp", "text/markdown", "text/latex", "application/json", "text/plain"]
hide_input_tags = ["hide-input", "remove-input"]
hide_output_tags = ["hide-output", "remove-output"]
remove_cell_tags = ["remove-cell", "hide-cell", "skip"]
split_markdown_cells = false
show_prompts = true

[serve]
host = "127.0.0.1"
port = 3000
open = false
```

### `[deck]`

| Key | Default | Meaning |
|---|---|---|
| `theme` | `"default"` | The theme: a built-in name or a theme directory, relative to this file ([Themes](themes.md#choose-a-theme)) |
| `lang` | `"en"` | `<html lang>` when the front matter sets no `lang` |
| `title` | unset | The page `<title>` of decks whose front matter sets no `title` |
| `author` | unset | The author for decks whose front matter sets none: on the title slide and in `<meta name="author">` |
| `date` | unset | The date on the title slide for decks whose front matter sets none, shown as written |

### `[build]`

| Key | Default | Meaning |
|---|---|---|
| `out_dir` | `"dist"` | Where `presenter build` writes when `-o` is not given: `<out_dir>/<deck name>.html`, or `<out_dir>/<deck name>/` with `--site`. Unset, it is `dist` next to each deck; set in `presenter.toml`, it is relative to that file, so all decks below it share one directory |
| `standalone` | `true` | `true`: one self-contained HTML file. `false`: a directory with `index.html` and `assets/` (like `--site`) |
| `minify` | `false` | Reserved; has no effect |

### `[slides]`

| Key | Default | Meaning |
|---|---|---|
| `split` | `"rules"` | `"rules"`: `---` and `--` lines start slides. `"headings"`: headings start slides ([headings mode](authoring.md#headings-mode)) |
| `slide_level` | `2` | Headings mode: the heading level of an ordinary slide; lower levels start sections, deeper levels are content |
| `incremental_lists` | `false` | `true`: the items of each slide's top-level lists appear one click at a time |

### `[markdown]`

| Key | Default | Meaning |
|---|---|---|
| `smart_punctuation` | `false` | `true`: "straight quotes" become typographic quotes, `--` and `---` inside text en and em dashes |
| `sourcepos` | `false` | `true`: HTML elements carry their source position (`data-sourcepos`), for scripts and debugging |

### `[reveal]`

Any [reveal.js option](https://revealjs.com/config/), with reveal's own names
(`slideNumber`, not `slide_number`), goes to `Reveal.initialize`. presenter sets these:

| Key | Default | Meaning |
|---|---|---|
| `hash` | `true` | Keep the slide in the URL (`#/3/1`), so a reload stays on it |
| `slideNumber` | `false` | `true`, or a format such as `"c/t"` (current/total) or `"h.v"` |
| `transition` | `"slide"` | `none`, `fade`, `slide`, `convex`, `concave` or `zoom`; a slide can set its own |
| `controls` | `true` | The navigation arrows |
| `progress` | `true` | The progress bar |
| `center` | `true` | Centre slides vertically. The built-in theme's slides start at the top anyway, except the `title` and `section` layouts and slides with `.center` |
| `width` | `1280` | Slide width in pixels; reveal scales slides to fit the window |
| `height` | `720` | Slide height in pixels |
| `plugins` | `["highlight", "notes", "zoom", "search"]` | Plugins to load. `highlight` is added only to decks with code, and `notes` only to decks with speaker notes unless you list it yourself. Math needs no plugin |

Other options worth knowing: `pdfSeparateFragments = false` (one PDF page per slide, not per
fragment step), `showNotes = true` (speaker notes on the slides, also in the PDF),
`pdfMaxPagesPerSlide`, `autoSlide`, `loop`, `navigationMode`.

### `[math]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `"auto"` | `"auto"`: include KaTeX when the deck has math. `"always"`, `"never"` |
| `katex_options` | `{}` | Options for every KaTeX render, e.g. `{ macros = { "\\R" = "\\mathbb{R}" } }` |

### `[notebook]`

| Key | Default | Meaning |
|---|---|---|
| `mime_priority` | `["text/html", "image/svg+xml", "image/png", "image/jpeg", "image/gif", "image/webp", "text/markdown", "text/latex", "application/json", "text/plain"]` | An output is shown as the first of these types it has ([Outputs](notebooks.md#outputs)) |
| `hide_input_tags` | `["hide-input", "remove-input"]` | Cell tags that hide the code |
| `hide_output_tags` | `["hide-output", "remove-output"]` | Cell tags that hide the outputs |
| `remove_cell_tags` | `["remove-cell", "hide-cell", "skip"]` | Cell tags that leave the cell out |
| `split_markdown_cells` | `false` | `true`: `---` and `--` lines in Markdown cells start slides |
| `show_prompts` | `true` | Show `In [n]:` and `Out [n]:` next to code cells |

### `[serve]`

| Key | Default | Meaning |
|---|---|---|
| `host` | `"127.0.0.1"` | The address `presenter serve` listens on (`--host`). `"0.0.0.0"` makes it reachable from other machines |
| `port` | `3000` | The port (`--port`); `0` picks a free one |
| `open` | `false` | Open the deck in the browser on start (`--open`) |

## Front matter

A deck's front matter overrides `presenter.toml` for that deck only:

```yaml
---
theme: ./dark-theme
reveal:
  transition: fade
  slideNumber: c/t
slides:
  incremental_lists: true
math:
  katex_options:
    macros:
      "\\R": "\\mathbb{R}"
notebook:
  show_prompts: false
---
```

`reveal:`, `slides:`, `markdown:`, `math:` and `notebook:` take the keys of the tables above.
`[deck]` settings are plain front matter keys (`title:`, `author:`, `lang:`, …), and
`[build]` and `[serve]` cannot be set per deck. Other keys are free values for templates
([front matter](authoring.md#front-matter)).

## Command-line flags

| Flag | Overrides |
|---|---|
| `--theme NAME\|PATH` | `theme` (front matter and `[deck]`) |
| `--standalone`, `--site` | `[build] standalone` |
| `-o PATH` | where the output goes, instead of `[build] out_dir` |
| `--port N`, `--host H`, `--open` | `[serve]` |
| `--config PATH` | which `presenter.toml` is read |

The [command line](cli.md) page lists every flag.
