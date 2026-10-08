# presenter — implementation plan

`presenter` is a single Rust binary that turns Markdown files and Jupyter notebooks into
reveal.js presentations. It renders the finished product to a self-contained HTML file,
and during authoring it watches the sources *and the theme* and pushes changes to the
browser instantly. Its reason to exist, compared with `jupyter nbconvert --to slides`, is
that the look and feel (templates, CSS, layouts, how notebook cells are framed) is meant
to be edited continuously with a tight feedback loop, not configured once.

This document is the master plan. It is written so that a fleet of independent,
relatively simple agents can each pick up one task file under `docs/tasks/` and complete
it without needing the whole picture. The contracts those tasks depend on live in
`docs/spec/` and are frozen before the fan-out begins.

Verified facts used in this plan are dated 2026-10-08 (versions, APIs). See §9.

---

## 1. Goals and non-goals

### Goals
1. **Inputs:** CommonMark/GFM Markdown (`.md`) and Jupyter notebooks (`.ipynb`, nbformat 4).
2. **Output:** one self-contained `.html` file (reveal.js, theme CSS, fonts, images, KaTeX
   all inlined) by default; optionally a directory with separate assets.
3. **Engine:** reveal.js (pinned 6.0.2, vendored, MIT). All reveal features reachable:
   vertical slides, fragments, speaker notes, backgrounds, transitions, auto-animate,
   code line highlighting, math, PDF export.
4. **Theming as a first-class workflow:** a theme is a directory of ordinary files
   (Jinja templates, CSS/SCSS, assets) with inheritance (`extends = "default"`). Editing
   any of them while `presenter serve` runs updates the open browser without losing the
   current slide.
5. **Notebook rendering under the author's control:** cell structure (inputs, outputs,
   prompts, tags like `hide-input`) is exposed to templates and CSS, and the mapping
   from cells to slides is compatible with nbconvert's `slideshow.slide_type` metadata.
6. **Fast iteration:** `presenter serve deck.md` rebuilds in well under a second for a
   typical deck and reports parse errors in the browser as an overlay.
7. **Deterministic, testable builds:** same inputs → byte-identical output; snapshot tests.

### Decisions already taken
- License: MIT (`LICENSE` in the repo root, copyright Johannes Blaschke). Vendored reveal.js and
  KaTeX are MIT as well; their license files ship next to the assets.
- Crate structure: a single crate with a library and a binary (confirmed by the repo owner,
  2026-10-08). Do not split into a workspace; module ownership per task is the unit of parallelism.

### Non-goals (for this plan)
- Executing notebooks. Authors run their notebooks in Jupyter; `presenter` renders saved
  outputs. (A thin `--execute` shell-out to `jupyter nbconvert` is listed as a stretch.)
- Interactive widgets (`ipywidgets`). They render as their `text/plain` fallback.
- A GUI editor, a WYSIWYG mode, or a hosted service.
- Supporting every pandoc extension. The dialect is small and documented.

---

## 2. User experience (what we are building toward)

```bash
presenter init talk            # scaffolds talk/presenter.toml, talk/slides.md, talk/theme/
cd talk
presenter serve slides.md      # opens http://127.0.0.1:3000, live reload on .md/.ipynb/theme changes
presenter build slides.md      # writes dist/slides.html (single file)
presenter build analysis.ipynb -o out/analysis.html --theme ./theme
presenter theme eject default ./theme   # copy the built-in theme to hack on it
presenter dump slides.md --context      # print the JSON the templates receive (template debugging)
presenter check slides.md               # lint: missing images, unknown layouts, bad separators
presenter export pdf slides.md          # (stretch) headless Chrome → PDF
```

Authoring syntax in one screen (full spec: `docs/spec/markdown-dialect.md`):

```markdown
---
title: Rust for data people
author: J. Blaschke
theme: ./theme
reveal:
  transition: fade
---

# Rust for data people {layout=title}

---

## Agenda

- Why Rust
- Tooling
- Live demo

--

## A vertical sub-slide

:::fragment
Appears on click.
:::

--- {.dark data-background-color=#111}

## Code

```python {lines="1-2|4"}
import polars as pl
df = pl.read_csv("data.csv")

print(df.describe())
```

:::notes
Mention compile times.
:::

---

## Two columns

::::columns
:::column
Left: $E = mc^2$
:::
:::column
![](figures/plot.png)
:::
::::
```

---

## 3. Architecture

```
            ┌────────────────────────────────────────────────────────────────────┐
 inputs     │  frontends                                                          │
 .md ──────►│  markdown::{frontmatter, split, render}  ─┐                         │
 .ipynb ───►│  notebook::{nbformat, convert, outputs}  ─┼──► Deck IR (+ Assets)   │
            └───────────────────────────────────────────┼─────────────────────────┘
                                                        ▼
            ┌───────────────────────────────────────────────────────────────────┐
 theme dir ►│  theme::{manifest, resolve chain, templates (minijinja), styles}   │
 config ───►│  render::{context, html}  ──► HTML document with asset: placeholders│
            │  assets::emit  ──► standalone single file  |  site directory       │
            └───────────────────────────────────────────────────────────────────┘
                                                        ▼
            ┌───────────────────────────────────────────────────────────────────┐
 serve      │  serve::watch (notify) → pipeline rebuild → in-memory build        │
            │  serve::server (axum): GET /  GET /__presenter/theme.css           │
            │                         GET /__presenter/events (SSE) → browser     │
            └───────────────────────────────────────────────────────────────────┘
```

### 3.1 Pipeline stages
1. **Parse** — a frontend turns one source file into a `Deck` (intermediate representation,
   `docs/spec/ir.md`) plus an `AssetRegistry` (images and binary outputs, referenced from
   HTML with `asset:<id>` placeholder URLs).
2. **Resolve theme and config** — `presenter.toml`, front matter, CLI flags and the theme's
   `theme.toml` are merged with a fixed precedence (`docs/spec/config.md`). The theme chain
   (`child → parent → … → built-in default`) yields a minijinja environment whose loader
   searches the chain.
3. **Render** — the `Deck` becomes a JSON-like template context (`docs/spec/theme-format.md`
   §4) and `deck.html` is rendered. Templates call helpers `style()`, `script()`, `asset()`
   and `theme_style()` which emit either inline content or URLs depending on output mode.
4. **Emit** — placeholders are replaced: standalone mode inlines everything as `data:`
   URIs and `<style>/<script>` blocks; site mode copies files into `out/assets/` with
   content-hashed names.
5. **Serve (dev loop)** — a watcher triggers stages 1–4 into memory, classifies the
   change (`css` vs `content`), and notifies browsers over Server-Sent Events. The client
   script swaps the stylesheet in place, or re-fetches the document and swaps the slides
   DOM, then calls `Reveal.sync()` and restores the slide indices.

### 3.2 Source layout (single crate, library + binary)

```
presenter_rs/
├── Cargo.toml                 package "presenter": lib + bin
├── PLAN.md                    this file
├── docs/spec/*.md             frozen contracts (IR, dialect, theme, config, CLI, live reload, vendoring)
├── docs/tasks/*.md            one file per task, pick-up-able independently
├── docs/guide/*.md            user documentation (T46); docs/dev/ holds QA checklists and release notes
├── assets/                    embedded into the binary with rust-embed
│   ├── vendor/reveal/…        reveal.js 6.0.2 subset (dist/, css/theme/**)  (see docs/spec/vendored-assets.md)
│   ├── vendor/katex/…         KaTeX 0.19.0 (js, css, woff2 fonts)
│   ├── presenter/             our own small JS: math.js (KaTeX init), live-reload.js
│   └── themes/default/        the built-in theme (theme.toml, templates/, styles/, assets/)
├── scripts/vendor.sh          re-downloads and trims vendored packages; writes assets/vendor/VERSIONS.toml
├── examples/                  basic/, notebook/, custom-theme/, headings-mode/
├── src/
│   ├── main.rs                clap entry; delegates to cli::*
│   ├── lib.rs                 pub mod declarations, re-exports
│   ├── error.rs               thiserror Error enum + Result alias
│   ├── ir.rs                  Deck/Slide/Block/… (contract)
│   ├── config.rs              Config structs, loading, merging
│   ├── pipeline.rs            parse → resolve → render → emit orchestration (Build struct)
│   ├── vendor.rs              rust-embed `Assets` + lookup helpers
│   ├── markdown/{mod,frontmatter,split,attrs,render,code}.rs   (+ compat.rs, postprocess.rs in T43)
│   ├── notebook/{mod,nbformat,convert,outputs}.rs
│   ├── theme/{mod,manifest,resolve,templates,styles}.rs
│   ├── assets/{mod,registry,emit}.rs
│   ├── render/{mod,context,html,reveal}.rs
│   ├── serve/{mod,server,watch,events}.rs
│   ├── check.rs               lint rules
│   ├── pdf.rs                 headless-Chrome PDF export (T45)
│   └── cli/{mod,build,serve,init,theme,dump,check,export}.rs
└── tests/                     integration tests + fixtures/{md,ipynb,ir,themes}
```

### 3.3 Technology decisions (and why)

| Decision | Choice | Why (alternatives considered) |
|---|---|---|
| Where Markdown is rendered | At build time, in Rust | Control over structure, validation, notebooks and themes. (reveal's runtime markdown plugin gives none of that.) |
| Markdown parser | `comrak` 0.56, `default-features = false, features = ["attributes"]` | AST with source positions; built-in `:::name` block directives, `{#id .class k=v}` attributes on headings/code/links/images, front matter extraction, `$…$` math nodes. (`pulldown-cmark` has no AST and none of these.) |
| Templates | `minijinja` 2.24 (`loader`, `json`, `loop_controls`) | Jinja2 syntax familiar to nbconvert users; `Environment::set_loader` makes theme-chain lookup a 20-line closure; good error messages. (`tera` has no custom loader.) |
| Code highlighting | reveal's bundled highlight.js plugin at runtime | Parity with reveal's `data-line-numbers="1-3|5"` step-through; no server-side language tables. (`syntect` later as an opt-in if wanted.) |
| Math | KaTeX 0.19 vendored + our own 15-line init that renders comrak's `data-math-style` spans | reveal's math plugin loads KaTeX from a CDN and scans text delimiters; we need offline output and already have structured math nodes. |
| Styles | plain CSS always; SCSS via `grass` 0.13 (pure Rust) | Verified: `grass` compiles reveal 6's own theme SCSS (`@use 'template/settings' with (…)`) unchanged, so authors can derive themes from reveal's template exactly like the stock themes. |
| Live reload transport | Server-Sent Events (axum 0.8) | One-directional, trivial client, auto-reconnect for free. WebSocket adds nothing here. |
| Output default | Single self-contained HTML | The user's stated deliverable. Site mode exists for very large decks. |
| Front matter | YAML via `serde_norway` 0.9 (maintained `serde_yaml` fork) | Jupyter/pandoc users expect YAML `---` blocks. `serde_yaml` is deprecated. |
| HTML post-processing | `lol_html` 3 (CSS-selector rewriter) | Needed only for incremental lists and reveal-compat comments (M4); robust vs. regex. |
| Embedding | `rust-embed` 8 | Debug builds read from disk (fast theme/JS iteration); release builds embed. |
| Crate structure | One crate (lib + bin) | Fewer moving parts for many contributors; module ownership is per task. |
| Notebook execution | Not in scope | Keeps the tool deterministic and Python-free. |

Versions above were checked against crates.io / npm on 2026-10-08 (§9).

---

## 4. Contracts (frozen before fan-out)

These documents are the interfaces between tasks. A task may *implement* them, and may
propose changes through the coordinator, but may not silently diverge from them.

| Contract | File | Consumers |
|---|---|---|
| Intermediate representation (`Deck`, `Slide`, `Block`, `NotebookCell`, …) and asset placeholders | `docs/spec/ir.md` | every parser, renderer, theme |
| Markdown dialect (separators, attributes, directives, code fences, math, notes) | `docs/spec/markdown-dialect.md` | T11, T12, T43, docs, fixtures |
| Notebook → slides mapping and output rendering rules | `docs/spec/notebook-mapping.md` | T30–T34 |
| Theme directory format, inheritance, template context, helper functions | `docs/spec/theme-format.md` | T02, T13, T15, T17, T41, T42 |
| Configuration schema and precedence | `docs/spec/config.md` | T01, T10, T15, T16, T40 |
| CLI surface | `docs/spec/cli.md` | T16, T23, T34, T40, T41, T45 |
| Live reload protocol | `docs/spec/live-reload.md` | T20–T23 |
| Vendored assets: exactly what is copied, sizes, licenses | `docs/spec/vendored-assets.md` | T00, T15, T42 |

---

## 5. Milestones and task graph

Each task is a file `docs/tasks/Txx-*.md` with goal, dependencies, owned files, steps,
acceptance criteria and verified API hints. Sizes: S ≈ half a day, M ≈ a day, L ≈ 2–3 days
for a careful agent, including tests.

### M0 — Contracts and scaffold (sequential; nothing fans out before T01 is merged)
| ID | Task | Size | Depends on |
|---|---|---|---|
| T00 | Repository scaffold, CI, vendoring script, embedded assets | M | — |
| T01 | Core IR, config and error types, fixture corpus | M | T00 |
| T02 | Default theme skeleton rendering a hand-written IR fixture | M | T01 |

### M1 — Markdown → self-contained HTML (`presenter build`)
| ID | Task | Size | Depends on |
|---|---|---|---|
| T10 | Front matter → `DeckMeta`; YAML parsing; config merge entry points | S | T01 |
| T11 | Slide splitter (AST-based `---` / `--` / attributes / heading lifting) | M | T01 |
| T12 | Markdown block renderer (directives, notes, code fences, math, images → assets) | L | T01, T11 (interface only) |
| T13 | Theme discovery, manifest, inheritance chain, minijinja environment | M | T01 |
| T14 | Asset registry and emitter (standalone inlining, site mode, CSS `url()` rewriting) | M | T01 |
| T15 | HTML renderer: context builder, template helpers, reveal config merge | M | T13, T14 |
| T16 | Pipeline, `build` + `dump` CLI, example deck, end-to-end snapshot tests | M | T10–T15 |
| T17 | Default theme v1 (layouts, code, notes, print styles, documented CSS variables) | L | T02, T16 |

### M2 — Live development loop (`presenter serve`)
| ID | Task | Size | Depends on |
|---|---|---|---|
| T20 | File watcher, debounce, rebuild loop, change classification | M | T16 |
| T21 | Dev server (axum): document, theme CSS, assets, SSE endpoint | M | T16 |
| T22 | Live-reload client: css swap, DOM swap + `Reveal.sync()`, error overlay | M | T21 (protocol only) |
| T23 | `serve` CLI wiring, port handling, `--open`, integration tests | S | T20, T21, T22 |
| T24 | Headless-browser smoke test (opt-in feature) | S | T16 |

### M3 — Jupyter notebooks
| ID | Task | Size | Depends on |
|---|---|---|---|
| T30 | nbformat 4 data model, lenient deserialization, fixtures | M | T01 |
| T31 | Cells → slides (slide_type, tags, raw cells, notes, first-cell front matter) | M | T30, T12 |
| T32 | Output rendering (MIME priority, images, ANSI, errors, LaTeX, Markdown) | M | T30, T14 |
| T33 | Notebook theming in the default theme + executed example notebook | M | T32, T17 |
| T34 | `presenter check` (lints for Markdown and notebooks) | M | T16, T31 |

### M4 — Theme workflow and polish
| ID | Task | Size | Depends on |
|---|---|---|---|
| T40 | `presenter init` scaffold | S | T16 |
| T41 | `presenter theme list / eject / new` | S | T13 |
| T42 | SCSS via `grass`, reveal theme template imports, font/asset inlining from CSS | M | T14, T17 |
| T43 | Heading-split mode, incremental lists, reveal `<!-- .slide: -->` / `.element:` compat | M | T11, T12 |
| T44 | Diagnostics quality (spans, pretty terminal errors, overlay payload) | S | T16, T22 |
| T45 | PDF export via headless Chrome | S | T16 |
| T46 | User documentation | M | all of M1–M3 |
| T47 | Release builds (macOS/Linux binaries, version embedding) | S | T16 |

### Dependency graph (what can run in parallel)

```
T00 ─► T01 ─┬─► T02 ───────────────┐
            ├─► T10 ─┐             │
            ├─► T11 ─┼─► T12 ─┐    │
            ├─► T13 ─┬─► T15 ─┼─► T16 ─┬─► T17 ─┬─► T33
            ├─► T14 ─┘        │        ├─► T20 ─┤
            └─► T30 ─┬─► T31 ─┘        ├─► T21 ─┼─► T23
                     └─► T32 ─────────►┤  T22 ──┘
                                       ├─► T24, T34, T40, T41, T42, T43, T44, T45, T47
                                       └─► T46 (last)
```

After T01 merges, up to six agents can work concurrently (T02, T10, T11, T13, T14, T30),
then T12 and T15, then T16 integrates. After T16, M2/M3/M4 tasks are almost all independent.

---

## 6. How an agent works a task

1. Read `PLAN.md` §3–§4, your task file, and the spec sections it lists under *Read first*.
2. Create a branch `task/Txx-short-name` from `main`. Rebase before opening the PR.
3. Only edit the files listed under *Files you own*. If you must touch another file, make
   the change additive (new function, new test) and call it out in the PR description.
   Never change `src/ir.rs` or `docs/spec/*` without a "contract change" note; the
   coordinator reviews those first.
4. Write tests as you go: unit tests next to code, snapshot tests (`insta`) for HTML,
   fixtures under `tests/fixtures/`. Every acceptance criterion in the task file must map
   to at least one test or a documented manual check.
5. Before the PR: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
   `cargo test`, and `cargo run -- build examples/basic/slides.md` (once T16 exists).
6. Update the status table in `docs/tasks/README.md` (one line) in the same PR.
7. Report honestly: if an acceptance criterion is not met, say which one and why.

### Coding conventions
- Edition 2024, stable toolchain. `#![deny(unsafe_code)]`, `#![warn(missing_docs)]` in `lib.rs`.
- Library code returns `crate::error::Result<T>`; no `unwrap()`/`expect()` outside tests
  unless infallibility is obvious and commented. The binary uses `anyhow` for top-level
  reporting.
- Deterministic output: `BTreeMap` for anything rendered; no timestamps unless requested.
- Paths: all paths relative to a source file are resolved against that file's directory,
  never the current working directory.
- Logging through `tracing`; `-v/-vv` raise the level. No `println!` in the library.
- Public items carry doc comments. Functions stay under ~80 lines; split otherwise.
- Tests must not need network access. Browser tests are behind the `browser-tests` feature.

---

## 7. Testing strategy

| Level | Tool | What |
|---|---|---|
| Unit | `#[test]` | splitter, attribute parser, MIME selection, config merge, path resolution |
| Golden IR | `insta` JSON snapshots | each `tests/fixtures/md/*.md` and `ipynb/*.ipynb` → `Deck` JSON |
| Golden HTML | `insta` snapshots of `<div class="slides">…</div>` only | theme output stability (build info stripped) |
| CLI | `assert_cmd` + `tempfile` | `build`, `dump`, `check`, `init`, `theme` exit codes and files written |
| Serve | integration test with `ureq` | start server on port 0, GET `/`, edit file, read one SSE event |
| Browser (opt-in) | `headless_chrome` | open built HTML, assert `Reveal.isReady()`, slide count, no console errors |

Fixture corpus (created in T01, extended by later tasks). No `presenter.toml` may live in an
ancestor directory of `tests/fixtures/{md,ipynb}` (config discovery would pick it up); the
config test fixture is `tests/fixtures/config/presenter.toml`.
`tests/fixtures/md/{minimal,separators,attributes,directives,code,math,images,notes,headings-mode,reveal-compat}.md`,
`tests/fixtures/ipynb/{minimal,slideshow,outputs,tags,frontmatter}.ipynb`,
`tests/fixtures/ir/sample-deck.json`, `tests/fixtures/themes/child-theme/`.

---

## 8. Risks and mitigations

| Risk | Mitigation |
|---|---|
| `--` vertical separator is ambiguous with setext headings (`Text\n--` = `<h2>`) | Splitter requires a blank line before `--`; `check` warns when a raw `--` line did not become a separator. Heading-split mode (T43) avoids separators entirely. |
| Standalone file size (fonts, KaTeX, highlight.js ≈ 1.5–2 MB before images) | Vendored subset only (woff2 fonts, no `.map`), KaTeX included only when `features.math`, site mode for huge decks. |
| Hot DOM swap desyncs reveal state (fragments, auto-animate) | Fall back to full reload with hash preserved whenever `Reveal.sync()` throws or slide count differs; css-only changes never reload. |
| Template errors are hard to read | minijinja `set_debug(true)` in serve mode; errors forwarded to the browser overlay with template name and line. |
| Notebook `text/html` outputs that need JavaScript (plotly, bokeh) | Rendered as-is; documented limitation; `check` warns. Plotly JSON support is a listed stretch. |
| reveal.js 6 API drift | Pinned 6.0.2 via `scripts/vendor.sh`; upgrade is a deliberate task with the browser smoke test as a gate. |
| Fleet agents editing the same files | Explicit *Files you own* per task; IR/spec changes gated by coordinator. |

---

## 9. Verified facts (2026-10-08)

- Local toolchain: rustc/cargo 1.99.0 (Homebrew), Node 26.10, Python 3.14 with ipykernel 7.4, Google Chrome installed.
- crates.io max stable versions: comrak 0.56.0 · minijinja 2.24.0 · notify 8.2.0 · notify-debouncer-full 0.7.0 · axum 0.8.9 · tokio 1.53 · clap 4.6 · grass 0.13.4 · rust-embed 8.13 · lol_html 3.0.1 · ansi-to-html 0.2.4 · base64 0.23 · insta 1.49 · assert_cmd 2.2 · serde_norway 0.9.42 · toml 1.1.
- comrak 0.56: `Options.extension.{block_directive, header_attributes, fenced_code_attributes, link_attributes, inline_code_attributes, front_matter_delimiter, math_dollars, math_code, table, strikethrough, tasklist, footnotes, alerts, description_lists}`; `Options.render.r#unsafe` enables raw HTML; `:::foo bar\n…\n:::` → `<div class="foo bar">…</div>`; `$x$` → `<span data-math-style="inline">x</span>`, `$$x$$` → `data-math-style="display"`; `NodeValue::BlockDirective(NodeBlockDirective { info, .. })`, `NodeValue::FrontMatter(String)`, `node.data().attrs: Option<Attributes { id, classes, pairs }>`; `SyntaxHighlighterAdapter { write_highlighted, write_pre_tag, write_code_tag }`.
- minijinja 2.24: `Environment::set_loader(|name| -> Result<Option<String>>)`, `path_loader`, `add_template_owned`, `render_str`, `set_debug`, `add_function`, `add_filter`, `add_global`, `set_undefined_behavior`; features `loader`, `json`, `loop_controls`, `custom_syntax`.
- reveal.js 6.0.2 (npm, MIT): plugins moved to `dist/plugin/<name>.js`; `dist/plugin/highlight.js` bundles highlight.js (920 KB); `dist/theme/*.css` have fonts inlined (≈170–580 KB each); theme SCSS sources at `css/theme/*.scss` + `css/theme/template/{settings,theme,mixins}.scss`, fonts at `css/theme/fonts/`; the math plugin loads KaTeX/MathJax from jsDelivr by default; `Reveal.sync()` now updates slide classes and fires a `sync` event; API has `getIndices`, `slide`, `layout`, `getState/setState`.
- `grass` 0.13.4 compiles reveal 6's `css/theme/white.scss` to 7 KB of CSS with no errors; `@import url(./fonts/…css)` passes through and must be resolved by our asset step.
- KaTeX 0.19.0 (MIT): `katex.min.js` 273 KB, `katex.min.css` 25 KB, woff2 fonts 296 KB total (60 font files in all formats; we ship woff2 only).
