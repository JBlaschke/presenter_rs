# presenter

`presenter` is a single Rust binary that turns Markdown files and Jupyter notebooks into
reveal.js presentations, rendered as one self-contained HTML file, with a live-reloading
development server that watches the sources and the theme so the look and feel can be
edited continuously. Status: **pre-alpha, see [PLAN.md](PLAN.md)** for the design and the
task breakdown. Licensed under MIT (see `LICENSE`); the vendored reveal.js and KaTeX
(`assets/vendor/`) are MIT-licensed as well and ship with their own license files.
