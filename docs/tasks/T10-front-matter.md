# T10 — Front matter → DeckMeta and config overrides

| | |
|---|---|
| Milestone | M1 |
| Size | S |
| Depends on | T01 |
| Parallel-safe with | T02, T11, T13, T14, T30 |
| Files you own | `src/markdown/frontmatter.rs` |

## Goal
Turn a YAML front matter block into `DeckMeta` plus per-deck config overrides, usable by
both the Markdown frontend (comrak gives us the block) and the notebook frontend (first
markdown cell).

## Read first
- `docs/spec/markdown-dialect.md` §2.1, `docs/spec/config.md` §3, `docs/spec/ir.md` (DeckMeta)

## API to implement
```rust
/// Text-based extraction for sources comrak does not see (notebook cells).
/// Returns (yaml_without_delimiters, rest_of_text, number_of_lines_consumed) when `text`
/// starts with a line `---` and a closing `---` line exists.
pub fn split_front_matter(text: &str) -> Option<(&str, &str, usize)>;

/// Strip the `---` delimiter lines from comrak's `NodeValue::FrontMatter` literal.
pub fn strip_delimiters(literal: &str) -> &str;

/// Parse YAML into deck metadata. Unknown keys go to `meta.vars`.
pub fn parse_front_matter(yaml: &str) -> Result<DeckMeta>;
```
Then `Config::merge_front_matter(&mut self, &DeckMeta)` (T01 wrote the signature; make
sure it moves the `slides/markdown/math/notebook` objects out of `vars` and applies them
over the config tables, erroring on unknown keys via the same deny_unknown_fields path:
deserialize each object with `serde_json::from_value::<SlidesConfig>` into a temporary
and overlay only the keys present — implement "only keys present" by deserializing into
`Map<String, Value>`, then `toml`/`serde_json` merging into the serialized current table).

## Steps
1. Parse YAML with `serde_norway::from_str::<serde_norway::Value>` (or `serde_json::Value`
   via `serde_norway::from_str::<serde_json::Value>`, which works for mappings with string
   keys). Non-mapping root → `Error::FrontMatter("expected a mapping")`.
2. Scalars for string fields: accept strings, numbers, booleans and dates by converting
   with their YAML scalar text (`date: 2026-10-08` → `"2026-10-08"`). Non-string `reveal` →
   error "reveal must be a mapping".
3. Keys: `title subtitle author date description lang theme` → fields; `reveal` → map;
   everything else → `vars` (preserve order).

## Acceptance criteria (unit tests in the same file)
- Empty YAML → `DeckMeta::default()`.
- All known keys + two unknown keys → fields set, `vars` has exactly the unknown ones in order.
- `date: 2026-10-08` → `Some("2026-10-08")`; `title: 42` → `Some("42")`.
- `reveal: fade` → error mentioning `reveal`.
- `split_front_matter("---\na: 1\n---\nbody")` → `Some(("a: 1", "body", 3))`;
  text not starting with `---` → `None`; unterminated → `None`.
- CRLF line endings work.
- `merge_front_matter` with `slides: {split: headings}` changes `config.slides.split` and leaves
  the other `slides` keys untouched; `slides: {splt: x}` → error with a hint.

## Out of scope
Reading files; comrak integration (T12); notebook metadata (T31).

## Hints (verified)
- `serde_norway` is the maintained `serde_yaml` fork (0.9.42, Dec 2024); API-compatible:
  `serde_norway::from_str`, `serde_norway::Value`, `Value::as_str/as_i64/...`,
  `Mapping` iteration yields `(Value, Value)`.
- YAML 1.1 quirks: `yes/no/on/off` are booleans in serde_yaml-family parsers; document it.
