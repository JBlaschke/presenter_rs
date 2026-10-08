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
- Values that are not strings for string fields (e.g. `date: 2026-10-08` parses as a
  date/plain scalar) are converted with their YAML scalar text; `title: 42` → `"42"`.

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

## 3. Attributes

### 3.1 Syntax
Pandoc-style attribute blocks `{#id .class1 .class2 key=value key2="quoted value"}`.
Values may be single- or double-quoted; unquoted values run to the next whitespace.
comrak parses these natively for headings, fenced code, links and images into
`node.data().attrs: Option<Attributes { id, classes, pairs }>`. The separator form
(`--- {…}`) is parsed by our own `markdown::attrs::parse_attr_block(&str)` with the same
grammar (T11), and the two must agree (shared unit tests).

### 3.2 Slide attributes: where they come from and how they normalize
Sources, applied in this order (later wins for the same key; classes accumulate):
1. the separator line `--- {…}` / `-- {…}` that opened the slide,
2. a `<!-- .slide: … -->` HTML comment as the slide's first node (T43),
3. attributes on the slide's **first** node when it is a heading: `# Title {layout=title .x}`.
   They are *lifted*: removed from the heading node (so the `<h1>` is clean) and applied to
   the slide. The heading's `#id` becomes the section id.

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
| `:::include path` | reserved; not implemented in this plan | `check` warns "unknown directive include" |

Directive `info` is split on whitespace; the first token is the name, remaining tokens are
classes unless they contain `=`, in which case they are `key=value` args. Args that are
not consumed by the table above are emitted as `data-<key>="<value>"`.

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

## 9. Raw HTML and reveal features
Because `render.r#unsafe = true`, authors can write `<video>`, `<iframe>`, `<div class="r-stack">`,
`<span class="fragment highlight-red">`, `<!-- comments -->` directly. HTML comments other
than the reveal-compat forms are passed through unchanged.

## 10. What `check` reports for Markdown (T34)
- raw `---`/`--` lines that did not become separators (setext or indentation pitfalls);
- empty slides; separator with attributes but unknown keys; unknown `layout=` (not present in
  the theme chain's `layouts/`); missing images; `:::include`; `:::notes` nested inside a
  fragment (notes would be lost); duplicate slide ids.

## 11. Fixtures (`tests/fixtures/md/`, created by T01, filled in by T11/T12)
`minimal.md` (one slide), `separators.md` (horizontal, vertical, consecutive, inside code
fence), `attributes.md` (separator attrs, heading lifting, shorthand expansion),
`directives.md` (notes, fragment with index, columns, custom), `code.md` (every table row of
§5), `math.md`, `images.md` (existing + missing + remote + raw HTML img), `notes.md`,
`headings-mode.md` (T43), `reveal-compat.md` (T43). Each has a sibling
`.expected.json` (IR) snapshot managed by `insta`.
