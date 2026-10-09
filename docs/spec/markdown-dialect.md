# Spec: Markdown dialect

**Status:** contract. Implemented by T11 (splitting), T12 (rendering), T43 (compat and
heading mode). Every example here is also a test fixture under `tests/fixtures/md/`.

The dialect is CommonMark + GitHub-flavored extensions as implemented by `comrak` 0.56,
plus a handful of presentation conventions. Design rule: anything that can be expressed
with comrak's existing extensions (directives, attributes, math, front matter) is, so the
parser stays small.

## 1. comrak configuration (exact)

```rust
let mut o = comrak::Options::default();
// extensions
o.extension.front_matter_delimiter = Some("---".into());
o.extension.table = true;
o.extension.strikethrough = true;
o.extension.tasklist = true;
o.extension.autolink = true;
o.extension.footnotes = true;
o.extension.description_lists = true;
o.extension.alerts = true;                 // > [!NOTE] callouts
o.extension.math_dollars = true;           // $x$ and $$x$$
o.extension.math_code = true;              // ```math fences
o.extension.block_directive = true;        // :::name … :::
o.extension.header_attributes = true;      // ## Title {#id .cls k=v}
o.extension.fenced_code_attributes = true; // ```python {lines="1-3"}
o.extension.link_attributes = true;        // ![](x.png){width=50%}
o.extension.inline_code_attributes = true;
// (no `shortcodes` line: the field only exists with comrak's `shortcodes` feature, which we do not enable)
// parse
o.parse.smart = config.markdown.smart_punctuation;   // default false (keeps `--` literal)
o.parse.relaxed_autolinks = false;
// render
o.render.r#unsafe = true;      // raw HTML passes through (needed for <video>, <iframe>, comments)
o.render.escape = false;
o.render.hardbreaks = false;
o.render.github_pre_lang = false;          // we emit <code class="language-x"> ourselves
o.render.full_info_string = false;
o.render.sourcepos = config.markdown.sourcepos;      // default false; serve mode may enable
```

Cargo: `comrak = { version = "0.56", default-features = false, features = ["attributes"] }`.

## 2. Document structure

```
[front matter]      optional, only at the very top: `---` YAML `---`
slide 1
---                 horizontal separator
slide 2
--                  vertical separator: slide 3 becomes a child of slide 2
slide 3
---
slide 4
```

### 2.1 Front matter
- Must start on line 1 with `---` and end with a line `---`. comrak extracts it as
  `NodeValue::FrontMatter(String)` (the raw block including delimiters).
- Parsed as YAML with `serde_norway`. Known keys map to `DeckMeta` fields (`title`,
  `subtitle`, `author`, `date`, `description`, `lang`, `theme`); `reveal:` must be a map
  and goes to `DeckMeta.reveal`; everything else goes to `DeckMeta.vars` verbatim.
- Non-map YAML is an error `FrontMatter("expected a mapping")`.
- An empty value for `reveal:`, `slides:`, `markdown:`, `math:` or `notebook:` (all options
  commented out) counts as an empty mapping; any other non-mapping value for these keys is a
  `FrontMatter` error. YAML is parsed as YAML 1.2 core schema (`serde_norway`): `yes/no/on/off`
  are strings, not booleans. Duplicate keys are errors. (T10, 2026-10-08)
- Values that are not strings for string fields (e.g. `date: 2026-10-08` parses as a
  date/plain scalar) are converted with their YAML scalar text; `title: 42` → `"42"`.

> **A3 note (2026-10-09):** `---` on line 1 directly followed by `---` on line 2 is empty front
> matter (as in Jekyll and pandoc), like `---`, blank line, `---`; such a deck is parsed without
> comrak's front matter extension, so a later `---` stays a slide separator. Markdown headings
> are YAML comments, so front matter can swallow slides silently; the frontend warns:
> `front-matter-empty` when the block has no key at all and a comment that reads as a Markdown
> heading (a deck that starts with a `---` rule; commented-out keys such as `# title: Draft`
> and labels ending with `:` do not count), and `front-matter-unclosed` when a Markdown heading
> follows a blank line after the last YAML line, with a blank line after it before the closing
> `---` (the closing `---` is probably missing; a remark right on the closing `---` is not
> reported). "expected a mapping" gets the root value's position, and
> it and other YAML errors in such a block name the block's extent and ask whether the closing
> `---` is missing. A `mapping values are not allowed` error on `key: value` with `: ` in the
> plain value ends with `(put the value in quotes: `title: "Rust: the good parts"`)`; a line
> indented with a tab with `(indent with spaces, not tabs)`.

### 2.2 Separators (rules mode, the default `slides.split = "rules"`)
Decided on the comrak AST of the whole document, walking the `Document`'s direct children:

| AST node (top level only) | Meaning |
|---|---|
| `ThematicBreak` (from `---`, `***`, `___`) | start a new **horizontal** slide |
| `Paragraph` whose sole child is `Text` equal to `--` | start a new **vertical** slide (child of the current horizontal slide) |
| `Paragraph` whose text matches `^(---|--)\s*\{(.*)\}$` | separator **with attributes** for the slide that follows (§3) |
| `HtmlBlock` whose literal is `<!-- .slide: … -->` as the first node of a slide | attributes for the current slide (reveal compat, §7 — T43) |

Rules:
- Content before the first separator is slide 1. An empty document yields one empty slide.
- Two consecutive separators yield an empty slide (do not collapse; `check` warns).
- A `--` directly after `---` makes the new horizontal slide have an empty first vertical
  slide; `check` warns.
- Separators inside fenced code, block quotes, lists or directives are **not** separators
  (they are not top-level nodes). This is the main reason splitting is AST-based.
- Setext pitfall: `Some text\n--` is parsed by CommonMark as an `<h2>`. The splitter cannot
  see a separator there. `check` (T34) compares raw source lines equal to `--`/`---` with
  separator positions and warns about each line that did not become a separator.
- Front matter does not count as a slide and is not a separator.
- Horizontal separators when a vertical stack is open close the stack.

### 2.3 Heading-split mode (`slides.split = "headings"`, T43)
No separator lines. `#` (level 1) headings start a horizontal slide; `##` headings start a
vertical slide under it; configurable through `slides.slide_level` (default 2):
headings with level < slide_level → horizontal, level == slide_level → vertical,
deeper levels → ordinary content. Separator lines are still honored if present (mixed
mode), so a deck can be migrated gradually. Content before the first heading becomes an
initial slide only if it is non-blank.

> **Contract change (coordinator, 2026-10-08, from T43):** headings mode follows pandoc. A
> slide-level heading (level == slide_level) is vertical only when a section heading
> (level < slide_level) opened the current stack; otherwise (a deck of only `##`, `slide_level = 1`,
> or `##` after a `---` rule) it starts a horizontal slide. A boundary heading joins a slide that
> has no visible content yet (e.g. right after `---` or a `<!-- .slide: -->` comment). `---` ends
> the section; `--` keeps it. `<!-- .slide: -->` is also read directly after a slide's first
> heading. A `data-background` value that reveal treats as an image moves to `data-background-image`
> (and is registered as an asset).

> **A3 note (2026-10-09):** "visible content" excludes HTML comments and `:::notes` blocks (also
> pandoc's `::: {.notes}`): speaker notes before a boundary heading go with that heading's slide
> instead of making a blank slide of their own.

## 3. Attributes

### 3.1 Syntax
Pandoc-style attribute blocks `{#id .class1 .class2 key=value key2="quoted value"}`.
Values may be single- or double-quoted; unquoted values run to the next whitespace.
comrak parses these natively for headings, fenced code, links and images into
`node.data().attrs: Option<Attributes { id, classes, pairs }>`. The separator form
(`--- {…}`) is parsed by our own `markdown::attrs::parse_attr_block(&str)` with the same
grammar (T11), and the two must agree (shared unit tests).

> **A3 note (2026-10-09):** comrak 0.56 rejects a whole block on headings, fenced code, links,
> images and inline code when a value is single-quoted or an unquoted value has characters other
> than letters, digits and `- _ . : %` (`background=#222`, `background=figures/bg.png`,
> `lines=1|3`), and leaves the `{…}` as text. The frontend then reads the block with
> `parse_attr_block`'s tokenizer, so these forms agree with `--- {…}`. Outside `--- {…}` and
> `<!-- .slide: -->` every item must be `#id`, `.class` or `key=value` with a valid key: a bare
> key needs a value (`auto-animate=""`), no item may end with an unquoted comma
> (`{debug=true, verbose=false}`), and text such as `{1, 2}` stays text. A block meant as
> attributes that is not valid (`{.dark auto-animate}`, an unbalanced quote, commas) stays text
> and is an `unparsed-attr-block` warning. On headings, links, images and inline code the block
> is rewritten with every value double-quoted, the form comrak reads itself, so the result is
> exactly that form's, also around it: `` `a`{k='1'}`b` `` keeps two code spans, a reference
> link keeps its target, and of two blocks at the end of a heading (`# H {.a} {k='v'}`) only the
> last is read and the first stays text, as for `# H {.a} {k="v"}`. A block that runs past the
> table cell or emphasis its element is in (a `|` in a value inside a table) stays text with an
> `unparsed-attr-block` warning. Not covered: notebook markdown cells with
> `split_markdown_cells = true`, whose headings, links, images and inline code comrak alone
> reads (their fences are covered).
>
> **Clarification (W8FIX, 2026-10-09):** no item may have a `{` or `}` outside quotes either.
> Two blocks written without a space (`## H {.dark}{background='#222'}`, also with double
> quotes, or a fence's `` ```python {.a}{lines='1|3'} ``) reach the frontend as one block, and a
> stray `}` (`{k='v'}}`) ends up in an item: such a block is not read (on a heading it stays in
> the text) and is an `unparsed-attr-block` warning, instead of becoming attributes with braces
> in their names and values. Quoted braces are content (`{data-x='a {b}'}`).

### 3.2 Slide attributes: where they come from and how they normalize
Sources, applied in this order (later wins for the same key; classes accumulate):
1. the separator line `--- {…}` / `-- {…}` that opened the slide,
2. a `<!-- .slide: … -->` HTML comment as the slide's first node (T43),
3. attributes on the slide's **first** node when it is a heading: `# Title {layout=title .x}`.
   They are *lifted*: removed from the heading node (so the `<h1>` is clean) and applied to
   the slide. The heading's `#id` becomes the section id.

> **A3 note (2026-10-09):** "first node" is the first *visible* node: HTML comments and
> `:::notes` blocks before the heading do not count. Slide attributes (`layout=`, `background=`
> and the other shorthands below; not `data-`/`aria-` keys) on a heading that is not lifted stay
> on the element and are an `unlifted-slide-attr` warning.

Normalization into `Slide` fields:
- `#id` → `Slide.id`; `.class` → `Slide.classes` (deduplicated, order kept);
- `layout=NAME` → `Slide.layout` (consumed, never emitted as an attribute);
- shorthand keys expand to reveal's data attributes (`Slide.attrs`):

  | shorthand | emitted key |
  |---|---|
  | `background` | `data-background-color` if value starts with `#` or matches `^(rgb|hsl)a?\(` or is a CSS color keyword; otherwise `data-background-image` |
  | `background-color`, `background-image`, `background-video`, `background-video-loop`, `background-video-muted`, `background-iframe`, `background-interactive`, `background-size`, `background-position`, `background-repeat`, `background-opacity`, `background-gradient`, `background-transition` | `data-` + key |
  | `transition`, `transition-speed`, `auto-animate`, `auto-animate-easing`, `auto-animate-duration`, `auto-animate-unmatched`, `auto-animate-id`, `auto-animate-restart`, `visibility`, `state`, `timing`, `autoslide`, `menu-title` | `data-` + key |
  | any key already starting with `data-` or `aria-` | verbatim |
  | anything else | verbatim, and `check` warns "unknown slide attribute" |

- Boolean shorthands written without a value (`{auto-animate}`) are stored with an empty
  string value and emitted as a bare attribute (`data-auto-animate`). comrak's attribute
  grammar requires `k=v`; for bare keys the separator parser accepts `key` alone and the
  heading form requires `auto-animate=""`. Document this in the user guide.

### 3.3 Element attributes
Headings, fenced code, links, images and inline code carry their comrak-parsed attributes
into the HTML (`id`, `class`, `k="v"`). T12 must verify that comrak's HTML formatter emits
`attrs` for these nodes; if it does not, T12 writes them via a small AST pass that converts
the node to `HtmlInline`/`HtmlBlock` before formatting. The reveal-compat
`<!-- .element: class="fragment" -->` comment form is T43.

> **A3 note (2026-10-09):** on links and images a `title=` attribute is written when there is no
> Markdown title (`[a](u){title="T"}`); a Markdown title (`[a](u "T")`) wins.

## 4. Directives

`:::name arg arg` … `:::` is a comrak block directive. To nest directives the **outer**
fence must be longer than the inner ones (`::::columns` … `::::` around `:::column` … `:::`):
comrak 0.56 closes the first open directive whose fence is not longer than the closing fence,
so same-length or indented inner fences close the outer directive early (verified by T01,
contract change 2026-10-08). Default rendering by comrak is `<div class="name arg arg">…</div>`, which is
already right for reveal (`fragment`, `r-stack`, `r-fit-text`, …). We special-case:

| directive | rendered as | notes |
|---|---|---|
| `:::notes` | removed from the body; inner content rendered to HTML and appended to `Slide.notes` (multiple blocks concatenate) | reveal needs `<aside class="notes">`, which the template emits |
| `:::fragment [effects…] [index=N]` | `<div class="fragment effects…" [data-fragment-index="N"]>` | comrak already produces the classes; T12 adds `data-fragment-index` from an `index=` arg if present (parse `info` manually) |
| `:::columns [widths="1fr 2fr"]` | `<div class="columns" [style="grid-template-columns: 1fr 2fr"]>` | layout is CSS grid in the default theme |
| `:::column [width=…]` | `<div class="column">` | |
| `:::center`, `:::small`, `:::large`, `:::note`, `:::warning` and any other name | `<div class="name args">` (default) | the default theme styles a documented subset |
| `:::include path` | replaced by the included file's slides (§4.1, T48) | must be the only content of its horizontal slide |

### 4.1 Includes: composing a deck from several files (T48, contract change 2026-10-09)

```markdown
---
title: Rust for data people
---

# Intro {layout=title}

---

:::include analysis.ipynb
:::

---

:::include part2.md
:::
```

- `:::include PATH` followed by a closing `:::` (the body must be empty) pulls in another source
  file: a `.ipynb` notebook or a `.md` deck (by extension). `PATH` is relative to the directory of
  the file that contains the directive (absolute paths work; no URLs). Quote it if it contains
  spaces (`:::include "my talk/part 2.md"`).
- **Boundaries are horizontal only.** The directive must be the only visible content of its
  horizontal slide (between top-level separators, or in headings mode the only content of a slide;
  HTML comments do not count). That slide is replaced, in place, by every top-level slide of the
  included file, each with its own vertical children. Included slides are never merged with
  Markdown content, and an include in a vertical slide (after `--`), inside another directive, a
  list or a quote, or next to other content is a build error (`Error::Parse` at the directive's
  line) — `check` reports it as an error too.
- Slide attributes on the include slide (a `--- {…}` separator or a `<!-- .slide: -->` comment)
  are ignored with a warning `include-attrs-ignored`; `:::notes` on the include slide likewise.
- The included file is parsed as if it were built on its own, with the **including deck's
  effective config**; its own front matter / notebook metadata config tables (`slides:`,
  `markdown:`, `math:`, `notebook:`) still apply to its own parsing. Its deck metadata (`title`,
  `subtitle`, `author`, `date`, `description`, `lang`, `theme`, `reveal`, `vars`) is discarded; a
  warning `include-meta-ignored` names the keys it sets that differ from the including deck's.
- Includes nest (a `.md` may include further files) up to depth 8; a cycle is a build error
  naming the chain. Notebooks cannot include (a `:::include` in a notebook markdown cell is a
  warning `include-in-notebook` and renders nothing). A missing or unreadable file is a build
  error in every mode (like a parse error, not like a missing image).
- Assets, features and diagnostics of included files are merged into the deck (asset ids are
  collision-free across files). Diagnostics keep the included file's path; slides from an
  included file record it in `SourceSpan.file` (ir.md §1), and `Deck.includes` lists every
  included file (transitively, in first-use order) so `serve`/`build --watch` watch them.
- `Slide.title`, ids, layouts etc. are whatever the included file produced. Duplicate slide ids
  across files are reported by `check` like any duplicate.

Directive `info` is split on whitespace; the first token is the name, remaining tokens are
classes unless they contain `=`, in which case they are `key=value` args. Args that are
not consumed by the table above are emitted as `data-<key>="<value>"`.

Directive opening lines may not contain `:` after the fence (comrak 0.56 then does not open a
directive; T12 warns `invalid-directive`); write `&#58;` instead. comrak resolves backslash
escapes in the info string, so a value containing `"` must be single-quoted.

> **A3 note (2026-10-09):** pandoc's attribute block is accepted as the info string:
> `::: {.fragment .fade-up}` is `:::fragment fade-up`, `:::: {.columns}` is `::::columns`,
> `::: {.column width="40%"}` is `:::column width=40%`, `::: {#intro .note}` adds
> `id="intro"`, and `::: {.notes}` is `:::notes` (the first class is the name). `index=N` of
> `:::fragment` must be a non-negative integer (reveal.js never shows a fragment with another
> index); any other value is dropped with an `invalid-directive-arg` warning. A directive opened
> by a bare `:::` line (mostly a nested closing fence as long as the outer one's, as pandoc
> allows) is an `invalid-directive` warning that names the longer-outer-fence fix: the empty
> `<div>` runs to the next `:::` line or to the end of the document, and `---` lines inside it
> start no slides. A `:::name …` or `::: {.name …}` line comrak does not open (a `:` after the
> fence, also pandoc's closing colons `::: {.notes} :::`) is one `invalid-directive` warning
> naming the fix (`&#58;`, or no closing colons); the bare `:::` that closes it is not reported
> again.

## 5. Code blocks

```` ```python {lines="1-2|4" .numbered start=10 trim=false escape=false #snippet .big} ````

Emitted (via `SyntaxHighlighterAdapter` or an AST pass; T12 decides, see the task file):

```html
<pre id="snippet" class="big"><code class="language-python" data-line-numbers="1-2|4" data-ln-start-from="10" data-noescape>…unescaped source…</code></pre>
```

| info / attribute | effect |
|---|---|
| first word of info | `class="language-<word>"` on `<code>`; none → no class (highlight.js auto-detects) |
| `lines="SPEC"` | `data-line-numbers="SPEC"` (reveal syntax, `|` separates steps) |
| `.numbered` | `data-line-numbers=""` (all lines numbered, no steps) unless `lines=` given |
| `start=N` | `data-ln-start-from="N"` |
| `trim=false` | omit `data-trim` (default: present) |
| `escape=false` | add `data-noescape` and emit source **unescaped** (author uses `<mark>` etc.) |
| `.plain` | `class="nohighlight"` on `<code>` (disables highlighting) |
| `#id`, other `.classes` | on `<pre>` |
| other `key=value` | `data-key="value"` on `<code>` |

Source is HTML-escaped (`html_escape::encode_text`) unless `escape=false`. Trailing
newline of the fence content is removed. `Features.code` becomes true.

```` ```math ```` fences (comrak `math_code`) render as `<div data-math-style="display">…</div>`
(comrak itself emits `<pre><code class="language-math" data-math-style="display">`, so T12
rewrites the node; comrak's `format_html` also does not emit node attrs for headings, links,
images or inline code, so the §3.3 AST pass is required)
and set `Features.math`.

## 6. Images and local assets

- `![alt](path "title"){width=50%}` → comrak `Image` node. If `path` is relative (no scheme,
  not starting with `/`), it is resolved against `SourceInfo.base_dir`. If the file exists:
  `assets.register_file(abs)` and the `src` becomes `asset:<id>`. If it does not exist:
  record a `MissingAsset` diagnostic; in `build` this is an error, in `serve` a warning
  (the URL is left as written so the browser shows a broken image), in `check` a lint.
  This is implemented with comrak's `extension.image_url_rewriter` (an `Arc<dyn URLRewriter>`
  with access to the registry through interior mutability) or an AST pass — T12 decides.
- The same applies to `data-background-image`, `data-background-video` and
  `data-background-iframe` slide attributes with relative values.
- `<img src="…">` inside raw HTML blocks is also rewritten when relative (simple regex over
  `HtmlBlock`/`HtmlInline` literals for `src="…"`, `poster="…"`).
- Links (`[text](file.pdf)`) are **not** registered; they stay as written.
- Remote URLs are never fetched.

> **A3 note (2026-10-09):** like theme files, local files must match the letter case of the
> reference exactly. A file found only by letter case (`plain.PNG` for `plain.png` on a
> case-insensitive file system such as macOS's) is still used and is an `asset-case-mismatch`
> warning, since a case-sensitive file system (Linux CI) would not find it.

## 7. Speaker notes
- `:::notes … :::` anywhere in a slide (§4). Multiple blocks append in order.
- Compat (T43): a top-level `Paragraph` starting with `Note:` or `Notes:` ends the slide
  body; it and everything after it become notes (the reveal Markdown plugin convention).
- `Features.notes` becomes true when any slide has notes.

## 8. Math
- `$…$` inline and `$$…$$` display via comrak `math_dollars`; output is
  `<span data-math-style="inline|display">TeX</span>` with TeX HTML-escaped. KaTeX
  renders these on load (`assets/presenter/math.js`, T15). `Features.math` becomes true.
- A literal dollar sign is written `\$`.

> **A3 note (2026-10-09):** `Slide.title` keeps inline math as its TeX without the dollars
> (`# The $E=mc^2$ law` → `The E=mc^2 law`).

## 9. Raw HTML and reveal features
Because `render.r#unsafe = true`, authors can write `<video>`, `<iframe>`, `<div class="r-stack">`,
`<span class="fragment highlight-red">`, `<!-- comments -->` directly. HTML comments other
than the reveal-compat forms are passed through unchanged.

## 10. What `check` reports for Markdown (T34)
- raw `---`/`--` lines that did not become separators (setext or indentation pitfalls);
- empty slides; separator with attributes but unknown keys; unknown `layout=` (not present in
  the theme chain's `layouts/`); missing images; duplicate slide ids; and the same lints for every
  included file (§4.1). (`:::notes` at any
  depth, including inside fragments, is extracted by T12, so no lint is needed for it.)

> **A3 note (2026-10-09):** the Markdown frontend's own warnings, which `check` reports with the
> lints above: `unparsed-attr-block` (§3.1), `unlifted-slide-attr` (§3.2), `asset-case-mismatch`
> (§6), `front-matter-empty` and `front-matter-unclosed` (§2.1), `invalid-directive` for a bare
> `:::` and for a directive line comrak does not open, and `invalid-directive-arg` for a bad
> `:::fragment index=` (§4).

## 11. Fixtures (`tests/fixtures/md/`, created by T01, filled in by T11/T12)
`minimal.md` (one slide), `separators.md` (horizontal, vertical, consecutive, inside code
fence), `attributes.md` (separator attrs, heading lifting, shorthand expansion),
`directives.md` (notes, fragment with index, columns, custom), `code.md` (every table row of
§5), `math.md`, `images.md` (existing + missing + remote + raw HTML img), `notes.md`,
`headings-mode.md` (T43), `reveal-compat.md` (T43); `images/` holds the images that
`images.md` finds. Each fixture's IR (the `Deck` as JSON, machine-dependent paths and asset
ids normalized) is an `insta` snapshot, `tests/snapshots/markdown_snapshots__<name>.snap`,
written by `tests/markdown_snapshots.rs`, which snapshots every `*.md` in the directory.
