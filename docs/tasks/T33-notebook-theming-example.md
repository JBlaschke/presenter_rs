# T33 — Notebook theming and executed example notebook

| | |
|---|---|
| Milestone | M3 |
| Size | M |
| Depends on | T32, T17 (and T31 for a real build) |
| Parallel-safe with | T34, T42, T43 |
| Files you own | `assets/themes/default/templates/cell.html`, `assets/themes/default/styles/notebook.css` (+ entry in `theme.toml`), `examples/notebook/**`, `docs/screenshots/notebook-*.png` |

## Goal
Notebook slides that look designed, not exported: input/output framing, optional prompts,
scrollable long outputs, readable DataFrames, colored streams and errors — all overridable
by a child theme through one template and a handful of tokens.

## Read first
- `docs/spec/theme-format.md` §6 (class contract), §7 (`cell.html`); `docs/spec/notebook-mapping.md` §4

## Steps
1. `cell.html`: `<div class="cell{% if cell.fragment %} fragment{% endif %}{% if cell.hide_input %} hide-input{% endif %}{% if cell.hide_output %} hide-output{% endif %}" data-cell-index="{{ cell.index }}">`,
   prompt (`In [n]:` when `config.notebook.show_prompts`), input `<pre><code class="language-{{ cell.language }}" data-trim>{{ cell.source }}</code></pre>`,
   outputs loop: `stream` → `<pre class="output output-stream {{ out.name }}">{{ out.html|safe }}</pre>`;
   `error` → `<pre class="output output-error">…`; `display` → `<div class="output output-display" data-mime="{{ out.mime }}">{{ out.html|safe }}</div>` with `Out [n]:` prompt when `execution_count`;
   `unsupported` → `<div class="output output-unsupported">No renderable output ({{ out.mimes|join(", ") }})</div>`.
2. `notebook.css`: tokens `--p-cell-gap`, `--p-output-max-height` (default `40vh`, overflow auto),
   `--p-prompt-color`; DataFrame tables (`.output-display table`) compact with sticky header;
   images max-width 100 %; `stderr` tinted; errors red-tinted; hidden parts `display:none`.
3. `examples/notebook/analysis.ipynb`: a small, real notebook (numpy + matplotlib + pandas;
   ~8 cells) with `slideshow` metadata set on every cell, one `hide-input` tag, a markdown
   cell with front matter (`title`, `theme: default`), a deliberate `ZeroDivisionError` cell
   marked `fragment`, a `$$` LaTeX markdown cell. Execute it with
   `jupyter nbconvert --to notebook --execute --inplace analysis.ipynb` and commit outputs.
   If matplotlib/pandas are unavailable, install them in a venv (document the exact commands in
   `examples/notebook/README.md`); never hand-edit outputs.
4. Screenshots of a code+plot slide and a DataFrame slide into `docs/screenshots/`.

## Acceptance criteria
- `presenter build examples/notebook/analysis.ipynb` succeeds; output has `class="cell` for each code cell, the error output is visible and styled, the plot renders as an inline PNG, the DataFrame table fits the slide at 1280×720 (screenshot).
- A child theme overriding only `cell.html` (write one under `tests/fixtures/themes/child-theme/`) renders without touching CSS.
- Tokens documented in the theme README (extend T17's test).
- `--p-output-max-height` works (an output with 200 lines scrolls instead of overflowing; manual check).

## Out of scope
Execution inside `presenter`, widgets, plotly.
