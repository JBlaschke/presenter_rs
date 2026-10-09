# Notebook example

A small, executed Jupyter notebook (numpy, pandas, matplotlib) turned into slides with the
built-in `default` theme. It shows how notebook cells map to slides
([`docs/spec/notebook-mapping.md`](../../docs/spec/notebook-mapping.md)) and how the theme
draws code cells and their outputs
([Notebook cells](../../assets/themes/default/README.md#notebook-cells)).

| File | What it is |
|---|---|
| `analysis.ipynb` | The notebook, with the outputs of one run committed. Open it in Jupyter to edit it. |

![A code cell with its plot](../../docs/screenshots/notebook-code-plot.png)

## Build

From the repository root (or `cd examples/notebook` and drop the directory from the paths):

```sh
# One self-contained HTML file: examples/notebook/dist/analysis.html
presenter build examples/notebook/analysis.ipynb

# Lint it: no warnings
presenter check examples/notebook/analysis.ipynb

# Live preview with reload on every save of the notebook
presenter serve examples/notebook/analysis.ipynb
```

With a checkout and no installed binary, use `cargo run --` in place of `presenter`.
Building needs no Python: presenter renders the outputs stored in the notebook. `dist/` is
not committed.

## What each slide shows

Every cell has `slideshow` metadata: its slide type (JupyterLab: the Property Inspector in
the right sidebar; `metadata.slideshow.slide_type` in the JSON).

| # | Slide | Cells (slide type, tags) | Feature |
|---|---|---|---|
| 1 | Smoothing a noisy signal | markdown (`slide`) | Front matter in the first markdown cell (`title`, `subtitle`, `author`, `theme: default`); the cell's `metadata.presenter` `{"layout": "title"}` picks the title layout |
| 2 | A noisy signal | markdown (`slide`), code (`-`) | A code cell continues the slide; stdout plain, stderr tinted |
| 3 | A rolling mean | markdown (`slide`), code (`-`), markdown (`notes`) | A matplotlib figure as an inline PNG; speaker notes (press <kbd>S</kbd>) |
| 4 | Summary statistics | markdown (`slide`), code (`-`) | A pandas DataFrame (an execute result, with its `Out [3]:` prompt) |
| 4.1 | Every sample | markdown (`subslide`), code (`-`) | A vertical slide; a 200-line output scrolls inside its frame |
| 5 | How the smoothing works | markdown (`slide`), code (`-`, tag `hide-input`) | `$…$` and `$$…$$` math (KaTeX); a hidden-input cell whose Markdown result reads as slide text |
| 6 | When a cell fails | markdown (`slide`), code (`fragment`, tag `raises-exception`) | The `ZeroDivisionError` traceback, styled, appearing on the next click |

To hide the `In [n]:` / `Out [n]:` prompts, add `notebook: {show_prompts: false}` to the
front matter, or `show_prompts = false` under `[notebook]` in a `presenter.toml`.

## Regenerating the outputs

The outputs come from running the notebook top to bottom; never edit them by hand. After
changing a cell, run it again with Python in a virtual environment outside the repository
(`$VENV` below, e.g. `~/.venvs/presenter-notebook`):

```sh
VENV=~/.venvs/presenter-notebook
python3 -m venv "$VENV"
"$VENV/bin/pip" install numpy pandas matplotlib ipykernel nbconvert nbformat

cd examples/notebook
"$VENV/bin/jupyter" nbconvert --to notebook --execute --inplace \
    --ExecutePreprocessor.kernel_name=python3 \
    --ExecutePreprocessor.record_timing=False \
    analysis.ipynb
```

- Run through the venv's own `jupyter`, the `python3` kernel is the venv's `ipykernel`
  (`"$VENV/bin/jupyter" kernelspec list` shows it), so no kernel needs registering.
- `record_timing=False` keeps per-cell execution timestamps out of the file. The random
  generator is seeded, so running the notebook again with the same package versions gives a
  byte-identical file.
- The failing cell carries the tag `raises-exception`, so execution goes on past it and
  records its traceback (no `--allow-errors` needed).
- The committed outputs were made with Python 3.14.8, numpy 2.5.3, pandas 3.0.6,
  matplotlib 3.11.2, ipykernel 7.4.0 and nbconvert 7.17.1. Other versions may change the
  plot's bytes or the traceback's colours, not the slides' structure.

Afterwards, `presenter check examples/notebook/analysis.ipynb` must still report nothing
(`tests/cli_check.rs` checks it), and `cargo test --test notebook_theme` checks the
notebook's shape.
