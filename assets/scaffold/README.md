# {{name}}

A presentation made with presenter: `{{deck}}` becomes a reveal.js slide deck in a single
HTML file that works offline.

## Preview and build

```sh
presenter serve {{deck}}          # live preview at http://127.0.0.1:3000; every save updates it
presenter build {{deck}}          # one self-contained file: dist/slides.html
presenter build {{deck}} --site   # a directory instead: dist/slides/index.html + assets/
presenter export pdf {{deck}}     # a PDF (needs Google Chrome or Chromium)
presenter check {{deck}}          # look for mistakes without building
```

## Files

| File | What it is |
|---|---|
| `{{deck}}` | The deck |
| `presenter.toml` | Settings; every option is listed with its default value |
| `theme/` | The look: `theme.toml`, `styles/theme.css` and `templates/` |
| `.gitignore` | Keeps the build output (`dist/`) out of git |

## Change the look

- Colors, fonts and spacing: the custom properties at the top of `theme/styles/theme.css`.
- Markup: the templates in `theme/templates/` (minijinja, Jinja2 syntax), for example
  `layouts/title.html` for the title slide. A theme that `extends` another one only needs
  the files it changes; every other file comes from its parent.
- `presenter theme eject default ./default-theme` copies the built-in theme next to this
  one, so you can read (or start from) all of its templates and styles.
- `presenter dump {{deck}} --context` prints the data the templates receive.

A slide picks a layout with `{layout=NAME}` after its first heading, as the title slide
does. `title`, `section` and `blank` are built in; a file `theme/templates/layouts/NAME.html`
adds a layout or changes one.
