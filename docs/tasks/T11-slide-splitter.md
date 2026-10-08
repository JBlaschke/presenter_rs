# T11 — Slide splitter and attribute parsing

| | |
|---|---|
| Milestone | M1 |
| Size | M |
| Depends on | T01 |
| Parallel-safe with | T02, T10, T13, T14, T30 |
| Files you own | `src/markdown/split.rs`, `src/markdown/attrs.rs`, `tests/fixtures/md/separators.md`, `tests/fixtures/md/attributes.md` (may edit) |

## Goal
Given a comrak AST, decide where slides start, which slides are vertical children, and
which attributes apply to each slide — without rendering anything.

## Read first
- `docs/spec/markdown-dialect.md` §1 (comrak options), §2.2, §3
- `docs/spec/ir.md` (Slide fields, SourceSpan)

## API to implement
```rust
// src/markdown/attrs.rs
pub struct ParsedAttrs { pub id: Option<String>, pub classes: Vec<String>, pub pairs: Vec<(String, String)> }
/// Parses `{#id .cls key=value key2="v 2" bare}` (braces optional). Errors on unbalanced quotes.
pub fn parse_attr_block(s: &str) -> Result<ParsedAttrs>;
/// Convert comrak's `comrak::nodes::Attributes` to ParsedAttrs.
impl From<&comrak::nodes::Attributes> for ParsedAttrs { … }
/// Slide-level normalization (dialect §3.2): consumes `layout`, expands shorthands.
pub struct SlideAttrs { pub id: Option<String>, pub layout: Option<String>, pub classes: Vec<String>, pub attrs: BTreeMap<String, String>, pub unknown_keys: Vec<String> }
impl SlideAttrs { pub fn from_parsed(p: ParsedAttrs) -> SlideAttrs; pub fn merge(&mut self, later: SlideAttrs); pub fn apply_to(self, slide: &mut Slide); }
pub fn normalize_key(key: &str, value: &str) -> (String /* emitted key */, bool /* known */);

// src/markdown/split.rs
pub enum SplitMode { Rules, Headings { slide_level: u8 } }   // Headings implemented in T43; return Error::Other("headings mode: T43") for now
pub struct RawSlide<'a> { pub attrs: SlideAttrs, pub nodes: Vec<&'a AstNode<'a>>, pub start_line: usize, pub end_line: usize }
pub struct RawDeck<'a> { pub slides: Vec<(RawSlide<'a>, Vec<RawSlide<'a>>)>, pub diagnostics: Diagnostics }
pub fn split<'a>(root: &'a AstNode<'a>, mode: SplitMode) -> Result<RawDeck<'a>>;
/// If `nodes[0]` is a Heading with attributes, remove them from the node and return them.
pub fn lift_heading_attrs<'a>(nodes: &[&'a AstNode<'a>]) -> Option<ParsedAttrs>;
/// Plain text of the first heading among `nodes` (concatenate Text/Code descendants).
pub fn first_heading_text<'a>(nodes: &[&'a AstNode<'a>]) -> Option<String>;
```

## Steps
1. Implement `parse_attr_block` as a small hand-written tokenizer (no regex): skip `{`/`}`,
   split on whitespace outside quotes, classify `#id`, `.class`, `k=v`, `k="v"`, `k='v'`, bare `k`
   (→ `(k, "")`). Share a table-driven test with cases that comrak also parses (headings) so
   both agree.
2. `split`: iterate `root.children()`. Skip `FrontMatter`. Recognize separators per dialect
   §2.2 (ThematicBreak; Paragraph whose only child is `Text("--")`; Paragraph whose text
   matches the attribute-separator regex). Collect nodes into the current `RawSlide`;
   maintain `start_line/end_line` from `node.data().sourcepos`. Vertical: push into the
   children vec of the last horizontal slide. Emit diagnostics for empty slides (code
   `empty-slide`) and vertical-without-horizontal (treat as horizontal, code `orphan-vertical`).
3. After collecting, for each slide: `lift_heading_attrs` on its nodes and
   `SlideAttrs::merge` (heading attrs win over separator attrs for the same key; classes union).
   Clear the heading node's attrs (`node.data_mut().attrs = None`) when lifted.
4. For every key in `SlideAttrs.unknown_keys` push a `Diagnostic { level: Warning, code: "unknown-slide-attr", span: slide span }`
   into `RawDeck.diagnostics`; the attribute is still emitted verbatim. (`check` only prints these.)

## Acceptance criteria
- Fixture `separators.md` (horizontal, vertical, two consecutive `---`, `---` inside a code
  fence, `--` inside a list item, `***`) yields the expected slide/children counts and
  spans (write the test as a table over `(fixture, expected shape)`).
- `attributes.md`: `--- {.dark background=#111 layout=title}` → classes `["dark"]`,
  attrs `{"data-background-color": "#111"}`, layout `title`; `background=bg.png` →
  `data-background-image`; `{auto-animate}` → `("data-auto-animate", "")`; `# T {#intro .big k=v}`
  → id `intro`, class `big`, attrs `{"k": "v"}` + `unknown_keys == ["k"]`, and the heading node's
  attrs are `None` afterwards.
- `first_heading_text` returns `"Hello *world*"` → `"Hello world"` (emphasis stripped), code spans kept.
- Setext case `Text\n--` produces no separator (documented in a test with a comment).
- Line numbers are 1-based and match `sourcepos`.

## Out of scope
Rendering HTML (T12), headings mode and reveal-compat comments (T43).

## Hints (verified)
- comrak 0.56: `node.data()` returns `Ref<Ast>` with `value: NodeValue`, `sourcepos: Sourcepos { start: LineColumn { line, column }, end }`, `attrs: Option<Attributes>`; `Attributes { id: Option<String>, classes: Vec<String>, pairs: Vec<(String, String)> }`.
- `NodeValue::ThematicBreak`, `NodeValue::Paragraph`, `NodeValue::Text(String)`, `NodeValue::Heading(NodeHeading { level, setext })`, `NodeValue::FrontMatter(String)`.
- Use `comrak::parse_document(&arena, text, &options)` with the options from dialect §1; `smart` must be `false` or `--` becomes an en dash.
