# Basic example

A deck that uses every feature of presenter's Markdown dialect
([`docs/spec/markdown-dialect.md`](../../docs/spec/markdown-dialect.md)) with the built-in
`default` theme.

| File | What it is |
|---|---|
| `slides.md` | The deck: front matter, then slides separated by `---` and `--` lines |
| `presenter.toml` | Project configuration; every key is commented ([`docs/spec/config.md`](../../docs/spec/config.md)) |
| `figures/plot.png` | The figure on the "Two columns" and "Background images" slides |
| `figures/embed.html` | The page in the `<iframe>` on the "Raw HTML" slide |
| `figures/make-plot.py` | How `plot.png` was drawn (Python standard library only; not needed to build) |

## Build and serve

From the repository root (or `cd examples/basic` and drop the directory from the paths):

```sh
# One self-contained HTML file: examples/basic/dist/slides.html
presenter build examples/basic/slides.md

# A directory instead: examples/basic/dist/slides/index.html + assets/
presenter build examples/basic/slides.md --site

# Choose the output path
presenter build examples/basic/slides.md -o /tmp/basic.html

# Live preview with reload on every save (deck, figures or theme)
presenter serve examples/basic/slides.md

# The deck as presenter parsed it, and the data the theme templates receive
presenter dump examples/basic/slides.md
presenter dump examples/basic/slides.md --context
```

With a checkout and no installed binary, use `cargo run --` in place of `presenter`.
`dist/` is not committed.

## What each slide shows

| # | Slide | Feature | Written as |
|---|---|---|---|
| 1 | Presenter basics | title layout, front matter | `# Presenter basics {layout=title}` |
| 2 | What this deck shows | speaker notes (press <kbd>S</kbd>) | `:::notes` … `:::` |
| 3 | A vertical stack (3 slides) | vertical slides, slide ids | `--`, `## One level down {#stack-child}` |
| 4 | Fragments | effects and explicit order | `:::fragment fade-up index=3` |
| 5 | Two columns | columns, a local image | `::::columns widths="2fr 3fr"` around `:::column` blocks, `![…](figures/plot.png)` |
| 6 | Code with line stepping | highlighted code, line steps | ```` ```python {lines="1\|3-4\|6-7"} ```` |
| 7 | Math | inline and display math (KaTeX) | `$A = \pi r^2$`, `$$…$$` |
| 8 | A table | GitHub tables | `\| a \| b \|` |
| 9 | A footnote | footnotes | `reveal.js[^reveal]` and `[^reveal]: …` |
| 10 | Backgrounds | background colour, per-slide transition | `--- {background="#0f172a" transition=zoom}` |
| 11 | Background images | background image attributes | `--- {background=figures/plot.png background-opacity=0.2 …}` |
| 12 | Raw HTML: an embedded page | raw HTML, a bundled local `<iframe>` | `<iframe src="figures/embed.html" …>` |
| 13 | Thank you | section layout | `# Thank you {layout=section}` |

Nested directives need a longer outer fence: `::::columns` … `::::` around `:::column` …
`:::` (slide 5).
