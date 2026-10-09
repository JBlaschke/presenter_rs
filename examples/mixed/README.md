# Mixed example

A Markdown deck that includes an executed notebook and a second Markdown file with
`:::include` ([Writing slides: including files](../../docs/guide/authoring.md#including-files)).
The notebook's slides come first, then two slides written in Markdown, then the slides of
`part2.md`.

| File | What it is |
|---|---|
| `slides.md` | The deck: front matter, `:::include ../notebook/analysis.ipynb`, a Markdown slide with a vertical slide, `:::include part2.md` |
| `part2.md` | Two more slides; its image is found next to it |
| `figures/window.svg` | The diagram on the first slide of `part2.md` |
| [`../notebook/analysis.ipynb`](../notebook/README.md) | The notebook, shared with the notebook example |

The front matter repeats the notebook's `title`, `subtitle`, `author` and `theme`: an
included file's deck metadata is ignored, and `presenter check` names every value that
differs from the deck's.

## Build and serve

From the repository root (or `cd examples/mixed` and drop the directory from the paths):

```sh
# One self-contained HTML file: examples/mixed/dist/slides.html
presenter build examples/mixed/slides.md

# Lint the deck and the files it includes: no warnings
presenter check examples/mixed/slides.md

# Live preview: saving slides.md, part2.md or the notebook reloads the browser
presenter serve examples/mixed/slides.md
```

With a checkout and no installed binary, use `cargo run --` in place of `presenter`.
`dist/` is not committed.
