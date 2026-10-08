# T30 — nbformat 4 data model and notebook fixtures

| | |
|---|---|
| Milestone | M3 |
| Size | M |
| Depends on | T01 |
| Parallel-safe with | T02, T10–T15 |
| Files you own | `src/notebook/nbformat.rs`, `tests/fixtures/ipynb/*.ipynb`, `tests/nbformat.rs` |

## Goal
Deserialize real-world `.ipynb` files leniently into typed structs, with accessors for the
slideshow metadata, tags and language; plus the fixture notebooks every M3 task tests against.

## Read first
- `docs/spec/notebook-mapping.md` §1, §3, §7

## Steps
1. Types per spec §1. `MultilineString`: custom `Deserialize` accepting a string or an
   array of strings (concatenate as-is). `MimeBundle = BTreeMap<String, Value>` with a helper
   `pub fn mime_text(v: &Value) -> Option<String>` (string or array-of-strings → joined).
2. Outputs: deserialize `outputs` as `Vec<Value>` first, then `serde_json::from_value::<Output>`
   each; collect failures as warnings (return `(Vec<Output>, Vec<String>)`) so one odd output
   does not fail the deck.
3. Accessors on `Cell`: `slide_type()`, `tags() -> Vec<String>`, `is_hidden_source()`,
   `is_hidden_outputs()`, `raw_mimetype()`, `presenter_overrides() -> Option<&Value>`.
   On `Notebook`: `language()`, `title()`, `presenter_meta() -> Option<&Value>`.
4. Fixtures (hand-written JSON, small, valid nbformat 4.5 with `id` fields):
   `minimal.ipynb`, `slideshow.ipynb` (spec §6 example exactly), `outputs.ipynb` (every §4
   row: `text/html` table, `image/png` 1×1 base64, `image/svg+xml`, `text/latex` `$$…$$`,
   `text/markdown`, `application/json`, `text/plain`, stream stdout split into two consecutive
   outputs, stream stderr with ANSI color codes, an `error` with ANSI traceback, an
   unsupported-only widget output), `tags.ipynb`, `frontmatter.ipynb`.
   Validate them with `python3 -c "import json,sys; json.load(open(sys.argv[1]))"` at least;
   if `nbformat` is installable, also `nbformat.validate` and note the result.

## Acceptance criteria
- Every fixture deserializes; `slideshow.ipynb` cells report the expected `SlideType`s in order.
- `source` given as a list and as a string produce identical `MultilineString`.
- An output with `"output_type": "weird"` is skipped with a warning; the rest of the cell parses.
- `nbformat: 3` → `Error::Parse` mentioning nbformat 4.
- Unknown top-level/cell/metadata keys do not fail parsing.
- `language()` precedence test: `language_info.name` wins over `kernelspec.language`; default `"text"`.

## Out of scope
Any IR production (T31), output HTML (T32).

## Hints
- Use `#[serde(default)]` liberally; nbformat allows `metadata: {}` and omitted `outputs` on markdown cells.
- Cell `id` exists from nbformat 4.5; keep `Option<String>`.
